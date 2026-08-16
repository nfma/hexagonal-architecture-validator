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

fn assert_semgrep_gate_is_fail_closed(workflow: &str) {
    let tests = named_step(workflow, "Test Semgrep report checker");
    assert!(
        tests.contains("python3 -m unittest tests/test_check_semgrep.py"),
        "Semgrep checker regressions are not executed"
    );

    let scan = named_step(workflow, "Run Semgrep community security rules");
    for required in [
        "semgrep==1.173.0 semgrep scan \\",
        "--config p/default \\",
        "--config p/security-audit \\",
        "--error \\",
        "--metrics=off \\",
        "--exclude target \\",
        "--json \\",
        "--output \"$report\" \\",
        "python3 scripts/check_semgrep.py",
        "--report \"$report\"",
        "--baseline .semgrep-baseline.json",
        "--repository-root .",
    ] {
        assert!(
            scan.contains(required),
            "Semgrep gate is missing {required}"
        );
    }
    for fail_open in ["continue-on-error:", "|| true", "set +e", "set +o errexit"] {
        assert!(
            !scan.contains(fail_open),
            "Semgrep gate contains fail-open wiring: {fail_open}"
        );
    }
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
        security.replace("--error \\\n", ""),
        security.replace("semgrep scan \\\n", "semgrep scan\n"),
        security.replace(
            "      - name: Run Semgrep community security rules\n        run: |",
            "      - name: Run Semgrep community security rules\n        continue-on-error: true\n        run: |",
        ),
        security.replace("--repository-root .", "--repository-root . || true"),
        security.replace(
            "python3 -m unittest tests/test_check_semgrep.py",
            "python3 -m unittest",
        ),
    ];

    for mutant in mutants {
        assert!(std::panic::catch_unwind(|| assert_semgrep_gate_is_fail_closed(&mutant)).is_err());
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
