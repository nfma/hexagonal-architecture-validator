use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn run(name: &str, format: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_hav"))
        .args([
            "check",
            "--root",
            fixture(name).to_str().unwrap(),
            "--format",
            format,
        ])
        .output()
        .expect("validator should execute")
}

fn run_with_config(name: &str, config: &str, format: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_hav"))
        .args(["check", "--root"])
        .arg(fixture(name))
        .args(["--config", config, "--format", format])
        .output()
        .expect("validator should execute")
}

fn assert_fixture_compiles(name: &str) {
    assert_fixture_source_compiles(name, "src/lib.rs", "2024");
}

fn assert_fixture_source_compiles(name: &str, source: &str, edition: &str) {
    let output_dir = std::env::temp_dir().join("hav-regression-rustc").join(name);
    fs::create_dir_all(&output_dir).unwrap();
    let output = Command::new("rustc")
        .args([
            "--crate-name",
            &name.replace('-', "_"),
            "--crate-type",
            "lib",
            "--edition",
            edition,
        ])
        .arg(fixture(name).join(source))
        .arg("--out-dir")
        .arg(output_dir)
        .output()
        .expect("rustc should execute");
    assert!(
        output.status.success(),
        "fixture {name} must compile: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn json_report(output: &Output) -> serde_json::Value {
    serde_json::from_slice(&output.stdout).expect("stdout should be a JSON report")
}

fn repository_file(path: &str) -> String {
    fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(path))
        .expect("repository file should be readable")
}

#[test]
fn compliant_hexagonal_dependencies_pass() {
    let output = run("compliant", "text");
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        concat!(
            "architecture validation passed: 5 modules, 5 dependencies\n",
            "role coverage: 4 classified module(s), 1 unclassified module(s)\n"
        )
    );
}

#[test]
fn inverse_core_to_adapter_dependency_has_stable_evidence() {
    let output = run("violating", "text");
    assert_eq!(output.status.code(), Some(1));
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("error[core-must-not-depend-on-adapters]"));
    assert!(stdout.contains("src/core.rs:1 (crate::adapter::Console)"));
}

#[test]
fn json_output_matches_the_versioned_golden_file() {
    let first = run("violating", "json");
    let second = run("violating", "json");
    assert_eq!(first.status.code(), Some(1));
    assert_eq!(first.stdout, second.stdout, "JSON must be deterministic");
    assert_eq!(
        String::from_utf8(first.stdout).unwrap(),
        include_str!("golden/violating.json")
    );
}

#[test]
fn inline_modules_are_analyzed_as_distinct_modules() {
    let output = run("inline", "text");
    assert_eq!(output.status.code(), Some(1));
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("::core -> inline-fixture::lib(inline_fixture)::adapter"));
}

#[test]
fn workspace_crate_dependencies_are_resolved() {
    let output = run("workspace", "json");
    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("adapter::lib(adapter)"));
    assert!(stdout.contains("domain::lib(domain)"));
    assert!(stdout.contains("\"dependencies\": 1"));
}

#[test]
fn relative_roots_and_path_modules_are_supported() {
    let root = fixture("relative-path");
    let output = Command::new(env!("CARGO_BIN_EXE_hav"))
        .current_dir(root.join("src"))
        .args(["check", "--root", "..", "--format", "text"])
        .output()
        .expect("validator should execute");
    assert_eq!(output.status.code(), Some(0));
}

#[test]
fn cycles_are_sorted_and_reported_as_violations() {
    let output = run("cycle", "text");
    assert_eq!(output.status.code(), Some(1));
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("error[no-cycles]"));
    assert!(stdout.contains("::a -> cycle-fixture::lib(cycle_fixture)::b"));
}

#[test]
fn unresolved_modules_fail_analysis_not_architecture() {
    let output = run("unresolved", "text");
    assert_eq!(output.status.code(), Some(2));
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("error[unresolved-module]"));
    assert!(!stdout.contains("architecture validation found"));
}

#[test]
fn unsupported_include_macro_fails_analysis() {
    let output = run("unsupported", "text");
    assert_eq!(output.status.code(), Some(2));
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("error[unsupported-include]"));
}

#[test]
fn malformed_config_has_the_analysis_error_exit_code() {
    let output = run("malformed-config", "text");
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stderr.is_empty());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("architecture analysis failed with 1 error(s):"));
    assert!(stdout.contains("role coverage: 0 classified module(s), 0 unclassified module(s)"));
    assert!(stdout.contains("unsupported config version 99; expected 1"));

    let json_output = run("malformed-config", "json");
    assert_eq!(json_output.status.code(), Some(2));
    let report = json_report(&json_output);
    assert_eq!(report["schema_version"], 1);
    assert_eq!(report["outcome"], "analysis-failure");
    assert_eq!(report["summary"]["classified_modules"], 0);
    assert_eq!(report["summary"]["unclassified_modules"], 0);
    assert_eq!(report["summary"]["analysis_errors"], 1);
    assert_eq!(report["modules"], serde_json::json!([]));
    assert_eq!(report["dependencies"], serde_json::json!([]));
    assert_eq!(report["findings"], serde_json::json!([]));
    assert_eq!(report["limitations"], serde_json::json!([]));
    assert_eq!(report["analysis_errors"].as_array().unwrap().len(), 1);
    assert!(report.get("error").is_none());
}

#[test]
fn current_module_scope_imports_and_prelude_names_resolve() {
    assert_fixture_compiles("resolution");
    let output = run("resolution", "json");
    assert_eq!(output.status.code(), Some(0));
    let report = json_report(&output);
    assert_eq!(report["summary"]["analysis_errors"], 0);

    let edges = report["dependencies"]
        .as_array()
        .unwrap()
        .iter()
        .map(|dependency| {
            format!(
                "{} -> {}",
                dependency["source"].as_str().unwrap(),
                dependency["target"].as_str().unwrap()
            )
        })
        .collect::<BTreeSet<_>>();
    let root = "resolution-fixture::lib(resolution_fixture)";
    assert!(edges.contains(&format!("{root} -> {root}::core")));
    assert!(edges.contains(&format!("{root}::outer -> {root}::outer::inner")));
    assert!(!edges.contains(&format!("{root}::outer -> {root}::inner")));
    assert!(edges.contains(&format!("{root}::consumer -> {root}::outer")));
    assert!(edges.contains(&format!("{root}::consumer -> {root}::outer::inner")));
}

#[test]
fn path_attributes_use_the_correct_inline_and_file_module_bases() {
    assert_fixture_compiles("path-context");
    let output = run("path-context", "json");
    assert_eq!(output.status.code(), Some(0));
    let report = json_report(&output);
    let sources = report["modules"]
        .as_array()
        .unwrap()
        .iter()
        .map(|module| module["source"].as_str().unwrap())
        .collect::<BTreeSet<_>>();
    assert!(sources.contains("src/inline/custom/child.rs"));
    assert!(sources.contains("src/custom.rs"));
}

#[test]
fn qualified_include_macros_fail_and_text_keeps_rule_findings() {
    assert_fixture_compiles("qualified-include");
    let output = run("qualified-include", "text");
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stderr.is_empty());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(stdout.matches("error[unsupported-include]").count(), 3);
    assert!(stdout.contains("macro 'include'"));
    assert!(stdout.contains("macro 'std::include'"));
    assert!(stdout.contains("macro 'core::include'"));
    assert!(stdout.contains("error[core-must-not-depend-on-adapter]"));
}

#[test]
fn root_without_a_manifest_does_not_search_ancestor_directories() {
    let output = run("missing-manifest", "json");
    assert_eq!(output.status.code(), Some(2));
    let report = json_report(&output);
    assert_eq!(report["outcome"], "analysis-failure");
    assert_eq!(report["summary"]["modules"], 0);
    assert_eq!(report["summary"]["dependencies"], 0);
    assert_eq!(report["summary"]["violations"], 0);
    assert_eq!(report["summary"]["analysis_errors"], 1);
    assert!(
        report["analysis_errors"][0]["message"]
            .as_str()
            .unwrap()
            .contains("missing-manifest/Cargo.toml")
    );
}

#[test]
fn an_explicit_manifest_may_be_outside_the_analysis_root() {
    let output = Command::new(env!("CARGO_BIN_EXE_hav"))
        .args(["check", "--root"])
        .arg(fixture("missing-manifest"))
        .arg("--manifest-path")
        .arg(fixture("resolution").join("Cargo.toml"))
        .args(["--format", "text"])
        .output()
        .expect("validator should execute");
    assert_eq!(output.status.code(), Some(0));
}

#[test]
fn strict_help_describes_item_position_macros() {
    let output = Command::new(env!("CARGO_BIN_EXE_hav"))
        .args(["check", "--help"])
        .output()
        .expect("validator should execute");
    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("Fail on item-position macros that cannot be expanded"));
    assert!(!stdout.contains("Fail on module-level macros"));
}

#[test]
fn glob_imported_module_names_resolve_to_the_exporting_module() {
    assert_fixture_compiles("glob-import");
    let output = run("glob-import", "json");
    assert_eq!(output.status.code(), Some(0));
    let report = json_report(&output);
    assert_eq!(report["summary"]["analysis_errors"], 0);
    let root = "glob-import-fixture::lib(glob_import_fixture)";
    assert!(
        report["dependencies"]
            .as_array()
            .unwrap()
            .iter()
            .any(|dependency| {
                dependency["source"] == format!("{root}::consumer")
                    && dependency["target"] == format!("{root}::provider::inner")
            })
    );
}

#[test]
fn local_child_modules_win_before_glob_import_fallback() {
    assert_fixture_compiles("glob-shadow");
    let output = run("glob-shadow", "json");
    assert_eq!(output.status.code(), Some(0));
    let report = json_report(&output);
    let root = "glob-shadow-fixture::lib(glob_shadow_fixture)";
    let source = format!("{root}::domain");
    let targets = report["dependencies"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|dependency| dependency["source"] == source)
        .map(|dependency| dependency["target"].as_str().unwrap())
        .collect::<BTreeSet<_>>();
    assert!(targets.contains(format!("{root}::domain::model").as_str()));
    assert!(targets.contains(format!("{root}::adapters").as_str()));
    assert!(!targets.contains(format!("{root}::adapters::model").as_str()));
}

#[test]
fn function_local_use_bindings_resolve_follow_on_imports() {
    assert_fixture_compiles("function-import");
    let output = run("function-import", "json");
    assert_eq!(output.status.code(), Some(0));
    let report = json_report(&output);
    assert_eq!(report["summary"]["analysis_errors"], 0);
    let root = "function-import-fixture::lib(function_import_fixture)";
    assert!(
        report["dependencies"]
            .as_array()
            .unwrap()
            .iter()
            .any(|dependency| {
                dependency["source"] == format!("{root}::consumer")
                    && dependency["target"] == format!("{root}::provider::inner")
            })
    );
}

#[test]
fn edition_2015_top_level_module_wins_over_standard_names() {
    assert_fixture_source_compiles("edition-2015", "src/lib.rs", "2015");
    let output = run("edition-2015", "json");
    assert_eq!(output.status.code(), Some(0));
    let report = json_report(&output);
    assert_eq!(report["summary"]["analysis_errors"], 0);
    assert_eq!(report["summary"]["dependencies"], 1);
    let root = "edition-2015-fixture::lib(edition_2015_fixture)";
    assert!(
        report["dependencies"]
            .as_array()
            .unwrap()
            .iter()
            .any(|dependency| {
                dependency["source"] == format!("{root}::consumer")
                    && dependency["target"] == format!("{root}::write")
            })
    );
}

#[test]
fn standard_crates_win_over_unrelated_top_level_modules() {
    assert_fixture_compiles("standard-resolution");
    let output = run("standard-resolution", "json");
    assert_eq!(output.status.code(), Some(0));
    let report = json_report(&output);
    assert_eq!(report["summary"]["analysis_errors"], 0);
    assert_eq!(report["summary"]["dependencies"], 1);
    let root = "standard-resolution-fixture::lib(standard_resolution_fixture)";
    assert!(
        report["dependencies"]
            .as_array()
            .unwrap()
            .iter()
            .any(|dependency| {
                dependency["source"] == root && dependency["target"] == format!("{root}::core")
            })
    );
    assert!(
        !report["dependencies"]
            .as_array()
            .unwrap()
            .iter()
            .any(|dependency| dependency["source"] == format!("{root}::consumer"))
    );
}

#[test]
fn test_is_a_fallback_prelude_name_not_an_unconditional_standard_crate() {
    assert_fixture_source_compiles("test-local-2015", "src/lib.rs", "2015");
    let local_output = run("test-local-2015", "json");
    assert_eq!(local_output.status.code(), Some(0));
    let local_report = json_report(&local_output);
    let root = "test-local-2015-fixture::lib(test_local_2015_fixture)";
    assert!(
        local_report["dependencies"]
            .as_array()
            .unwrap()
            .iter()
            .any(|dependency| {
                dependency["source"] == format!("{root}::consumer")
                    && dependency["target"] == format!("{root}::test")
            })
    );

    assert_fixture_compiles("test-prelude");
    let fallback_output = run("test-prelude", "json");
    assert_eq!(fallback_output.status.code(), Some(0));
    let fallback_report = json_report(&fallback_output);
    assert_eq!(fallback_report["summary"]["analysis_errors"], 0);
    assert_eq!(fallback_report["summary"]["dependencies"], 0);
}

#[test]
fn role_coverage_rejects_vacuous_pass_but_allows_partial_coverage() {
    assert_fixture_compiles("role-coverage");

    let zero_output = run_with_config("role-coverage", "zero.hav.toml", "json");
    assert_eq!(zero_output.status.code(), Some(2));
    let zero_report = json_report(&zero_output);
    assert_eq!(zero_report["outcome"], "analysis-failure");
    assert_eq!(zero_report["summary"]["classified_modules"], 0);
    assert_eq!(zero_report["summary"]["unclassified_modules"], 3);
    assert!(
        zero_report["analysis_errors"]
            .as_array()
            .unwrap()
            .iter()
            .any(|diagnostic| diagnostic["code"] == "no-classified-modules")
    );

    let zero_text = run_with_config("role-coverage", "zero.hav.toml", "text");
    assert_eq!(zero_text.status.code(), Some(2));
    let zero_stdout = String::from_utf8(zero_text.stdout).unwrap();
    assert!(
        zero_stdout.contains("role coverage: 0 classified module(s), 3 unclassified module(s)")
    );
    assert!(zero_stdout.contains("error[no-classified-modules]"));

    let partial_output = run_with_config("role-coverage", "partial.hav.toml", "json");
    assert_eq!(partial_output.status.code(), Some(0));
    let partial_report = json_report(&partial_output);
    assert_eq!(partial_report["outcome"], "passed");
    assert_eq!(partial_report["summary"]["classified_modules"], 1);
    assert_eq!(partial_report["summary"]["unclassified_modules"], 2);
    assert_eq!(partial_report["summary"]["analysis_errors"], 0);

    let partial_text = run_with_config("role-coverage", "partial.hav.toml", "text");
    assert_eq!(partial_text.status.code(), Some(0));
    assert!(
        String::from_utf8(partial_text.stdout)
            .unwrap()
            .contains("role coverage: 1 classified module(s), 2 unclassified module(s)")
    );
}

#[test]
fn explicit_allowed_rule_suppresses_the_same_forbidden_edge() {
    assert_fixture_compiles("allowed-rule");

    let forbidden_output = run_with_config("allowed-rule", "without-allowed.hav.toml", "json");
    assert_eq!(forbidden_output.status.code(), Some(1));
    let forbidden_report = json_report(&forbidden_output);
    assert_eq!(forbidden_report["summary"]["violations"], 1);
    assert_eq!(
        forbidden_report["findings"][0]["rule_id"],
        "core-must-not-depend-on-adapter"
    );

    let allowed_output = run_with_config("allowed-rule", "with-allowed.hav.toml", "json");
    assert_eq!(allowed_output.status.code(), Some(0));
    let allowed_report = json_report(&allowed_output);
    assert_eq!(allowed_report["summary"]["dependencies"], 1);
    assert_eq!(allowed_report["summary"]["violations"], 0);
    assert_eq!(allowed_report["findings"], serde_json::json!([]));
}

#[test]
fn shipped_example_classifies_workspace_package_paths() {
    assert_fixture_source_compiles("example-workspace", "crates/app/src/lib.rs", "2024");
    let output = Command::new(env!("CARGO_BIN_EXE_hav"))
        .args(["check", "--root"])
        .arg(fixture("example-workspace"))
        .arg("--config")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/hexagonal.hav.toml"))
        .args(["--format", "json"])
        .output()
        .expect("validator should execute");
    assert_eq!(output.status.code(), Some(1));
    let report = json_report(&output);
    assert_eq!(report["outcome"], "violations");
    assert_eq!(report["summary"]["violations"], 1);
    assert!(
        report["findings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|finding| {
                finding["rule_id"] == "core-must-not-depend-on-adapters"
                    && finding["evidence"]["path"] == "crates/app/src/core.rs"
            })
    );
}

#[test]
fn documented_install_commands_fail_closed_on_checksum_errors() {
    let installation = repository_file("docs/INSTALLATION.md");
    assert_eq!(installation.matches("set -euo pipefail").count(), 2);
    for checksum_command in [
        "grep \"  $artifact\\$\" SHA256SUMS | shasum -a 256 -c -",
        "grep \"  $artifact\\$\" SHA256SUMS | sha256sum -c -",
    ] {
        let checksum = installation.find(checksum_command).unwrap();
        let block = installation[..checksum].rfind("```console").unwrap();
        assert!(installation[block..checksum].contains("set -euo pipefail"));
    }
}

#[test]
fn release_notes_are_checked_before_attestation() {
    let release = repository_file(".github/workflows/release.yml");
    let guard = release
        .find("test -f \"docs/release-notes-${GITHUB_REF_NAME}.md\"")
        .unwrap();
    let attestation = release.find("uses: actions/attest@").unwrap();
    assert!(guard < attestation);
}

#[test]
fn untrusted_sonar_pull_requests_fail_closed() {
    let sonar = repository_file(".github/workflows/sonar.yml");
    let untrusted = sonar
        .find("if [ \"$UNTRUSTED_PULL_REQUEST\" = \"true\" ]")
        .unwrap();
    let trusted = sonar[untrusted..].find("else").unwrap() + untrusted;
    let branch = &sonar[untrusted..trusted];
    assert!(branch.contains("::error::"));
    assert!(branch.contains("required secrets are unavailable"));
    assert!(branch.contains("explicit admin bypass"));
    assert!(branch.contains("exit 1"));
}

#[test]
fn documentation_covers_regex_role_overlap_and_workspace_paths() {
    let configuration = repository_file("docs/CONFIGURATION.md");
    assert!(configuration.contains("Role regexes use unanchored matching"));
    assert!(configuration.contains("suppresses every forbidden rule for"));

    let example = repository_file("examples/hexagonal.hav.toml");
    let readme = repository_file("README.md");
    let workspace_prefix = "^(?:crates/[^/]+/)?src/core";
    assert!(example.contains(workspace_prefix));
    assert!(readme.contains(workspace_prefix));
}

#[test]
fn documentation_limits_where_analysis_limitations_are_reported() {
    let readme = repository_file("README.md");
    let analysis = repository_file("docs/ANALYSIS.md");
    assert!(readme.contains("Text reports do not render"));
    assert!(readme.contains("these limitations"));
    assert!(analysis.contains("Reports that"));
    assert!(analysis.contains("reach evaluation include this list in JSON output"));
    assert!(!readme.contains("All reports list the remaining"));
    assert!(!analysis.contains("listed in every JSON report"));

    let text_output = run("compliant", "text");
    let stdout = String::from_utf8(text_output.stdout).unwrap();
    assert!(!stdout.contains("cfg predicates are not evaluated"));
}
