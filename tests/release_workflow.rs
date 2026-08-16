use std::fs;
use std::path::Path;

#[test]
fn packaged_binary_is_executed_before_release_upload() {
    let workflow = fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join(".github/workflows/release.yml"),
    )
    .expect("release workflow must be readable");

    let smoke = workflow
        .find("- name: Smoke-test packaged binary")
        .expect("release workflow must smoke-test its package");
    let upload = workflow
        .find("- name: Upload release archive")
        .expect("release workflow must upload its package");
    assert!(
        smoke < upload,
        "the package must be exercised before upload"
    );

    let smoke_step = &workflow[smoke..upload];
    assert!(smoke_step.contains("tar -C \"${smoke_dir}\" -xzf \"${artifact}\""));
    assert!(smoke_step.contains("\"${smoke_dir}/hav\" --version"));
    assert!(smoke_step.contains(
        "\"${smoke_dir}/hav\" check --root \"${GITHUB_WORKSPACE}\" --config hav.toml --format json"
    ));
}
