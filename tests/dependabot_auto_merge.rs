use std::fs;
use std::path::Path;

fn workflow(path: &str) -> String {
    fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(path))
        .unwrap_or_else(|error| panic!("failed to read {path}: {error}"))
}

fn section<'a>(contents: &'a str, start: &str, end: Option<&str>) -> &'a str {
    let (_, contents) = contents
        .split_once(start)
        .unwrap_or_else(|| panic!("missing section start: {start}"));
    end.and_then(|marker| contents.split_once(marker).map(|(section, _)| section))
        .unwrap_or(contents)
}

#[test]
fn auto_merge_allows_only_cargo_patch_and_minor_updates() {
    let workflow = workflow(".github/workflows/dependabot-auto-merge.yml");
    assert!(workflow.contains(
        "uses: dependabot/fetch-metadata@25dd0e34f4fe68f24cc83900b1fe3fe149efef98 # v3.1.0"
    ));

    let merge_step = section(
        &workflow,
        "      - name: Enable squash auto-merge\n",
        Some("      - name: Explain manual review requirement\n"),
    );
    assert!(merge_step.contains("steps.metadata.outputs.package-ecosystem == 'cargo'"));
    assert!(
        merge_step.contains("steps.metadata.outputs.update-type == 'version-update:semver-patch'")
    );
    assert!(
        merge_step.contains("steps.metadata.outputs.update-type == 'version-update:semver-minor'")
    );
    assert!(!merge_step.contains("version-update:semver-major"));
    assert!(merge_step.contains("gh pr merge --repo \"$GH_REPO\" --auto --squash \"$PR_NUMBER\""));

    let manual_review_step = section(
        &workflow,
        "      - name: Explain manual review requirement\n",
        None,
    );
    assert!(manual_review_step.contains("steps.metadata.outputs.package-ecosystem != 'cargo'"));
    assert!(
        manual_review_step
            .contains("steps.metadata.outputs.update-type != 'version-update:semver-patch'")
    );
    assert!(
        manual_review_step
            .contains("steps.metadata.outputs.update-type != 'version-update:semver-minor'")
    );
}

#[test]
fn untrusted_sonar_analysis_is_visibly_not_applicable() {
    let workflow = workflow(".github/workflows/sonar.yml");
    let sonar_job = section(&workflow, "  sonar:\n", Some("  sonar-not-applicable:\n"));

    assert!(sonar_job.contains("name: SonarQube Cloud"));
    assert!(sonar_job.contains("github.event.pull_request.user.login != 'dependabot[bot]'"));
    assert!(
        sonar_job.contains("github.event.pull_request.head.repo.full_name == github.repository")
    );
    assert!(sonar_job.contains("Scan and wait for the quality gate"));
    assert!(sonar_job.contains("SONAR_TOKEN: ${{ secrets.SONAR_TOKEN }}"));
    assert!(!sonar_job.contains("steps.trust.outputs.trusted"));

    let not_applicable_job = section(&workflow, "  sonar-not-applicable:\n", None);
    assert!(not_applicable_job.contains("name: SonarQube Cloud (not applicable)"));
    assert!(
        not_applicable_job.contains("github.event.pull_request.user.login == 'dependabot[bot]'")
    );
    assert!(
        not_applicable_job
            .contains("github.event.pull_request.head.repo.full_name != github.repository")
    );
    assert!(not_applicable_job.contains("the protected Sonar job is intentionally skipped"));
    assert!(!not_applicable_job.contains("secrets.SONAR_TOKEN"));
    assert!(!not_applicable_job.contains("uses:"));
}
