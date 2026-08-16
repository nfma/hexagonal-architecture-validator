use std::fs;
use std::path::Path;

fn smoke_step(workflow: &str) -> Result<&str, String> {
    let marker = "      - name: Smoke-test packaged binary";
    let start = workflow
        .find(marker)
        .ok_or_else(|| "release workflow must smoke-test its package".to_owned())?;
    let rest = &workflow[start..];
    let end = rest[marker.len()..]
        .find("\n      - name:")
        .map_or(rest.len(), |offset| marker.len() + offset);
    Ok(&rest[..end])
}

fn validate_smoke_step(workflow: &str) -> Result<(), String> {
    let smoke = smoke_step(workflow)?;
    if smoke.lines().any(|line| {
        line.trim()
            .strip_prefix("continue-on-error:")
            .is_some_and(|value| value.trim() == "true")
    }) {
        return Err("smoke step must not continue on error".to_owned());
    }

    let run = smoke
        .split_once("        run: |\n")
        .map(|(_, run)| run)
        .ok_or_else(|| "smoke step must use a shell run block".to_owned())?;
    let uses_fail_fast_shell = smoke.lines().any(|line| line.trim() == "shell: bash");
    let sets_fail_fast_options = run.lines().any(|line| line.trim() == "set -euo pipefail");
    if !uses_fail_fast_shell && !sets_fail_fast_options {
        return Err("smoke step must use a fail-fast shell".to_owned());
    }
    if run.lines().any(|line| {
        let command = line.trim();
        matches!(command, "set +e" | "set +o errexit") || command.contains("|| true")
    }) {
        return Err("smoke commands must fail closed".to_owned());
    }

    let commands = run
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| line.replace(['\"', '\''], ""))
        .collect::<Vec<_>>();
    for expected in [
        "${smoke_dir}/hav --version",
        "${smoke_dir}/hav check --root ${GITHUB_WORKSPACE} --config hav.toml --format json > /dev/null",
    ] {
        if !commands.iter().any(|command| command == expected) {
            return Err(format!("smoke step must execute '{expected}' directly"));
        }
    }
    Ok(())
}

fn mutate_smoke_step(workflow: &str, from: &str, to: &str) -> String {
    let step = smoke_step(workflow).expect("release workflow must contain its smoke step");
    let mutant = step.replacen(from, to, 1);
    assert_ne!(mutant, step, "mutation target must occur in the smoke step");
    workflow.replacen(step, &mutant, 1)
}

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

    validate_smoke_step(&workflow).expect("release smoke step must fail closed");
}

#[test]
fn release_smoke_validation_rejects_fail_open_mutations() {
    let workflow = fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join(".github/workflows/release.yml"),
    )
    .expect("release workflow must be readable");
    let mutants = [
        (
            "ignored exit status",
            mutate_smoke_step(
                &workflow,
                "\"${smoke_dir}/hav\" --version",
                "\"${smoke_dir}/hav\" --version || true",
            ),
        ),
        (
            "continue on error",
            mutate_smoke_step(
                &workflow,
                "        shell: bash",
                "        continue-on-error: true\n        shell: bash",
            ),
        ),
        (
            "disabled errexit",
            mutate_smoke_step(
                &workflow,
                "        run: |",
                "        run: |\n          set +e",
            ),
        ),
        (
            "disabled errexit option",
            mutate_smoke_step(
                &workflow,
                "        run: |",
                "        run: |\n          set +o errexit",
            ),
        ),
        (
            "custom bash shell without fail-fast flags",
            mutate_smoke_step(
                &workflow,
                "        shell: bash",
                "        shell: bash --noprofile --norc {0}",
            ),
        ),
        (
            "echo decoy",
            mutate_smoke_step(
                &workflow,
                "\"${smoke_dir}/hav\" --version",
                "echo \"${smoke_dir}/hav\" --version",
            ),
        ),
    ];
    for (name, mutant) in mutants {
        assert!(
            validate_smoke_step(&mutant).is_err(),
            "{name} mutation must be rejected"
        );
    }
}
