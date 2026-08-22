# Semgrep gate runbook

The `Semgrep and Gitleaks` required check runs Semgrep 1.173.0 with hash-pinned
snapshots of the `p/default` and `p/security-audit` community rule packs. CI
downloads each snapshot to temporary storage and verifies its URL, byte size,
and SHA-256 digest before Semgrep reads it. The checker fails closed when the
pack integrity changes, the scan reports a finding, skips a rule, changes
engine, reaches a fixpoint timeout, loses required path coverage, or emits an
unreviewed parser warning.

The repository deliberately does not commit the rule bodies. The
[Semgrep Rules License v1.0](https://semgrep.dev/legal/rules-license/) permits
internal use but prohibits distributing the rules. `.semgrep/packs.lock.json`
therefore records only source URLs, hashes, byte sizes, the tested Semgrep
version, and license provenance.

## Coverage

The checker derives its required paths from production and CI sources:

- `Cargo.toml`, `build.rs`, and every Rust source under `src/`
- `.github/dependabot.yml`, workflows, and local actions
- supported script and configuration files under `scripts/`

`target/` is excluded because it contains generated build output. Test fixtures
are intentionally not required: several contain malformed or deliberately
violating source used to test fail-closed behavior. Semgrep may still inspect
fixture files it recognizes, but fixture coverage does not determine whether
the required gate passes.

## Run the gate locally

From the repository root:

```sh
semgrep_work_dir=$(mktemp -d)
semgrep_report="$semgrep_work_dir/semgrep.json"
semgrep_rules=.semgrep/rules
trap 'rm -rf "$semgrep_rules" "$semgrep_work_dir"' EXIT

mkdir -p "$semgrep_rules"
curl --fail --silent --show-error --proto '=https' --tlsv1.2 \
  --max-time 60 --max-filesize 8388608 \
  --output "$semgrep_rules/default.yml" \
  https://semgrep.dev/c/p/default
curl --fail --silent --show-error --proto '=https' --tlsv1.2 \
  --max-time 60 --max-filesize 8388608 \
  --output "$semgrep_rules/security-audit.yml" \
  https://semgrep.dev/c/p/security-audit
python3 scripts/semgrep_packs.py verify \
  --manifest .semgrep/packs.lock.json \
  --input-dir "$semgrep_rules"

uvx --no-build --from semgrep==1.173.0 semgrep scan \
  --config "$semgrep_rules/default.yml" \
  --config "$semgrep_rules/security-audit.yml" \
  --error \
  --metrics=off \
  --exclude target \
  --json \
  --output "$semgrep_report" \
  .

python3 scripts/check_semgrep.py \
  --report "$semgrep_report" \
  --baseline .semgrep-baseline.json \
  --repository-root .
```

The exit trap removes the report and gitignored rule cache. Do not copy the
fetched rule bodies into the repository or another public location.

## Update pinned rule packs

`Update Semgrep rules` runs every Monday and can also be started manually. It:

1. downloads the two allowlisted Registry packs;
2. updates only their hashes and byte sizes in `.semgrep/packs.lock.json`;
3. fetches the packs again through the integrity verifier;
4. validates both packs with the pinned Semgrep version; and
5. opens a signed draft pull request containing only the lock manifest.

The draft does not auto-merge. Review its hash and size changes, the Semgrep
scan result, parser-warning baseline, and all normal protected checks before
marking it ready. GitHub may require a maintainer to approve workflow runs for
a pull request created by `GITHUB_TOKEN`.

The repository setting **Settings → Actions → General → Workflow permissions →
Allow GitHub Actions to create and approve pull requests** must be enabled for
the scheduled job to open its draft. The workflow still receives no default
permissions; write access is limited to the updater job and the PR contains
only the lock manifest.

To prepare the same update locally:

```sh
python3 scripts/semgrep_packs.py update \
  --manifest .semgrep/packs.lock.json \
  --input-dir "$semgrep_rules"

python3 -m unittest tests/test_semgrep_packs.py
python3 scripts/semgrep_packs.py verify \
  --manifest .semgrep/packs.lock.json \
  --input-dir "$semgrep_rules"

uvx --no-build --from semgrep==1.173.0 semgrep scan \
  --validate \
  --metrics=off \
  --config "$semgrep_rules/default.yml" \
  --config "$semgrep_rules/security-audit.yml"
```

Then run the full gate above. Never edit a digest or byte size without fetching
and validating the corresponding rule pack.

## Review and regenerate parser warnings

`.semgrep-baseline.json` identifies reviewed warnings by rule metadata, path,
source line, and a line-number-neutral message. Moving unchanged source does
not invalidate the baseline, while changing the source or warning still
requires review.

Generate a candidate outside the working tree:

```sh
semgrep_candidate="$semgrep_work_dir/semgrep-baseline.json"

python3 scripts/check_semgrep.py \
  --report "$semgrep_report" \
  --baseline .semgrep-baseline.json \
  --repository-root . \
  --write-baseline "$semgrep_candidate"
```

The command preserves rationales for unchanged warnings. It writes
`REVIEW REQUIRED` for every new warning and exits nonzero. For each new entry:

1. inspect the referenced source and Semgrep rule;
2. confirm the warning is a parser limitation rather than lost analysis;
3. replace the placeholder with a specific rationale; and
4. compare the candidate with the committed baseline.

Only after every new warning is justified, replace the committed baseline with
the candidate and rerun:

```sh
python3 -m unittest tests/test_check_semgrep.py
cargo test --locked --test workflow_security
python3 scripts/check_semgrep.py \
  --report "$semgrep_report" \
  --baseline .semgrep-baseline.json \
  --repository-root .
```

Never edit a warning identity to make the gate green. If the source, rule,
engine, skipped-rule set, timeout state, or required coverage changed, review
the underlying scan first.
