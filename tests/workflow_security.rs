use std::{fs, path::PathBuf};

fn repository_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn workflows() -> Vec<(String, String)> {
    let directory = repository_root().join(".github/workflows");
    let mut workflows = fs::read_dir(directory)
        .expect("read workflow directory")
        .filter_map(|entry| {
            let path = entry.expect("read workflow entry").path();
            let extension = path.extension()?.to_str()?;
            if extension != "yml" && extension != "yaml" {
                return None;
            }
            let name = path
                .file_name()
                .expect("workflow filename")
                .to_string_lossy()
                .into_owned();
            let content = fs::read_to_string(path).expect("read workflow");
            Some((name, content))
        })
        .collect::<Vec<_>>();
    workflows.sort_by(|left, right| left.0.cmp(&right.0));
    workflows
}

fn action_reference(line: &str) -> Option<&str> {
    line.trim()
        .strip_prefix("- ")
        .unwrap_or(line.trim())
        .strip_prefix("uses: ")
        .and_then(|value| value.split_whitespace().next())
}

fn assert_external_actions_are_pinned(name: &str, workflow: &str) -> usize {
    let mut checked = 0;
    for reference in workflow.lines().filter_map(action_reference) {
        if reference.starts_with("./") {
            continue;
        }
        checked += 1;
        let (_, revision) = reference
            .rsplit_once('@')
            .unwrap_or_else(|| panic!("unversioned action in {name}: {reference}"));
        assert_eq!(revision.len(), 40, "non-SHA action in {name}: {reference}");
        assert!(
            revision
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
            "non-lowercase SHA action in {name}: {reference}"
        );
    }
    checked
}

fn assert_checkout_credentials_are_disabled(name: &str, workflow: &str) -> usize {
    let mut checked = 0;
    let mut steps = Vec::new();
    let mut current_step = String::new();
    for line in workflow.lines() {
        if line.trim_start().starts_with("- ") && !current_step.is_empty() {
            steps.push(current_step);
            current_step = String::new();
        }
        if !current_step.is_empty() || line.trim_start().starts_with("- ") {
            current_step.push_str(line);
            current_step.push('\n');
        }
    }
    if !current_step.is_empty() {
        steps.push(current_step);
    }

    for step in steps {
        if step.contains("uses: actions/checkout@") {
            checked += 1;
            assert!(
                step.lines()
                    .map(|line| line.split('#').next().unwrap_or("").trim())
                    .any(|line| line == "persist-credentials: false"),
                "checkout persists credentials in {name}"
            );
        }
    }
    checked
}

fn named_step<'a>(workflow: &'a str, name: &str) -> &'a str {
    let marker = format!("      - name: {name}\n");
    let start = workflow
        .find(&marker)
        .unwrap_or_else(|| panic!("missing workflow step: {name}"));
    let tail = &workflow[start..];
    let end = tail[marker.len()..]
        .find("\n      - name: ")
        .map(|offset| marker.len() + offset)
        .unwrap_or(tail.len());
    &tail[..end]
}

fn named_job<'a>(workflow: &'a str, name: &str) -> &'a str {
    let marker = format!("  {name}:\n");
    let start = workflow
        .find(&marker)
        .unwrap_or_else(|| panic!("missing workflow job: {name}"));
    let tail = &workflow[start..];
    let mut end = tail.len();
    let mut offset = marker.len();
    for line in tail[marker.len()..].split_inclusive('\n') {
        if line.starts_with("  ") && !line.starts_with("   ") {
            end = offset;
            break;
        }
        offset += line.len();
    }
    &tail[..end]
}

fn assert_unconditional(label: &str, yaml: &str, indentation: &str) {
    for key in ["continue-on-error:", "if:"] {
        assert!(
            !yaml
                .lines()
                .any(|line| line.starts_with(indentation)
                    && line[indentation.len()..].starts_with(key)),
            "{label} must not declare {key}"
        );
    }
}

fn assert_pull_request_rechecks_ready_drafts(name: &str, workflow: &str) {
    let marker = "  pull_request:\n";
    let tail = workflow
        .split_once(marker)
        .map(|(_, tail)| tail)
        .unwrap_or_else(|| panic!("{name} must run for pull requests"));
    let mut end = tail.len();
    let mut offset = 0;
    for line in tail.split_inclusive('\n') {
        if line.starts_with("  ") && !line.starts_with("   ") {
            end = offset;
            break;
        }
        offset += line.len();
    }
    let pull_request = &tail[..end];
    assert!(
        pull_request
            .lines()
            .any(|line| line == "    types: [opened, reopened, synchronize, ready_for_review]"),
        "{name} must rerun when a draft update pull request is marked ready"
    );
}

fn assert_semgrep_gate_is_fail_closed(workflow: &str) {
    let job = named_job(workflow, "semgrep-and-secrets");
    assert_unconditional("Semgrep job", job, "    ");

    let tests = named_step(workflow, "Test Semgrep report checker");
    assert_unconditional("Semgrep checker-test step", tests, "        ");
    assert!(
        tests.lines().any(|line| line == "        shell: bash"),
        "Semgrep checker tests must use the fail-fast Bash shell"
    );
    assert!(
        tests.contains(concat!(
            "        run: >-\n",
            "          python3 -m unittest\n",
            "          tests/test_check_semgrep.py\n",
            "          tests/test_semgrep_packs.py\n",
        )),
        "Semgrep checker and pack regressions are not executed"
    );

    let scan = named_step(workflow, "Run Semgrep community security rules");
    assert_unconditional("Semgrep scan step", scan, "        ");
    assert!(
        scan.lines().any(|line| line == "        shell: bash"),
        "Semgrep scan must use the fail-fast Bash shell"
    );
    let run = scan
        .split_once("        run: |\n")
        .map(|(_, run)| run)
        .expect("Semgrep scan must use a shell run block");
    let commands = run
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .collect::<Vec<_>>();
    assert_eq!(
        commands,
        [
            "rules=\".semgrep/rules\"",
            "report=\"$RUNNER_TEMP/semgrep.json\"",
            "mkdir -p \"$rules\"",
            "curl --fail --silent --show-error --proto '=https' --tlsv1.2 \\",
            "--max-time 60 --max-filesize 8388608 \\",
            "--output \"$rules/default.yml\" \\",
            "https://semgrep.dev/c/p/default",
            "curl --fail --silent --show-error --proto '=https' --tlsv1.2 \\",
            "--max-time 60 --max-filesize 8388608 \\",
            "--output \"$rules/security-audit.yml\" \\",
            "https://semgrep.dev/c/p/security-audit",
            "python3 scripts/semgrep_packs.py verify \\",
            "--manifest .semgrep/packs.lock.json \\",
            "--input-dir \"$rules\"",
            "uvx --no-build --from semgrep==1.173.0 semgrep scan \\",
            "--config \"$rules/default.yml\" \\",
            "--config \"$rules/security-audit.yml\" \\",
            "--error \\",
            "--metrics=off \\",
            "--exclude target \\",
            "--json \\",
            "--output \"$report\" \\",
            ".",
            "python3 scripts/check_semgrep.py \\",
            "--report \"$report\" \\",
            "--baseline .semgrep-baseline.json \\",
            "--repository-root .",
        ],
        "Semgrep scan commands must match the reviewed fail-closed sequence"
    );
}

fn assert_semgrep_updater_is_protected(workflow: &str) {
    assert!(
        workflow.starts_with(concat!(
            "name: Update Semgrep rules\n\n",
            "on:\n",
            "  schedule:\n",
            "    - cron: \"17 5 * * 1\"\n",
            "  workflow_dispatch:\n\n",
            "permissions: {}\n",
        )),
        "Semgrep updater must be scheduled/manual with deny-by-default permissions"
    );

    let job = named_job(workflow, "update");
    assert_unconditional("Semgrep updater job", job, "    ");
    assert!(
        job.contains(concat!(
            "    permissions:\n",
            "      contents: write\n",
            "      pull-requests: write\n",
        )),
        "Semgrep updater must scope write permissions to its job"
    );

    let tests = named_step(workflow, "Test rule-pack helper");
    assert_unconditional("rule-pack helper test step", tests, "        ");
    assert!(
        tests.contains("        run: python3 -m unittest tests/test_semgrep_packs.py\n"),
        "Semgrep updater must run rule-pack helper regressions"
    );

    let download = named_step(workflow, "Download current rule packs");
    assert_unconditional("rule-pack download step", download, "        ");
    for required in [
        "mkdir -p \"$rules\"",
        "curl --fail --silent --show-error --proto '=https' --tlsv1.2 \\",
        "--max-time 60 --max-filesize 8388608 \\",
        "--output \"$rules/default.yml\" \\",
        "https://semgrep.dev/c/p/default",
        "--output \"$rules/security-audit.yml\" \\",
        "https://semgrep.dev/c/p/security-audit",
    ] {
        assert!(
            download.lines().any(|line| line.trim() == required),
            "Semgrep updater download is missing: {required}"
        );
    }

    let refresh = named_step(workflow, "Refresh pinned rule-pack hashes");
    assert_unconditional("rule-pack refresh step", refresh, "        ");
    assert!(
        refresh.contains(concat!(
            "        run: >-\n",
            "          python3 scripts/semgrep_packs.py update\n",
            "          --manifest .semgrep/packs.lock.json\n",
            "          --input-dir \"$RUNNER_TEMP/semgrep-rules\"\n",
        )),
        "Semgrep updater must refresh only the reviewed lock manifest"
    );

    let validate = named_step(workflow, "Validate refreshed rule packs");
    assert_unconditional("refreshed rule-pack validation step", validate, "        ");
    for required in [
        "python3 scripts/semgrep_packs.py verify \\",
        "--manifest .semgrep/packs.lock.json \\",
        "--input-dir \"$rules\"",
        "uvx --no-build --from semgrep==1.173.0 semgrep scan \\",
        "--validate \\",
        "--config \"$rules/default.yml\" \\",
        "--config \"$rules/security-audit.yml\"",
    ] {
        assert!(
            validate.lines().any(|line| line.trim() == required),
            "Semgrep updater validation is missing: {required}"
        );
    }

    let create = named_step(workflow, "Create signed protected update pull request");
    assert_unconditional("Semgrep update-PR step", create, "        ");
    for required in [
        "uses: peter-evans/create-pull-request@5f6978faf089d4d20b00c7766989d076bb2fc7f1",
        "add-paths: .semgrep/packs.lock.json",
        "branch: automation/update-semgrep-rules",
        "delete-branch: true",
        "draft: always-true",
        "sign-commits: true",
    ] {
        assert!(
            create
                .lines()
                .any(|line| { line.split('#').next().unwrap_or("").trim() == required }),
            "Semgrep update PR is missing: {required}"
        );
    }

    let verify = named_step(workflow, "Verify signed update commit");
    assert!(
        verify.contains(concat!(
            "        if: >-\n",
            "          ${{\n",
            "            steps.pull-request.outputs.pull-request-operation == 'created' ||\n",
            "            steps.pull-request.outputs.pull-request-operation == 'updated'\n",
            "          }}\n",
        )),
        "Semgrep updater must verify created or updated commits only"
    );
    assert!(
        !verify.contains("continue-on-error:"),
        "signed-commit verification must not continue on error"
    );
    assert!(
        verify
            .lines()
            .any(|line| line.trim() == "run: test \"$COMMITS_VERIFIED\" = \"true\""),
        "Semgrep updater must fail when its commit is not verified"
    );
    assert!(
        !workflow.contains("gh pr merge") && !workflow.contains("--auto"),
        "Semgrep update pull requests must require protected human review"
    );
}

#[test]
fn every_external_action_is_pinned_to_a_full_commit_sha() {
    let checked = workflows()
        .iter()
        .map(|(name, workflow)| assert_external_actions_are_pinned(name, workflow))
        .sum::<usize>();

    assert!(checked > 0, "no external action references were checked");
}

#[test]
fn protected_pull_request_workflows_recheck_ready_drafts() {
    for name in [
        "ci.yml",
        "codeql.yml",
        "dependency-audit.yml",
        "repository-quality.yml",
        "security.yml",
        "sonar.yml",
    ] {
        let workflow = fs::read_to_string(repository_root().join(".github/workflows").join(name))
            .unwrap_or_else(|error| panic!("read {name}: {error}"));
        assert_pull_request_rechecks_ready_drafts(name, &workflow);

        let mutant = workflow.replace(
            "    types: [opened, reopened, synchronize, ready_for_review]",
            "    types: [opened, reopened, synchronize]",
        );
        assert_ne!(mutant, workflow, "{name} mutation must change the workflow");
        assert!(
            std::panic::catch_unwind(|| assert_pull_request_rechecks_ready_drafts(name, &mutant))
                .is_err(),
            "{name} must reject omission of ready_for_review"
        );
    }
}

#[test]
fn action_pin_validator_rejects_an_unpinned_list_step() {
    let fixture = "jobs:\n  test:\n    steps:\n      - uses: owner/action@v1\n";

    assert!(
        std::panic::catch_unwind(|| assert_external_actions_are_pinned("fixture.yaml", fixture))
            .is_err()
    );
}

#[test]
fn every_checkout_disables_persisted_credentials() {
    let checked = workflows()
        .iter()
        .map(|(name, workflow)| assert_checkout_credentials_are_disabled(name, workflow))
        .sum::<usize>();

    assert!(checked > 0, "no checkout steps were checked");
}

#[test]
fn checkout_validator_rejects_spoofed_or_misindented_credentials() {
    let fixtures = [
        (
            "comment-spoof.yml",
            concat!(
                "jobs:\n  test:\n    steps:\n",
                "      - uses: actions/checkout@de0fac2e4500dabe0009e67214ff5f5447ce83dd\n",
                "        # persist-credentials: false\n",
            ),
        ),
        (
            "four-space-indent.yml",
            concat!(
                "jobs:\n  test:\n    steps:\n",
                "    - uses: actions/checkout@de0fac2e4500dabe0009e67214ff5f5447ce83dd\n",
                "    - uses: actions/checkout@de0fac2e4500dabe0009e67214ff5f5447ce83dd\n",
                "      with:\n",
                "        persist-credentials: false\n",
            ),
        ),
    ];

    for (name, fixture) in fixtures {
        assert!(
            std::panic::catch_unwind(|| {
                assert_checkout_credentials_are_disabled(name, fixture)
            })
            .is_err()
        );
    }
}

#[test]
fn semgrep_gate_is_fail_closed_and_baselined() {
    let security = fs::read_to_string(repository_root().join(".github/workflows/security.yml"))
        .expect("read security workflow");

    assert_semgrep_gate_is_fail_closed(&security);
}

#[test]
fn semgrep_gate_validator_rejects_fail_open_mutants() {
    let security = fs::read_to_string(repository_root().join(".github/workflows/security.yml"))
        .expect("read security workflow");
    let mutants = [
        (
            "job continues on error",
            security.replacen(
                "  semgrep-and-secrets:\n",
                "  semgrep-and-secrets:\n    continue-on-error: true\n",
                1,
            ),
        ),
        (
            "job skips pull requests",
            security.replacen(
                "  semgrep-and-secrets:\n",
                "  semgrep-and-secrets:\n    if: github.event_name != 'pull_request'\n",
                1,
            ),
        ),
        (
            "scan step is conditional",
            security.replace(
                "      - name: Run Semgrep community security rules\n        shell: bash",
                "      - name: Run Semgrep community security rules\n        if: ${{ false }}\n        shell: bash",
            ),
        ),
        (
            "scan exits successfully before running",
            security.replace(
                "        run: |\n          rules=\".semgrep/rules\"",
                "        run: |\n          exit 0\n          rules=\".semgrep/rules\"",
            ),
        ),
        (
            "checker failure is suppressed with colon",
            security.replace("--repository-root .", "--repository-root . || :"),
        ),
        (
            "Semgrep error mode removed",
            security.replace("--error \\\n", ""),
        ),
        (
            "Semgrep shell continuation removed",
            security.replace("semgrep scan \\\n", "semgrep scan\n"),
        ),
        (
            "scan step continues on error",
            security.replace(
                "      - name: Run Semgrep community security rules\n        shell: bash",
                "      - name: Run Semgrep community security rules\n        continue-on-error: true\n        shell: bash",
            ),
        ),
        (
            "checker failure is suppressed with true",
            security.replace("--repository-root .", "--repository-root . || true"),
        ),
        (
            "checker tests are not targeted",
            security.replace(
                "          tests/test_check_semgrep.py\n",
                "",
            ),
        ),
        (
            "baseline path is replaced",
            security.replace(
                "--baseline .semgrep-baseline.json",
                "--baseline .semgrep-baseline.json.disabled",
            ),
        ),
        (
            "Semgrep command is commented out",
            security.replace(
                "          uvx --no-build --from semgrep==1.173.0 semgrep scan \\",
                "          # uvx --no-build --from semgrep==1.173.0 semgrep scan \\",
            ),
        ),
        (
            "checker tests continue on error",
            security.replace(
                "      - name: Test Semgrep report checker\n        shell: bash",
                "      - name: Test Semgrep report checker\n        continue-on-error: true\n        shell: bash",
            ),
        ),
        (
            "pack helper tests are removed",
            security.replace("          tests/test_semgrep_packs.py\n", ""),
        ),
        (
            "pack manifest is bypassed",
            security.replace(
                "--manifest .semgrep/packs.lock.json",
                "--manifest /tmp/unreviewed.json",
            ),
        ),
        (
            "pack verification is removed",
            security.replace(
                concat!(
                    "          python3 scripts/semgrep_packs.py verify \\\n",
                    "            --manifest .semgrep/packs.lock.json \\\n",
                    "            --input-dir \"$rules\"\n",
                ),
                "",
            ),
        ),
        (
            "dynamic Registry packs replace pinned files",
            security
                .replace("--config \"$rules/default.yml\"", "--config p/default")
                .replace(
                    "--config \"$rules/security-audit.yml\"",
                    "--config p/security-audit",
                ),
        ),
    ];

    for (name, mutant) in mutants {
        assert_ne!(mutant, security, "{name} mutation must change the workflow");
        assert!(
            std::panic::catch_unwind(|| assert_semgrep_gate_is_fail_closed(&mutant)).is_err(),
            "{name} mutation must be rejected"
        );
    }
}

#[test]
fn semgrep_rule_pack_updater_is_signed_draft_and_fail_closed() {
    let updater =
        fs::read_to_string(repository_root().join(".github/workflows/update-semgrep-rules.yml"))
            .expect("read Semgrep updater workflow");

    assert_semgrep_updater_is_protected(&updater);
}

#[test]
fn semgrep_rule_pack_updater_validator_rejects_unsafe_mutants() {
    let updater =
        fs::read_to_string(repository_root().join(".github/workflows/update-semgrep-rules.yml"))
            .expect("read Semgrep updater workflow");
    let mutants = [
        (
            "update job continues on error",
            updater.replacen(
                "  update:\n",
                "  update:\n    continue-on-error: true\n",
                1,
            ),
        ),
        (
            "refresh step is conditional",
            updater.replace(
                "      - name: Refresh pinned rule-pack hashes\n        shell: bash",
                "      - name: Refresh pinned rule-pack hashes\n        if: ${{ false }}\n        shell: bash",
            ),
        ),
        (
            "refresh failure is suppressed",
            updater.replace(
                "--input-dir \"$RUNNER_TEMP/semgrep-rules\"\n\n      - name: Validate",
                "--input-dir \"$RUNNER_TEMP/semgrep-rules\" || true\n\n      - name: Validate",
            ),
        ),
        (
            "current packs are not downloaded",
            updater.replace("https://semgrep.dev/c/p/default", "https://example.com/default"),
        ),
        (
            "download failure is suppressed",
            updater.replace("https://semgrep.dev/c/p/default", "https://semgrep.dev/c/p/default || true"),
        ),
        (
            "refreshed packs are not verified",
            updater.replace("python3 scripts/semgrep_packs.py verify", "echo verify"),
        ),
        (
            "refreshed packs are not validated",
            updater.replace("--validate \\", ""),
        ),
        (
            "update PR can include arbitrary files",
            updater.replace(
                "add-paths: .semgrep/packs.lock.json",
                "add-paths: .",
            ),
        ),
        (
            "update PR uses a mutable branch",
            updater.replace(
                "branch: automation/update-semgrep-rules",
                "branch: main",
            ),
        ),
        (
            "update PR is not a draft",
            updater.replace("draft: always-true", "draft: false"),
        ),
        (
            "update commit is not signed",
            updater.replace("sign-commits: true", "sign-commits: false"),
        ),
        (
            "signed-commit verification is removed",
            updater.replace(
                "run: test \"$COMMITS_VERIFIED\" = \"true\"",
                "run: echo \"$COMMITS_VERIFIED\"",
            ),
        ),
        (
            "closed pull requests spuriously require commit verification",
            updater.replace(
                concat!(
                    "        if: >-\n",
                    "          ${{\n",
                    "            steps.pull-request.outputs.pull-request-operation == 'created' ||\n",
                    "            steps.pull-request.outputs.pull-request-operation == 'updated'\n",
                    "          }}\n",
                ),
                concat!(
                    "        if: ${{ ",
                    "steps.pull-request.outputs.pull-request-operation != 'none' }}\n",
                ),
            ),
        ),
    ];

    for (name, mutant) in mutants {
        assert_ne!(mutant, updater, "{name} mutation must change the workflow");
        assert!(
            std::panic::catch_unwind(|| assert_semgrep_updater_is_protected(&mutant)).is_err(),
            "{name} mutation must be rejected"
        );
    }
}

#[test]
fn semgrep_update_runbook_is_self_contained_and_explains_check_triggering() {
    let documentation =
        fs::read_to_string(repository_root().join("docs/SEMGREP.md")).expect("read Semgrep docs");
    let local_update = documentation
        .split_once("To prepare the same update locally:\n")
        .map(|(_, section)| section)
        .expect("Semgrep docs must include a local update runbook");

    for required in [
        "semgrep_update_dir=$(mktemp -d)",
        "semgrep_rules=\"$semgrep_update_dir/rules\"",
        "--output \"$semgrep_rules/default.yml\"",
        "--output \"$semgrep_rules/security-audit.yml\"",
        "python3 scripts/semgrep_packs.py update",
        "python3 scripts/semgrep_packs.py verify",
        "semgrep scan \\",
    ] {
        assert!(
            local_update.contains(required),
            "local Semgrep update runbook is missing: {required}"
        );
    }
    assert!(
        documentation.contains(
            "Pull requests created by `GITHUB_TOKEN` do not trigger other\nworkflow runs."
        ),
        "Semgrep docs must explain GITHUB_TOKEN workflow suppression"
    );
    assert!(
        documentation.contains("`ready_for_review` event and starts the normal protected checks."),
        "Semgrep docs must name the human-triggered check event"
    );
}

#[test]
fn security_workflows_keep_all_expected_gates() {
    let root = repository_root();
    let security = fs::read_to_string(root.join(".github/workflows/security.yml"))
        .expect("read security workflow");
    let dependency = fs::read_to_string(root.join(".github/workflows/dependency-audit.yml"))
        .expect("read dependency workflow");
    let codeql = fs::read_to_string(root.join(".github/workflows/codeql.yml"))
        .expect("read CodeQL workflow");

    assert!(security.contains("semgrep==1.173.0"));
    assert!(security.contains("scripts/semgrep_packs.py verify"));
    assert!(security.contains("--baseline .semgrep-baseline.json"));
    assert!(security.contains("gitleaks git --redact --verbose"));
    assert!(security.contains("version: v0.74.0"));
    assert!(dependency.contains("cargo audit --file Cargo.lock --deny warnings"));
    assert!(dependency.contains("actions/dependency-review-action@"));
    assert!(dependency.contains("fail-on-severity: low"));
    assert!(dependency.contains("environment: dependabot-alerts"));
    assert!(codeql.contains("language: [actions, rust]"));
    assert!(codeql.contains("build-mode: none"));
}
