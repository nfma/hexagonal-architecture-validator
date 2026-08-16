use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

use hexagonal_architecture_validator::analyzer::{AnalysisOptions, analyze};
use hexagonal_architecture_validator::config::LoadedConfig;
use hexagonal_architecture_validator::evaluate::evaluate;
use hexagonal_architecture_validator::model::Outcome;
use hexagonal_architecture_validator::report::render_text;

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
        let project = Self::new(name);
        project.write(
            "Cargo.toml",
            &format!("[package]\nname = \"{name}\"\nversion = \"0.0.0\"\nedition = \"2024\"\n"),
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

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn run(name: &str, format: &str) -> Output {
    run_root(&fixture(name), format)
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
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("unsupported config version 99; expected 1"));

    let json_output = run("malformed-config", "json");
    assert_eq!(json_output.status.code(), Some(2));
    let stdout = String::from_utf8(json_output.stdout).unwrap();
    assert!(stdout.contains("\"schema_version\": 1"));
    assert!(stdout.contains("\"outcome\": \"configuration-or-analysis-failure\""));
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
        let output = project.run("text");
        assert_eq!(output.status.code(), Some(0), "{name} must pass hav");
    }
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
        assert!(String::from_utf8(output.stderr).unwrap().contains(expected));
    }

    let allowed_reference = format!(
        "{base}\n[[allowed]]\nid = \"first-allow\"\nfrom = [\"core\"]\nto = [\"core\"]\nexempts = [\"core-rule\"]\n\n[[allowed]]\nid = \"second-allow\"\nfrom = [\"core\"]\nto = [\"core\"]\nexempts = [\"first-allow\"]\n"
    );
    let project =
        ScratchProject::basic("allowed-reference", "pub fn run() {}\n", &allowed_reference);
    let output = project.run("text");
    assert_eq!(output.status.code(), Some(2));
    assert!(
        String::from_utf8(output.stderr)
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
fn shipped_example_analyzes_this_repository_without_mutation() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let config_path = root.join("examples/hexagonal.hav.toml");
    let config = LoadedConfig::load(&config_path).expect("shipped example config must load");
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
    assert_eq!(report.notices.len(), 4);
    assert!(
        report
            .notices
            .iter()
            .all(|notice| notice.code == "preset-role-unmatched")
    );
    let text = render_text(&report);
    assert!(text.contains(
        "notice[preset-role-unmatched] preset role 'adapter' matched no discovered modules"
    ));
}

#[test]
fn unmatched_preset_roles_are_non_fatal_notices() {
    let config = "version = 1\npreset = \"hexagonal\"\n\n[[roles]]\nid = \"core\"\nmodules = [\"::core$\"]\n\n[[roles]]\nid = \"application\"\nmodules = [\"::application$\"]\n\n[[roles]]\nid = \"port\"\nmodules = [\"::port$\"]\n\n[[roles]]\nid = \"adapter\"\nmodules = [\"::adapter$\"]\n\n[[roles]]\nid = \"composition-root\"\nmodules = [\"::composition_root$\"]\n";
    let project = ScratchProject::basic(
        "preset-unmatched-notice",
        "pub mod core { pub struct Order; }\n",
        config,
    );
    let output = project.run("text");
    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains(
        "notice[preset-role-unmatched] preset role 'adapter' matched no discovered modules"
    ));
    assert!(!stdout.contains("role-matched-no-modules"));

    let json = project.run("json");
    assert_eq!(json.status.code(), Some(0));
    let value: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    assert_eq!(value["summary"]["notices"], 4);
    assert_eq!(value["notices"].as_array().unwrap().len(), 4);

    let violating = ScratchProject::basic(
        "preset-notice-with-violation",
        "pub mod adapter { pub struct Console; }\npub mod core { use crate::adapter::Console; pub fn run(_: Console) {} }\n",
        config,
    );
    let output = violating.run("text");
    assert_eq!(output.status.code(), Some(1));
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("error[core-must-not-depend-on-adapters]"));
    assert!(stdout.contains("notice[preset-role-unmatched]"));
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
