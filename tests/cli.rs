use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

use hexagonal_architecture_validator::analyzer::{AnalysisOptions, analyze};
use hexagonal_architecture_validator::config::LoadedConfig;
use hexagonal_architecture_validator::evaluate::evaluate;
use hexagonal_architecture_validator::model::Outcome;

static SCRATCH_SEQUENCE: AtomicUsize = AtomicUsize::new(0);

struct ScratchProject {
    root: PathBuf,
}

impl ScratchProject {
    fn new(name: &str) -> Self {
        let sequence = SCRATCH_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("hav-cli-{name}-{}-{sequence}", std::process::id()));
        fs::create_dir_all(&root).expect("scratch project directory should be created");
        Self { root }
    }

    fn write(&self, relative: &str, contents: &str) {
        let path = self.root.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("scratch parent directory should be created");
        }
        fs::write(path, contents).expect("scratch fixture should be written");
    }

    fn basic(name: &str, source: &str, config: &str) -> Self {
        Self::basic_with_edition(name, "2024", source, config)
    }

    fn basic_with_edition(name: &str, edition: &str, source: &str, config: &str) -> Self {
        let project = Self::new(name);
        project.write(
            "Cargo.toml",
            &format!(
                "[package]\nname = \"{name}\"\nversion = \"0.0.0\"\nedition = \"{edition}\"\n"
            ),
        );
        project.write("src/lib.rs", source);
        project.write("hav.toml", config);
        project
    }

    fn run(&self, format: &str) -> Output {
        run_root(&self.root, format)
    }

    fn rustc(&self) -> Output {
        fs::create_dir_all(self.root.join("rustc-out"))
            .expect("rustc output directory should exist");
        Command::new("rustc")
            .current_dir(&self.root)
            .args([
                "--crate-name",
                "fixture",
                "--crate-type",
                "lib",
                "--edition",
                "2024",
                "src/lib.rs",
                "--out-dir",
                "rustc-out",
            ])
            .output()
            .expect("rustc should execute")
    }

    fn cargo_check(&self) -> Output {
        Command::new("cargo")
            .current_dir(&self.root)
            .args(["check", "--workspace", "--offline"])
            .output()
            .expect("cargo check should execute")
    }
}

impl Drop for ScratchProject {
    fn drop(&mut self) {
        if self.root.starts_with(std::env::temp_dir()) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
}

fn run_root(root: &Path, format: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_hav"))
        .args([
            "check",
            "--root",
            root.to_str().unwrap(),
            "--format",
            format,
        ])
        .output()
        .expect("validator should execute")
}

fn single_role_config() -> &'static str {
    "version = 1\n\n[[roles]]\nid = \"module\"\npaths = [\"^src/\"]\n"
}

fn core_adapter_config() -> &'static str {
    "version = 1\n\n[[roles]]\nid = \"core\"\nmodules = [\"::core$\"]\n\n[[roles]]\nid = \"adapter\"\nmodules = [\"::adapter$\"]\n\n[[forbidden]]\nid = \"core-no-adapter\"\nfrom = [\"core\"]\nto = [\"adapter\"]\n"
}

fn workspace_shadow_project(name: &str, core_source: &str) -> ScratchProject {
    let project = ScratchProject::new(name);
    project.write(
        "Cargo.toml",
        "[workspace]\nmembers = [\"domain\", \"app\"]\nresolver = \"3\"\n",
    );
    project.write(
        "domain/Cargo.toml",
        "[package]\nname = \"domain\"\nversion = \"0.0.0\"\nedition = \"2024\"\n",
    );
    project.write(
        "domain/src/lib.rs",
        "pub mod adapter { pub struct Leak; }\n",
    );
    project.write(
        "app/Cargo.toml",
        "[package]\nname = \"app\"\nversion = \"0.0.0\"\nedition = \"2024\"\n\n[dependencies]\ndomain = { path = \"../domain\" }\n",
    );
    project.write("app/src/lib.rs", "pub mod core;\n");
    project.write("app/src/core.rs", core_source);
    project.write(
        "hav.toml",
        "version = 1\n\n[[roles]]\nid = \"core\"\nmodules = [\"::core$\"]\n\n[[roles]]\nid = \"adapter\"\nmodules = [\"^domain::lib.*::adapter$\"]\n\n[[forbidden]]\nid = \"core-no-adapter\"\nfrom = [\"core\"]\nto = [\"adapter\"]\n",
    );
    project
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn run(name: &str, format: &str) -> Output {
    run_root(&fixture(name), format)
}

fn run_with_config(name: &str, config: &str, format: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_hav"))
        .args(["check", "--root"])
        .arg(fixture(name))
        .args(["--config", config, "--format", format])
        .output()
        .expect("validator should execute")
}

fn assert_fixture_source_compiles(name: &str, source: &str, edition: &str) {
    let output_dir = std::env::temp_dir()
        .join("hav-reconciliation-rustc")
        .join(name);
    fs::create_dir_all(&output_dir).expect("fixture output directory should be created");
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

fn assert_fixture_compiles(name: &str) {
    assert_fixture_source_compiles(name, "src/lib.rs", "2024");
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
        "architecture validation passed: 6 modules, 5 dependencies\n"
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
    assert!(stdout.contains("unsupported config version 99; expected 1"));

    let json_output = run("malformed-config", "json");
    assert_eq!(json_output.status.code(), Some(2));
    let report = json_report(&json_output);
    assert_eq!(report["schema_version"], 1);
    assert_eq!(report["outcome"], "analysis-failure");
    assert_eq!(report["summary"]["modules"], 0);
    assert_eq!(report["summary"]["dependencies"], 0);
    assert_eq!(report["summary"]["violations"], 0);
    assert_eq!(report["summary"]["analysis_errors"], 1);
    assert_eq!(report["summary"]["exemptions"], 0);
    assert_eq!(report["modules"], serde_json::json!([]));
    assert_eq!(report["dependencies"], serde_json::json!([]));
    assert_eq!(report["findings"], serde_json::json!([]));
    assert_eq!(report["exemptions"], serde_json::json!([]));
    assert_eq!(report["limitations"], serde_json::json!([]));
    assert_eq!(report["analysis_errors"].as_array().unwrap().len(), 1);
    assert!(report.get("error").is_none());
}

#[test]
fn readme_quick_start_configuration_loads() {
    let readme = include_str!("../README.md");
    let config = readme
        .split_once("```toml\n")
        .and_then(|(_, rest)| rest.split_once("```"))
        .map(|(config, _)| config)
        .expect("README must contain a TOML quick-start block");
    let project = ScratchProject::basic("readme-quick-start", "pub fn run() {}\n", config);
    LoadedConfig::load(&project.root.join("hav.toml"))
        .expect("README quick-start configuration must load");
}

#[test]
fn path_module_children_follow_the_path_files_directory() {
    let valid = ScratchProject::basic(
        "path-child-valid",
        "#[path = \"alt/parent.rs\"]\nmod parent;\n",
        single_role_config(),
    );
    valid.write("src/alt/parent.rs", "mod child;\n");
    valid.write("src/alt/child.rs", "pub struct Child;\n");
    assert!(
        valid.rustc().status.success(),
        "fixture must compile with rustc"
    );
    assert_eq!(valid.run("text").status.code(), Some(0));

    let invalid = ScratchProject::basic(
        "path-child-invalid",
        "#[path = \"alt/parent.rs\"]\nmod parent;\n",
        single_role_config(),
    );
    invalid.write("src/alt/parent.rs", "mod child;\n");
    invalid.write("src/alt/parent/child.rs", "pub struct Child;\n");
    assert!(
        !invalid.rustc().status.success(),
        "negative control must not compile"
    );
    let output = invalid.run("text");
    assert_eq!(output.status.code(), Some(2));
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .contains("unresolved-module")
    );
}

#[test]
fn inline_path_modules_follow_the_inline_module_directory() {
    for (name, parent_source, parent_file) in [
        (
            "inline-path-non-mod-rs",
            "pub mod outer { #[path = \"inner.rs\"] pub mod inner; }\n",
            "src/parent.rs",
        ),
        (
            "inline-path-mod-rs",
            "pub mod outer { #[path = \"inner.rs\"] pub mod inner; }\n",
            "src/parent/mod.rs",
        ),
    ] {
        let project = ScratchProject::basic(name, "pub mod parent;\n", single_role_config());
        project.write(parent_file, parent_source);
        project.write("src/parent/outer/inner.rs", "pub struct Inner;\n");
        assert!(
            project.rustc().status.success(),
            "{name} must compile with rustc"
        );
        let output = project.run("json");
        assert_eq!(output.status.code(), Some(0), "{name} must pass hav");
        let report = json_report(&output);
        assert!(
            report["modules"]
                .as_array()
                .unwrap()
                .iter()
                .any(|module| module["id"]
                    .as_str()
                    .is_some_and(|id| id.ends_with("::parent::outer::inner"))),
            "{name} must discover the inline #[path] child"
        );

        let inverse_name = format!("{name}-inverse");
        let inverse =
            ScratchProject::basic(&inverse_name, "pub mod parent;\n", single_role_config());
        inverse.write(parent_file, parent_source);
        inverse.write("src/parent/inner.rs", "pub struct Inner;\n");
        assert!(
            !inverse.rustc().status.success(),
            "{name} inverse layout must not compile with rustc"
        );
        let output = inverse.run("text");
        assert_eq!(output.status.code(), Some(2), "{name} must fail closed");
        assert!(
            String::from_utf8(output.stdout)
                .unwrap()
                .contains("unresolved-module")
        );
    }
}

#[test]
fn local_value_names_do_not_hide_workspace_crates() {
    for (name, source) in [
        (
            "workspace-fn-shadow",
            "pub fn domain() {}\npub fn leak(_: domain::adapter::Leak) {}\n",
        ),
        (
            "workspace-const-shadow",
            "pub const domain: u8 = 1;\npub fn leak(_: domain::adapter::Leak) {}\n",
        ),
        (
            "workspace-use-shadow",
            "pub fn domain() {}\nuse domain::adapter::Leak;\npub fn leak(_: Leak) {}\n",
        ),
    ] {
        let project = workspace_shadow_project(name, source);
        let cargo = project.cargo_check();
        assert!(
            cargo.status.success(),
            "{name} must compile with cargo: {}",
            String::from_utf8_lossy(&cargo.stderr)
        );
        let output = project.run("text");
        assert_eq!(output.status.code(), Some(1), "{name} must violate");
        assert!(
            String::from_utf8(output.stdout)
                .unwrap()
                .contains("error[core-no-adapter]")
        );
    }

    let control = workspace_shadow_project(
        "workspace-no-shadow",
        "pub fn leak(_: domain::adapter::Leak) {}\n",
    );
    assert!(control.cargo_check().status.success());
    assert_eq!(control.run("text").status.code(), Some(1));
}

#[test]
fn leading_colon_uses_extern_prelude_in_modern_editions() {
    for (name, source) in [
        (
            "absolute-path-modern",
            "pub mod domain { pub mod adapter { pub struct Leak { _private: () } } }\npub fn leak() -> ::domain::adapter::Leak { ::domain::adapter::Leak }\n",
        ),
        (
            "absolute-use-modern",
            "pub mod domain { pub mod adapter { pub struct Leak { _private: () } } }\nuse ::domain::adapter::Leak;\npub fn leak() -> Leak { Leak }\n",
        ),
    ] {
        let project = workspace_shadow_project(name, source);
        let cargo = project.cargo_check();
        assert!(
            cargo.status.success(),
            "{name} must bind the workspace crate: {}",
            String::from_utf8_lossy(&cargo.stderr)
        );
        let output = project.run("text");
        assert_eq!(output.status.code(), Some(1), "{name} must violate");
        assert!(
            String::from_utf8(output.stdout)
                .unwrap()
                .contains("error[core-no-adapter]")
        );
    }
}

#[test]
fn leading_colon_is_crate_root_relative_in_edition_2015() {
    let config = "version = 1\n\n[[roles]]\nid = \"core\"\nmodules = ['^absolute-edition-2015::lib\\(absolute_edition_2015\\)::core$']\n\n[[roles]]\nid = \"adapter\"\nmodules = ['^absolute-edition-2015::lib\\(absolute_edition_2015\\)::adapter$']\n\n[[forbidden]]\nid = \"core-no-adapter\"\nfrom = [\"core\"]\nto = [\"adapter\"]\n";
    let project = ScratchProject::basic_with_edition(
        "absolute-edition-2015",
        "2015",
        "pub mod adapter { pub struct Leak; }\npub mod core { pub mod adapter { pub struct Leak { _private: () } } pub fn leak() -> ::adapter::Leak { ::adapter::Leak } }\n",
        config,
    );
    let cargo = project.cargo_check();
    assert!(
        cargo.status.success(),
        "edition 2015 fixture must bind the crate root: {}",
        String::from_utf8_lossy(&cargo.stderr)
    );
    let output = project.run("text");
    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .contains("error[core-no-adapter]")
    );
}

#[test]
fn opaque_reexports_consider_the_intermediate_modules_role() {
    let config = "version = 1\n\n[[roles]]\nid = \"core\"\nmodules = [\"::core(?:$|::)\"]\n\n[[roles]]\nid = \"adapter\"\nmodules = [\"::adapters(?:$|::)\"]\n\n[[forbidden]]\nid = \"core-no-adapter\"\nfrom = [\"core\"]\nto = [\"adapter\"]\n";
    let source = "pub mod adapters { pub mod http { pub use crate::core::model::Order; pub struct Handler; } }\npub mod core { pub mod model { pub struct Order; } pub mod service { use crate::adapters::http::Order; pub fn run(_: Order) {} } }\n";
    let project = ScratchProject::basic("opaque-via-role", source, config);
    assert!(project.rustc().status.success());
    let output = project.run("text");
    assert_eq!(output.status.code(), Some(2));
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .contains("error[opaque-reexport]")
    );

    let control_source = "pub mod adapters { pub mod http { pub use crate::core::model::Order; pub struct Handler; } }\npub mod core { pub mod model { pub struct Order; } pub mod service { use crate::adapters::http::Handler; pub fn run(_: Handler) {} } }\n";
    let control = ScratchProject::basic("opaque-via-control", control_source, config);
    assert!(control.rustc().status.success());
    let output = control.run("text");
    assert_eq!(output.status.code(), Some(1));
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("error[core-no-adapter]"));
    assert!(!stdout.contains("error[opaque-reexport]"));
}

#[test]
fn inline_module_path_attribute_sets_the_child_directory() {
    let valid = ScratchProject::basic(
        "inline-module-path-valid",
        "#[path = \"d\"]\npub mod outer { pub mod inner; }\n",
        single_role_config(),
    );
    valid.write("src/d/inner.rs", "pub struct Inner;\n");
    assert!(
        valid.rustc().status.success(),
        "fixture must compile with rustc"
    );
    assert_eq!(valid.run("text").status.code(), Some(0));

    let invalid = ScratchProject::basic(
        "inline-module-path-invalid",
        "#[path = \"d\"]\npub mod outer { pub mod inner; }\n",
        single_role_config(),
    );
    invalid.write("src/outer/inner.rs", "pub struct Inner;\n");
    assert!(
        !invalid.rustc().status.success(),
        "inverse layout must not compile with rustc"
    );
    let output = invalid.run("text");
    assert_eq!(output.status.code(), Some(2));
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .contains("unresolved-module")
    );
}

#[test]
fn public_reexport_leaf_and_alias_fail_closed_at_role_boundaries() {
    let config = "version = 1\n\n[[roles]]\nid = \"core\"\nmodules = [\"::core$\"]\n\n[[roles]]\nid = \"adapter\"\nmodules = [\"::adapter$\"]\n\n[[forbidden]]\nid = \"core-no-adapter\"\nfrom = [\"core\"]\nto = [\"adapter\"]\n";
    for (name, export, import) in [
        ("reexport-leaf", "Console", "Console"),
        ("reexport-alias", "Console as Driver", "Driver"),
    ] {
        let source = format!(
            "pub mod adapter {{ pub struct Console; }}\npub mod api {{ pub use crate::adapter::{export}; }}\npub mod core {{ use crate::api::{import}; pub fn run(_: {import}) {{}} }}\n"
        );
        let project = ScratchProject::basic(name, &source, config);
        assert!(
            project.rustc().status.success(),
            "fixture must compile with rustc"
        );
        let output = project.run("text");
        assert_eq!(output.status.code(), Some(2));
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert!(stdout.contains("error[opaque-reexport]"));
        assert!(!stdout.contains("error[unresolved-import]"));
    }

    let direct = ScratchProject::basic(
        "reexport-direct-control",
        "pub mod adapter { pub struct Console; }\npub mod core { use crate::adapter::Console; pub fn run(_: Console) {} }\n",
        config,
    );
    assert!(
        direct.rustc().status.success(),
        "control must compile with rustc"
    );
    let output = direct.run("text");
    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .contains("error[core-no-adapter]")
    );
}

#[test]
fn unresolved_crate_paths_do_not_collapse_to_the_crate_root() {
    let project = ScratchProject::basic(
        "unresolved-crate-path",
        "use crate::does_not_exist::Thing;\npub fn run(_: Thing) {}\n",
        single_role_config(),
    );
    let output = project.run("text");
    assert_eq!(output.status.code(), Some(2));
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .contains("unresolved-import")
    );
}

#[test]
fn qualified_and_unqualified_include_macros_are_rejected() {
    for (name, invocation) in [
        ("include-bare", "include!(\"generated.rs\");"),
        ("include-std", "std::include!(\"generated.rs\");"),
        ("include-core", "core::include!(\"generated.rs\");"),
    ] {
        let project = ScratchProject::basic(name, invocation, single_role_config());
        project.write("src/generated.rs", "pub struct Generated;\n");
        assert!(
            project.rustc().status.success(),
            "fixture must compile with rustc"
        );
        let output = project.run("text");
        assert_eq!(output.status.code(), Some(2));
        assert!(
            String::from_utf8(output.stdout)
                .unwrap()
                .contains("unsupported-include")
        );
    }
}

#[test]
fn strict_analysis_defaults_true_and_requires_explicit_opt_out() {
    let source = "macro_rules! generated { () => { pub struct Generated; } }\ngenerated!();\n";
    let strict = ScratchProject::basic("strict-default", source, single_role_config());
    assert!(
        strict.rustc().status.success(),
        "fixture must compile with rustc"
    );
    let strict_output = strict.run("text");
    assert_eq!(strict_output.status.code(), Some(2));
    assert!(
        String::from_utf8(strict_output.stdout)
            .unwrap()
            .contains("unsupported-item-macro")
    );

    let relaxed_config = "version = 1\n\n[analysis]\nstrict = false\n\n[[roles]]\nid = \"module\"\npaths = [\"^src/\"]\n";
    let relaxed = ScratchProject::basic("strict-opt-out", source, relaxed_config);
    assert_eq!(relaxed.run("text").status.code(), Some(0));
}

#[test]
fn macro_definitions_are_allowed_but_item_invocations_remain_fatal() {
    let definition = ScratchProject::basic(
        "macro-definition",
        "macro_rules! square { ($value:expr) => { $value * $value }; }\npub fn square_value(value: i32) -> i32 { square!(value) }\n",
        single_role_config(),
    );
    assert!(
        definition.rustc().status.success(),
        "macro definition fixture must compile with rustc"
    );
    assert_eq!(definition.run("text").status.code(), Some(0));

    let invocation = ScratchProject::basic(
        "macro-item-invocation",
        "macro_rules! declare_item { () => { pub struct Generated; }; }\ndeclare_item!();\n",
        single_role_config(),
    );
    assert!(
        invocation.rustc().status.success(),
        "item invocation fixture must compile with rustc"
    );
    let output = invocation.run("text");
    assert_eq!(output.status.code(), Some(2));
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("unsupported-item-macro"));
    assert!(stdout.contains("macro 'declare_item'"));
    assert!(!stdout.contains("macro 'macro_rules'"));
}

#[test]
fn cfg_module_sources_coalesce_or_fail_with_stable_ambiguity() {
    let same = ScratchProject::basic(
        "cfg-same",
        "#[cfg(any())]\n#[path = \"shared.rs\"]\nmod selected;\n#[cfg(not(any()))]\n#[path = \"shared.rs\"]\nmod selected;\n",
        single_role_config(),
    );
    same.write("src/shared.rs", "pub struct Shared;\n");
    assert!(
        same.rustc().status.success(),
        "fixture must compile with rustc"
    );
    assert_eq!(same.run("text").status.code(), Some(0));

    let different = ScratchProject::basic(
        "cfg-different",
        "#[cfg(any())]\n#[path = \"one.rs\"]\nmod selected;\n#[cfg(not(any()))]\n#[path = \"two.rs\"]\nmod selected;\n",
        single_role_config(),
    );
    different.write("src/one.rs", "pub struct One;\n");
    different.write("src/two.rs", "pub struct Two;\n");
    assert!(
        different.rustc().status.success(),
        "fixture must compile with rustc"
    );
    let output = different.run("text");
    assert_eq!(output.status.code(), Some(2));
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .contains("cfg-ambiguous-module")
    );
}

#[test]
fn repeated_inline_module_bodies_are_all_analyzed() {
    let config = "version = 1\n\n[[roles]]\nid = \"core\"\nmodules = [\"::selected$\"]\n\n[[roles]]\nid = \"adapter\"\nmodules = [\"::adapter$\"]\n\n[[forbidden]]\nid = \"core-no-adapter\"\nfrom = [\"core\"]\nto = [\"adapter\"]\n";
    let violating_body = "use crate::adapter::Driver; pub fn run(_: Driver) {}";
    let compliant_body = "pub fn run() {}";

    for (name, first_body, second_body) in [
        ("cfg-inline-second-violates", compliant_body, violating_body),
        ("cfg-inline-first-violates", violating_body, compliant_body),
    ] {
        let source = format!(
            "pub mod adapter {{ pub struct Driver; }}\n#[cfg(any())]\npub mod selected {{ {first_body} }}\n#[cfg(not(any()))]\npub mod selected {{ {second_body} }}\n"
        );
        let project = ScratchProject::basic(name, &source, config);
        assert!(
            project.rustc().status.success(),
            "{name} must compile with rustc"
        );
        let output = project.run("text");
        assert_eq!(output.status.code(), Some(1), "{name} must violate");
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert_eq!(stdout.matches("error[core-no-adapter]").count(), 1);
    }
}

#[cfg(unix)]
#[test]
fn conditional_module_path_attributes_fail_closed() {
    let project = ScratchProject::basic(
        "conditional-module-path",
        "pub mod adapter { pub struct Driver; }\n#[cfg_attr(unix, path = \"active_core.rs\")]\npub mod core;\n",
        core_adapter_config(),
    );
    project.write(
        "src/active_core.rs",
        "use crate::adapter::Driver; pub fn run(_: Driver) {}\n",
    );
    project.write(
        "src/core.rs",
        "compile_error!(\"rustc must not select the default module file\");\n",
    );
    assert!(
        project.rustc().status.success(),
        "rustc must select the conditional path"
    );
    let output = project.run("text");
    assert_eq!(output.status.code(), Some(2));
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("error[unresolved-module]"));
    assert!(stdout.contains("conditional #[cfg_attr(..., path = ...)]"));
}

#[test]
fn nested_path_attributes_are_relative_to_the_declaring_file() {
    let config = "version = 1\n\n[[roles]]\nid = \"core\"\nmodules = [\"::application::console$\"]\n\n[[roles]]\nid = \"adapter\"\nmodules = [\"::adapter$\"]\n\n[[forbidden]]\nid = \"core-no-adapter\"\nfrom = [\"core\"]\nto = [\"adapter\"]\n";
    let project = ScratchProject::basic(
        "nested-path-base",
        "pub mod adapter { pub struct Driver; }\npub mod application;\n",
        config,
    );
    project.write(
        "src/application.rs",
        "#[path = \"adapters/console.rs\"] pub mod console;\n",
    );
    project.write(
        "src/adapters/console.rs",
        "use crate::adapter::Driver; pub fn run(_: Driver) {}\n",
    );
    project.write(
        "src/application/adapters/console.rs",
        "compile_error!(\"rustc must resolve #[path] from the declaring file\");\n",
    );
    assert!(
        project.rustc().status.success(),
        "fixture must compile with rustc"
    );
    let output = project.run("text");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(output.status.code(), Some(1), "{stdout}");
    assert!(stdout.contains("error[core-no-adapter]"));
}

#[test]
fn module_relative_paths_and_imports_create_dependency_edges() {
    for (name, body) in [
        (
            "module-relative-path",
            "pub fn run(_: adapter::Concrete) {}",
        ),
        (
            "module-relative-use",
            "use adapter::Concrete; pub fn run(_: Concrete) {}",
        ),
    ] {
        let source =
            format!("pub mod core {{ pub mod adapter {{ pub struct Concrete; }} {body} }}\n");
        let config = "version = 1\n\n[[roles]]\nid = \"core\"\nmodules = [\"::core$\"]\n\n[[roles]]\nid = \"adapter\"\nmodules = [\"::core::adapter$\"]\n\n[[forbidden]]\nid = \"core-no-adapter\"\nfrom = [\"core\"]\nto = [\"adapter\"]\n";
        let project = ScratchProject::basic(name, &source, config);
        assert!(
            project.rustc().status.success(),
            "{name} must compile with rustc"
        );
        let output = project.run("text");
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert_eq!(
            output.status.code(),
            Some(1),
            "{name} must violate: {stdout}"
        );
        assert!(stdout.contains("error[core-no-adapter]"));
    }
}

#[test]
fn public_reexports_fail_closed_when_role_sets_are_equal() {
    let config = "version = 1\n\n[[roles]]\nid = \"layer\"\npaths = [\"^src/\"]\n\n[[forbidden]]\nid = \"no-layer-dependencies\"\nfrom = [\"layer\"]\nto = [\"layer\"]\n";
    let project = ScratchProject::basic(
        "equal-role-reexport",
        "pub mod adapter { pub struct Driver; }\npub mod api { pub use crate::adapter::Driver; }\npub mod core { use crate::api::Driver; pub fn run(_: Driver) {} }\n",
        config,
    );
    assert!(
        project.rustc().status.success(),
        "fixture must compile with rustc"
    );
    let output = project.run("text");
    assert_eq!(output.status.code(), Some(2));
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .contains("error[opaque-reexport]")
    );
}

#[test]
fn extern_crate_self_and_private_alias_chains_keep_terminal_targets() {
    let extern_alias = ScratchProject::basic(
        "extern-self-alias",
        "extern crate self as aliased;\npub mod adapter { pub struct Concrete; }\npub mod core { pub fn run(_: aliased::adapter::Concrete) {} }\n",
        core_adapter_config(),
    );
    assert!(
        extern_alias.rustc().status.success(),
        "extern crate self alias must compile with rustc"
    );
    let output = extern_alias.run("text");
    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .contains("error[core-no-adapter]")
    );

    let private_chain = ScratchProject::basic(
        "private-alias-chain",
        "pub mod adapter { pub struct Concrete; }\npub mod aliases { use crate::adapter as hidden; use self::hidden as second; pub mod core { pub fn run(_: super::second::Concrete) {} } }\n",
        core_adapter_config(),
    );
    assert!(
        private_chain.rustc().status.success(),
        "private ancestor alias chain must compile with rustc"
    );
    let output = private_chain.run("text");
    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .contains("error[core-no-adapter]")
    );
}

#[test]
fn malformed_root_self_use_is_diagnostic_not_a_panic() {
    let project = ScratchProject::basic(
        "root-self-use",
        "use {self};\npub fn run() {}\n",
        single_role_config(),
    );
    assert!(
        !project.rustc().status.success(),
        "negative control must be rejected by rustc"
    );
    let output = project.run("text");
    assert_eq!(output.status.code(), Some(2));
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("error[unresolved-import]"));
    assert!(!stdout.contains("panicked"));
}

#[test]
fn recursive_canonical_module_source_has_a_stable_diagnostic() {
    let project = ScratchProject::basic(
        "recursive-source",
        "#[path = \"lib.rs\"]\nmod recursive;\n",
        single_role_config(),
    );
    let output = project.run("text");
    assert_eq!(output.status.code(), Some(2));
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .contains("recursive-module-source")
    );
}

#[test]
fn workspace_dependency_uses_the_library_target_name() {
    let project = ScratchProject::new("workspace-lib-name");
    project.write(
        "Cargo.toml",
        "[workspace]\nmembers = [\"domain-package\", \"consumer\"]\nresolver = \"3\"\n",
    );
    project.write(
        "domain-package/Cargo.toml",
        "[package]\nname = \"domain-package\"\nversion = \"0.0.0\"\nedition = \"2024\"\n\n[lib]\nname = \"domain_api\"\n",
    );
    project.write("domain-package/src/lib.rs", "pub struct Entity;\n");
    project.write(
        "consumer/Cargo.toml",
        "[package]\nname = \"consumer\"\nversion = \"0.0.0\"\nedition = \"2024\"\n\n[dependencies]\ndomain-package = { path = \"../domain-package\" }\n",
    );
    project.write(
        "consumer/src/lib.rs",
        "use domain_api::Entity;\npub fn consume(_: Entity) {}\n",
    );
    project.write(
        "hav.toml",
        "version = 1\n\n[[roles]]\nid = \"domain\"\nmodules = [\"^domain-package::lib\"]\n\n[[roles]]\nid = \"consumer\"\nmodules = [\"^consumer::lib\"]\n",
    );
    let cargo = Command::new("cargo")
        .current_dir(&project.root)
        .args(["check", "--workspace", "--offline"])
        .output()
        .expect("cargo check should execute");
    assert!(cargo.status.success(), "fixture must compile with cargo");
    assert_eq!(project.run("text").status.code(), Some(0));
}

#[cfg(unix)]
#[test]
fn path_modules_cannot_escape_by_absolute_traversal_or_symlink_paths() {
    use std::os::unix::fs::symlink;

    let base = ScratchProject::new("containment");
    base.write("outside.rs", "pub struct Outside;\n");
    let outside = base.root.join("outside.rs").canonicalize().unwrap();

    for (name, path) in [
        ("absolute", outside.to_string_lossy().into_owned()),
        ("traversal", "../../outside.rs".to_owned()),
    ] {
        let project_root = base.root.join(name);
        fs::create_dir_all(project_root.join("src")).unwrap();
        fs::write(
            project_root.join("Cargo.toml"),
            format!("[package]\nname = \"{name}\"\nversion = \"0.0.0\"\nedition = \"2024\"\n"),
        )
        .unwrap();
        fs::write(
            project_root.join("src/lib.rs"),
            format!("#[path = \"{path}\"]\nmod outside;\n"),
        )
        .unwrap();
        fs::write(project_root.join("hav.toml"), single_role_config()).unwrap();
        let output = run_root(&project_root, "text");
        assert_eq!(output.status.code(), Some(2));
        assert!(
            String::from_utf8(output.stdout)
                .unwrap()
                .contains("module-outside-workspace")
        );
    }

    let symlink_root = base.root.join("symlink");
    fs::create_dir_all(symlink_root.join("src")).unwrap();
    fs::write(
        symlink_root.join("Cargo.toml"),
        "[package]\nname = \"symlink-case\"\nversion = \"0.0.0\"\nedition = \"2024\"\n",
    )
    .unwrap();
    fs::write(
        symlink_root.join("src/lib.rs"),
        "#[path = \"link.rs\"]\nmod outside;\n",
    )
    .unwrap();
    fs::write(symlink_root.join("hav.toml"), single_role_config()).unwrap();
    symlink(&outside, symlink_root.join("src/link.rs")).unwrap();
    let output = run_root(&symlink_root, "text");
    assert_eq!(output.status.code(), Some(2));
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .contains("module-outside-workspace")
    );
}

#[test]
fn allowed_rules_require_valid_non_empty_forbidden_rule_ids() {
    let base = "version = 1\n\n[[roles]]\nid = \"core\"\npaths = [\"^src/\"]\n\n[[forbidden]]\nid = \"core-rule\"\nfrom = [\"core\"]\nto = [\"core\"]\n";
    let cases = [
        ("missing", "", "must declare non-empty exempts"),
        ("empty", "exempts = []\n", "must declare non-empty exempts"),
        (
            "unknown",
            "exempts = [\"typo\"]\n",
            "unknown forbidden rule 'typo'",
        ),
        (
            "self",
            "exempts = [\"allow-self\"]\n",
            "cannot exempt itself",
        ),
    ];
    for (name, exempts, expected) in cases {
        let config = format!(
            "{base}\n[[allowed]]\nid = \"allow-{name}\"\nfrom = [\"core\"]\nto = [\"core\"]\n{exempts}"
        );
        let project =
            ScratchProject::basic(&format!("allowed-{name}"), "pub fn run() {}\n", &config);
        let output = project.run("text");
        assert_eq!(output.status.code(), Some(2));
        assert!(String::from_utf8(output.stdout).unwrap().contains(expected));
    }

    let allowed_reference = format!(
        "{base}\n[[allowed]]\nid = \"first-allow\"\nfrom = [\"core\"]\nto = [\"core\"]\nexempts = [\"core-rule\"]\n\n[[allowed]]\nid = \"second-allow\"\nfrom = [\"core\"]\nto = [\"core\"]\nexempts = [\"first-allow\"]\n"
    );
    let project =
        ScratchProject::basic("allowed-reference", "pub fn run() {}\n", &allowed_reference);
    let output = project.run("text");
    assert_eq!(output.status.code(), Some(2));
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .contains("cannot exempt allowed rule")
    );
}

#[test]
fn applied_exemptions_are_narrow_auditable_and_preserve_exit_contracts() {
    let source = "pub mod adapter { pub struct Console; }\npub mod core { use crate::adapter::Console; pub fn run(_: Console) {} }\n";
    let roles = "version = 1\n\n[[roles]]\nid = \"core\"\nmodules = [\"::core$\"]\n\n[[roles]]\nid = \"adapter\"\nmodules = [\"::adapter$\"]\n";
    let one_rule = format!(
        "{roles}\n[[forbidden]]\nid = \"core-rule\"\nfrom = [\"core\"]\nto = [\"adapter\"]\n\n[[allowed]]\nid = \"narrow-exception\"\nfrom = [\"core\"]\nto = [\"adapter\"]\nexempts = [\"core-rule\"]\n"
    );
    let passed = ScratchProject::basic("fully-exempt", source, &one_rule);
    let text = passed.run("text");
    assert_eq!(text.status.code(), Some(0));
    assert!(
        String::from_utf8(text.stdout)
            .unwrap()
            .contains("allowed[narrow-exception] exempted forbidden[core-rule]")
    );
    let json = passed.run("json");
    assert_eq!(json.status.code(), Some(0));
    let value: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    assert_eq!(value["summary"]["exemptions"], 1);
    assert_eq!(value["exemptions"][0]["forbidden_rule_id"], "core-rule");

    let two_rules = one_rule.replace(
        "[[allowed]]",
        "[[forbidden]]\nid = \"second-core-rule\"\nfrom = [\"core\"]\nto = [\"adapter\"]\n\n[[allowed]]",
    );
    let violating = ScratchProject::basic("partly-exempt", source, &two_rules);
    let output = violating.run("text");
    assert_eq!(output.status.code(), Some(1));
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("error[second-core-rule]"));
    assert!(stdout.contains("allowed[narrow-exception] exempted forbidden[core-rule]"));
}

#[test]
fn root_configuration_analyzes_this_repository_without_mutation() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let config_path = root.join("hav.toml");
    let config = LoadedConfig::load(&config_path).expect("root config must load");
    let status_before = Command::new("git")
        .current_dir(root)
        .args(["status", "--porcelain=v1", "--untracked-files=all"])
        .output()
        .expect("git status should execute");
    assert!(status_before.status.success());

    let graph = analyze(AnalysisOptions {
        root,
        manifest_path: None,
        strict: config.analysis.strict,
    })
    .expect("the real repository must analyze");
    let report = evaluate(graph, &config);

    let status_after = Command::new("git")
        .current_dir(root)
        .args(["status", "--porcelain=v1", "--untracked-files=all"])
        .output()
        .expect("git status should execute");
    assert!(status_after.status.success());
    assert_eq!(status_before.stdout, status_after.stdout);
    assert_eq!(report.outcome, Outcome::Passed);
    assert!(report.analysis_errors.is_empty());
    assert!(report.findings.is_empty());
}

#[test]
fn shipped_example_validates_the_compliant_fixture_and_applies_a_real_exception() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    let config_path = repository.join("examples/hexagonal.hav.toml");
    let config = LoadedConfig::load(&config_path).expect("shipped example config must load");
    let compliant_root = fixture("compliant");
    let graph = analyze(AnalysisOptions {
        root: &compliant_root,
        manifest_path: None,
        strict: config.analysis.strict,
    })
    .expect("the compliant fixture must analyze");
    let report = evaluate(graph, &config);
    assert_eq!(report.outcome, Outcome::Passed);
    assert!(report.analysis_errors.is_empty());
    assert!(report.findings.is_empty());

    let project = ScratchProject::basic(
        "shipped-example-exemption",
        "pub mod adapter;\npub mod application;\npub mod composition_root;\npub mod core;\npub mod ports;\n",
        include_str!("../examples/hexagonal.hav.toml"),
    );
    project.write(
        "src/adapter.rs",
        "use crate::composition_root;\npub fn start() { composition_root::start(); }\n",
    );
    project.write("src/application.rs", "pub fn run() {}\n");
    project.write("src/composition_root.rs", "pub fn start() {}\n");
    project.write("src/core.rs", "pub struct Order;\n");
    project.write("src/ports.rs", "pub trait Orders {}\n");
    assert!(
        project.rustc().status.success(),
        "non-vacuous example fixture must compile with rustc"
    );
    let output = project.run("json");
    assert_eq!(output.status.code(), Some(0));
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["summary"]["exemptions"], 1);
    assert_eq!(
        value["exemptions"][0]["allowed_rule_id"],
        "adapter-startup-hook"
    );
    assert_eq!(
        value["exemptions"][0]["forbidden_rule_id"],
        "adapters-must-not-depend-on-composition-root"
    );
}

#[test]
fn unmatched_preset_role_fails_analysis() {
    let config = "version = 1\npreset = \"hexagonal\"\n\n[[roles]]\nid = \"core\"\nmodules = [\"::core$\"]\n\n[[roles]]\nid = \"application\"\nmodules = [\"::application$\"]\n\n[[roles]]\nid = \"port\"\nmodules = [\"::port$\"]\n\n[[roles]]\nid = \"adapter\"\nmodules = [\"::adapter$\"]\n\n[[roles]]\nid = \"composition-root\"\nmodules = [\"::composition_root$\"]\n";
    let project = ScratchProject::basic(
        "preset-unmatched",
        "pub mod application {}\npub mod composition_root {}\npub mod core {}\npub mod port {}\n",
        config,
    );
    assert!(project.rustc().status.success());
    let output = project.run("text");
    assert_eq!(output.status.code(), Some(2));
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(stdout.matches("error[role-matched-no-modules]").count(), 1);
    assert!(stdout.contains("declared role 'adapter' matched no discovered modules"));
}

#[test]
fn unmatched_role_cannot_mask_a_real_forbidden_dependency() {
    let config = "version = 1\npreset = \"hexagonal\"\n\n[[roles]]\nid = \"core\"\nmodules = [\"::kore$\"]\n\n[[roles]]\nid = \"application\"\nmodules = [\"::application$\"]\n\n[[roles]]\nid = \"port\"\nmodules = [\"::port$\"]\n\n[[roles]]\nid = \"adapter\"\nmodules = [\"::adapter$\"]\n\n[[roles]]\nid = \"composition-root\"\nmodules = [\"::composition_root$\"]\n";
    let project = ScratchProject::basic(
        "preset-masked-forbidden-edge",
        "pub mod adapter { pub struct Console; }\npub mod application {}\npub mod composition_root {}\npub mod core { use crate::adapter::Console; pub fn run(_: Console) {} }\npub mod port {}\n",
        config,
    );
    assert!(project.rustc().status.success());
    let output = project.run("text");
    assert_eq!(output.status.code(), Some(2));
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("error[role-matched-no-modules]"));
    assert!(stdout.contains("declared role 'core' matched no discovered modules"));
}

#[test]
fn unmatched_roles_fail_analysis_instead_of_passing_vacuously() {
    let config = "version = 1\n\n[[roles]]\nid = \"core\"\nmodules = [\"::kore$\"]\n\n[[roles]]\nid = \"adapter\"\nmodules = [\"::adapter$\"]\n";
    let project = ScratchProject::basic(
        "unmatched-role",
        "pub mod adapter { pub struct Console; }\npub mod core {}\n",
        config,
    );
    let output = project.run("text");
    assert_eq!(output.status.code(), Some(2));
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .contains("role-matched-no-modules")
    );
}

#[test]
fn unresolved_qualified_path_origins_fail_analysis() {
    let project = ScratchProject::basic(
        "path-origin-unresolved",
        "pub mod core { pub fn run(_: super::Nope) {} }\n",
        single_role_config(),
    );
    let output = project.run("text");
    assert_eq!(output.status.code(), Some(2));
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .contains("unresolved-import")
    );
}

#[test]
fn current_module_scope_aliases_and_prelude_names_resolve() {
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
fn edition_2015_top_level_module_wins_before_prelude_names() {
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
fn glob_and_function_imports_keep_terminal_targets() {
    for fixture_name in ["glob-import", "function-import"] {
        assert_fixture_compiles(fixture_name);
        let output = run(fixture_name, "json");
        assert_eq!(output.status.code(), Some(0), "{fixture_name} must pass");
        let report = json_report(&output);
        assert_eq!(report["summary"]["analysis_errors"], 0);
        let root_name = fixture_name.replace('-', "_");
        let root = format!("{fixture_name}-fixture::lib({root_name}_fixture)");
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
        .map(|dependency| dependency["target"].as_str().unwrap().to_owned())
        .collect::<BTreeSet<_>>();
    assert!(targets.contains(&format!("{root}::domain::model")));
    assert!(targets.contains(&format!("{root}::adapters")));
    assert!(!targets.contains(&format!("{root}::adapters::model")));
}

#[test]
fn root_without_manifest_does_not_search_ancestor_directories() {
    let output = run("missing-manifest", "json");
    assert_eq!(output.status.code(), Some(2));
    let report = json_report(&output);
    assert_eq!(report["outcome"], "analysis-failure");
    assert!(
        report["analysis_errors"][0]["message"]
            .as_str()
            .unwrap()
            .contains("missing-manifest/Cargo.toml")
    );
}

#[test]
fn explicit_manifest_may_be_outside_analysis_root() {
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
fn diagnostics_and_findings_share_the_exit_two_text_report() {
    let project = ScratchProject::basic(
        "diagnostic-with-finding",
        "pub mod adapter { pub struct Driver; }\npub mod core { use crate::adapter::Driver; pub fn run(_: Driver) {} }\nstd::include!(\"generated.rs\");\n",
        core_adapter_config(),
    );
    project.write("src/generated.rs", "pub struct Generated;\n");
    assert!(project.rustc().status.success());
    let output = project.run("text");
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stderr.is_empty());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("error[unsupported-include]"));
    assert!(stdout.contains("architecture validation also found 1 violation(s):"));
    assert!(stdout.contains("error[core-no-adapter]"));
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
fn shipped_example_classifies_workspace_paths_and_finds_violation() {
    assert_fixture_source_compiles("example-workspace", "crates/app/src/lib.rs", "2024");
    let config = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/hexagonal.hav.toml");
    let output = run_with_config("example-workspace", config.to_str().unwrap(), "json");
    assert_eq!(output.status.code(), Some(1));
    let report = json_report(&output);
    assert_eq!(report["outcome"], "violations");
    assert_eq!(report["summary"]["violations"], 1);
    assert_eq!(report["summary"]["analysis_errors"], 0);
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
fn explicit_allowed_rule_exempts_only_the_named_forbidden_edge() {
    assert_fixture_compiles("allowed-rule");
    let forbidden = run_with_config("allowed-rule", "without-allowed.hav.toml", "json");
    assert_eq!(forbidden.status.code(), Some(1));
    let forbidden_report = json_report(&forbidden);
    assert_eq!(forbidden_report["summary"]["violations"], 1);

    let allowed = run_with_config("allowed-rule", "with-allowed.hav.toml", "json");
    assert_eq!(allowed.status.code(), Some(0));
    let allowed_report = json_report(&allowed);
    assert_eq!(allowed_report["summary"]["violations"], 0);
    assert_eq!(allowed_report["summary"]["exemptions"], 1);
    assert_eq!(
        allowed_report["exemptions"][0]["allowed_rule_id"],
        "explicit-core-adapter-exception"
    );
    assert_eq!(
        allowed_report["exemptions"][0]["forbidden_rule_id"],
        "core-must-not-depend-on-adapter"
    );
}

#[test]
fn release_and_documentation_contracts_are_pinned() {
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

    let release = repository_file(".github/workflows/release.yml");
    let guard = release
        .find("test -f \"docs/release-notes-${GITHUB_REF_NAME}.md\"")
        .unwrap();
    let attestation = release.find("uses: actions/attest@").unwrap();
    assert!(guard < attestation);

    let ci = repository_file(".github/workflows/ci.yml");
    assert!(ci.contains("package-smoke-test:"));
    assert!(ci.contains("Exercise documented checksum verification"));
    assert!(ci.contains("grep \"  $artifact\\$\" SHA256SUMS | sha256sum -c -"));

    let manifest = repository_file("Cargo.toml");
    assert!(!manifest.lines().any(|line| line.starts_with("thiserror =")));
}

#[test]
fn documentation_limits_where_analysis_limitations_are_reported() {
    let readme = repository_file("README.md");
    let analysis = repository_file("docs/ANALYSIS.md");
    assert!(readme.contains("Text reports do not"));
    assert!(readme.contains("render these limitations"));
    assert!(analysis.contains("reach evaluation include this list in JSON output"));
    assert!(!readme.contains("All reports list the remaining"));
    assert!(!analysis.contains("listed in every JSON report"));

    let text_output = run("compliant", "text");
    let stdout = String::from_utf8(text_output.stdout).unwrap();
    assert!(!stdout.contains("cfg predicates are not evaluated"));
}
