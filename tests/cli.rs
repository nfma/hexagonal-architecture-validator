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

#[test]
fn compliant_hexagonal_dependencies_pass() {
    let output = run("compliant", "text");
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "architecture validation passed: 5 modules, 5 dependencies\n"
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
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("unsupported config version 99; expected 1"));

    let json_output = run("malformed-config", "json");
    assert_eq!(json_output.status.code(), Some(2));
    let stdout = String::from_utf8(json_output.stdout).unwrap();
    assert!(stdout.contains("\"schema_version\": 1"));
    assert!(stdout.contains("\"outcome\": \"configuration-or-analysis-failure\""));
}
