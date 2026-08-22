# Semgrep gate runbook

The `Semgrep and Gitleaks` required check runs Semgrep 1.173.0 with the
`p/default` and `p/security-audit` community rule packs. The checker fails
closed when the scan reports a finding, skips a rule, changes engine, reaches a
fixpoint timeout, loses required path coverage, or emits an unreviewed parser
warning.

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

uvx --no-build --from semgrep==1.173.0 semgrep scan \
  --config p/default \
  --config p/security-audit \
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

The community rule packs are fetched from the Semgrep registry at scan time.
Their rule identities can therefore change independently of this repository;
any resulting gate change must be reviewed, not accepted automatically.

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
