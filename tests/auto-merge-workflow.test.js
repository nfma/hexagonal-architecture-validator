const assert = require("node:assert/strict");
const { readFileSync } = require("node:fs");
const { join } = require("node:path");
const test = require("node:test");

const workflowDirectory = join(__dirname, "..", ".github", "workflows");
const signal = readFileSync(
  join(workflowDirectory, "dependabot-auto-merge.yml"),
  "utf8",
);
const reconciler = readFileSync(
  join(workflowDirectory, "auto-merge-reconcile.yml"),
  "utf8",
);

test("keeps pull-request event handling unprivileged", () => {
  for (const event of [
    "pull_request:",
    "pull_request_review:",
    "pull_request_review_comment:",
    "ready_for_review",
    "converted_to_draft",
    "labeled",
    "unlabeled",
  ]) {
    assert.ok(signal.includes(event), `missing signal event: ${event}`);
  }
  assert.match(signal, /permissions: \{\}/);
  assert.doesNotMatch(
    signal,
    /pull_request_target|workflow_run|actions\/checkout|secrets\.|artifacts?|cache/,
  );
});

test("uses a default-branch App-token reconciler without PR code", () => {
  assert.match(reconciler, /workflow_run:/);
  assert.match(reconciler, /schedule:/);
  assert.match(reconciler, /cron: "\*\/5 \* \* \* \*"/);
  assert.match(reconciler, /workflow_dispatch:/);
  assert.match(reconciler, /cancel-in-progress: false/);
  assert.match(
    reconciler,
    /actions\/create-github-app-token@bcd2ba49218906704ab6c1aa796996da409d3eb1/,
  );
  assert.match(
    reconciler,
    /actions\/github-script@3a2844b7e9c422d3c10d287c895573f7108da1b3/,
  );
  assert.match(reconciler, /vars\.NFMA_AUTO_MERGE_CLIENT_ID/);
  assert.match(reconciler, /secrets\.NFMA_AUTO_MERGE_PRIVATE_KEY/);
  assert.match(reconciler, /permission-contents: write/);
  assert.match(reconciler, /permission-pull-requests: write/);
  assert.doesNotMatch(
    reconciler,
    /pull_request_target|actions\/checkout|download-artifact|upload-artifact|actions\/cache/,
  );
});

test("fails closed on every eligibility boundary", () => {
  for (const gate of [
    'new Set(["nfma", "dependabot[bot]"])',
    "pull.headRepository?.nameWithOwner === repositoryFullName",
    "pull.baseRepository?.nameWithOwner === repositoryFullName",
    "pull.baseRefName === defaultBranch",
    "!pull.isDraft",
    'author === "dependabot[bot]" || labelNames.has("automerge")',
    'pull.mergeable === "MERGEABLE"',
    'pull.mergeStateStatus === "CLEAN"',
    'pull.statusCheckRollup?.state === "SUCCESS"',
    'pull.reviewDecision === "APPROVED"',
    "allReviewThreadsResolved",
  ]) {
    assert.ok(reconciler.includes(gate), `missing gate: ${gate}`);
  }
  assert.match(reconciler, /reviewThreads\(first: 100, after: \$cursor\)/);
  assert.match(reconciler, /threads\.pageInfo\.hasNextPage/);
  assert.match(reconciler, /fresh\.headRefOid !== initial\.headRefOid/);
});

test("reconciles both directions and permits only squash", () => {
  assert.match(reconciler, /disablePullRequestAutoMerge/);
  assert.match(reconciler, /enablePullRequestAutoMerge/);
  assert.match(reconciler, /mergeMethod: SQUASH/);
  assert.match(reconciler, /expectedHeadOid: \$expectedHeadOid/);
  assert.match(reconciler, /fresh\.armed && fresh\.mergeMethod === "SQUASH"/);
  assert.doesNotMatch(reconciler, /mergeMethod: (MERGE|REBASE)/);
});
