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
        tests
            .lines()
            .any(|line| line == "        run: python3 -m unittest tests/test_check_semgrep.py"),
        "Semgrep checker regressions are not executed"
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
            "report=\"$RUNNER_TEMP/semgrep.json\"",
            "uvx --no-build --from semgrep==1.173.0 semgrep scan \\",
            "--config p/default \\",
            "--config p/security-audit \\",
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

#[test]
fn every_external_action_is_pinned_to_a_full_commit_sha() {
    let checked = workflows()
        .iter()
        .map(|(name, workflow)| assert_external_actions_are_pinned(name, workflow))
        .sum::<usize>();

    assert!(checked > 0, "no external action references were checked");
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
                "        run: |\n          report=\"$RUNNER_TEMP/semgrep.json\"",
                "        run: |\n          exit 0\n          report=\"$RUNNER_TEMP/semgrep.json\"",
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
                "python3 -m unittest tests/test_check_semgrep.py",
                "python3 -m unittest",
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
fn security_workflows_keep_all_expected_gates() {
    let root = repository_root();
    let security = fs::read_to_string(root.join(".github/workflows/security.yml"))
        .expect("read security workflow");
    let dependency = fs::read_to_string(root.join(".github/workflows/dependency-audit.yml"))
        .expect("read dependency workflow");
    let codeql = fs::read_to_string(root.join(".github/workflows/codeql.yml"))
        .expect("read CodeQL workflow");

    assert!(security.contains("semgrep==1.173.0"));
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
