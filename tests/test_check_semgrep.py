from __future__ import annotations

import importlib.util
import io
import json
import tempfile
import unittest
from contextlib import redirect_stderr
from pathlib import Path


REPOSITORY_ROOT = Path(__file__).resolve().parents[1]
CHECKER_PATH = REPOSITORY_ROOT / "scripts" / "check_semgrep.py"
SPEC = importlib.util.spec_from_file_location("check_semgrep", CHECKER_PATH)
if SPEC is None or SPEC.loader is None:
    raise RuntimeError("could not load Semgrep report checker")
CHECKER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(CHECKER)


def known_error(line: int = 38) -> dict[str, object]:
    return {
        "code": 3,
        "level": "warn",
        "type": [
            "PartialParsing",
            [
                {
                    "path": ".github/workflows/ci.yml",
                    "start": {"line": line, "col": 21, "offset": 0},
                    "end": {"line": line, "col": 24, "offset": 3},
                }
            ],
        ],
        "message": (
            f"Syntax error at line .github/workflows/ci.yml:{line}:\n"
            " When parsing a snippet as Bash for metavariable-pattern in rule "
            "'yaml.github-actions.security.curl-eval.curl-eval', `${{` was unexpected"
        ),
        "path": ".github/workflows/ci.yml",
        "spans": [
            {
                "file": ".github/workflows/ci.yml",
                "start": {"line": line, "col": 21, "offset": 0},
                "end": {"line": line, "col": 24, "offset": 3},
            }
        ],
    }


def baseline_entry(
    error: dict[str, object],
    repository_root: Path = REPOSITORY_ROOT,
) -> dict[str, object]:
    entry = CHECKER.normalize_semgrep_error(error, repository_root)
    return {**entry, "rationale": "Semgrep cannot parse a valid GitHub matrix expression."}


def baseline(
    *errors: dict[str, object],
    repository_root: Path = REPOSITORY_ROOT,
) -> dict[str, object]:
    return {
        "schemaVersion": 1,
        "semgrepVersion": "1.173.0",
        "errors": [baseline_entry(error, repository_root) for error in errors],
    }


def report(*errors: dict[str, object]) -> dict[str, object]:
    return {
        "version": "1.173.0",
        "engine_requested": "OSS",
        "skipped_rules": [],
        "results": [],
        "errors": list(errors),
        "paths": {"scanned": ["src/lib.rs"]},
        "time": {"fixpoint_timeouts": []},
    }


class SemgrepReportCheckerTests(unittest.TestCase):
    def test_accepts_an_exact_reviewed_error_set(self) -> None:
        error = known_error()

        summary = CHECKER.validate_report(
            report(error),
            baseline(error),
            required_paths={"src/lib.rs"},
            repository_root=REPOSITORY_ROOT,
        )

        self.assertEqual(summary, {"errors": 1, "scanned": 1, "skipped_rules": 0})

    def test_rejects_a_new_parser_warning(self) -> None:
        reviewed = known_error()
        new_error = known_error(line=41)

        with self.assertRaisesRegex(CHECKER.ValidationError, "unexpected errors"):
            CHECKER.validate_report(
                report(reviewed, new_error),
                baseline(reviewed),
                required_paths={"src/lib.rs"},
                repository_root=REPOSITORY_ROOT,
            )

    def test_rejects_a_stale_baseline_entry(self) -> None:
        reviewed = known_error()

        with self.assertRaisesRegex(CHECKER.ValidationError, "missing reviewed errors"):
            CHECKER.validate_report(
                report(),
                baseline(reviewed),
                required_paths={"src/lib.rs"},
                repository_root=REPOSITORY_ROOT,
            )

    def test_accepts_a_reviewed_warning_after_its_source_line_moves(self) -> None:
        with (
            tempfile.TemporaryDirectory() as old_directory,
            tempfile.TemporaryDirectory() as new_directory,
        ):
            old_root = Path(old_directory)
            new_root = Path(new_directory)
            source = "run: cargo test --toolchain ${{ matrix.toolchain }}\n"
            for root, content in ((old_root, source), (new_root, f"# comment\n{source}")):
                path = root / ".github/workflows/ci.yml"
                path.parent.mkdir(parents=True)
                path.write_text(content, encoding="utf-8")

            reviewed = baseline(known_error(line=1), repository_root=old_root)

            summary = CHECKER.validate_report(
                report(known_error(line=2)),
                reviewed,
                required_paths={"src/lib.rs"},
                repository_root=new_root,
            )

            self.assertEqual(
                summary,
                {"errors": 1, "scanned": 1, "skipped_rules": 0},
            )

    def test_baseline_generation_preserves_reviews_and_marks_new_warnings(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            workflow = root / ".github/workflows/ci.yml"
            workflow.parent.mkdir(parents=True)
            workflow.write_text("first command\nsecond command\n", encoding="utf-8")
            reviewed = known_error(line=1)
            new_warning = known_error(line=2)

            generated = CHECKER.build_baseline(
                report(reviewed, new_warning),
                baseline(reviewed, repository_root=root),
                required_paths={"src/lib.rs"},
                repository_root=root,
            )

            rationales = {
                entry["source"]: entry["rationale"] for entry in generated["errors"]
            }
            self.assertEqual(
                rationales["first command"],
                "Semgrep cannot parse a valid GitHub matrix expression.",
            )
            self.assertEqual(
                rationales["second command"],
                CHECKER.REVIEW_REQUIRED,
            )

    def test_cli_writes_a_candidate_but_fails_until_new_warnings_are_reviewed(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            workflow = root / ".github/workflows/ci.yml"
            workflow.parent.mkdir(parents=True)
            workflow.write_text("first command\nsecond command\n", encoding="utf-8")
            source = root / "src/lib.rs"
            source.parent.mkdir(parents=True)
            source.write_text("pub fn checked() {}\n", encoding="utf-8")
            reviewed = known_error(line=1)
            new_warning = known_error(line=2)
            report_path = root / "report.json"
            baseline_path = root / "baseline.json"
            candidate_path = root / "candidate.json"
            semgrep_report = report(reviewed, new_warning)
            semgrep_report["paths"] = {
                "scanned": ["src/lib.rs", ".github/workflows/ci.yml"]
            }
            report_path.write_text(
                json.dumps(semgrep_report), encoding="utf-8"
            )
            baseline_path.write_text(
                json.dumps(baseline(reviewed, repository_root=root)),
                encoding="utf-8",
            )
            stderr = io.StringIO()

            with redirect_stderr(stderr):
                result = CHECKER.main(
                    [
                        "--report",
                        str(report_path),
                        "--baseline",
                        str(baseline_path),
                        "--repository-root",
                        str(root),
                        "--write-baseline",
                        str(candidate_path),
                    ]
                )

            self.assertEqual(result, 1)
            self.assertIn("replace every", stderr.getvalue())
            candidate = json.loads(candidate_path.read_text(encoding="utf-8"))
            self.assertTrue(
                any(
                    entry["rationale"] == CHECKER.REVIEW_REQUIRED
                    for entry in candidate["errors"]
                )
            )

    def test_rejects_skipped_rules(self) -> None:
        semgrep_report = report()
        semgrep_report["skipped_rules"] = [{"rule_id": "failed.rule"}]

        with self.assertRaisesRegex(CHECKER.ValidationError, "skipped rules"):
            CHECKER.validate_report(
                semgrep_report,
                baseline(),
                required_paths={"src/lib.rs"},
                repository_root=REPOSITORY_ROOT,
            )

    def test_rejects_an_engine_downgrade(self) -> None:
        semgrep_report = report()
        semgrep_report["engine_requested"] = "NONE"

        with self.assertRaisesRegex(CHECKER.ValidationError, "engine"):
            CHECKER.validate_report(
                semgrep_report,
                baseline(),
                required_paths={"src/lib.rs"},
                repository_root=REPOSITORY_ROOT,
            )

    def test_rejects_fixpoint_timeouts(self) -> None:
        semgrep_report = report()
        semgrep_report["time"] = {"fixpoint_timeouts": ["src/lib.rs"]}

        with self.assertRaisesRegex(CHECKER.ValidationError, "fixpoint timeouts"):
            CHECKER.validate_report(
                semgrep_report,
                baseline(),
                required_paths={"src/lib.rs"},
                repository_root=REPOSITORY_ROOT,
            )

    def test_rejects_findings_even_if_semgrep_error_mode_is_removed(self) -> None:
        semgrep_report = report()
        semgrep_report["results"] = [
            {
                "check_id": "rust.lang.security.example",
                "path": "src/lib.rs",
                "start": {"line": 1},
            }
        ]

        with self.assertRaisesRegex(CHECKER.ValidationError, "findings"):
            CHECKER.validate_report(
                semgrep_report,
                baseline(),
                required_paths={"src/lib.rs"},
                repository_root=REPOSITORY_ROOT,
            )

    def test_rejects_an_unscanned_required_path(self) -> None:
        with self.assertRaisesRegex(CHECKER.ValidationError, "required paths were not scanned"):
            CHECKER.validate_report(
                report(),
                baseline(),
                required_paths={"src/lib.rs", "src/main.rs"},
                repository_root=REPOSITORY_ROOT,
            )

    def test_rejects_blank_rationales_and_unknown_baseline_fields(self) -> None:
        error = known_error()
        invalid = baseline(error)
        invalid["errors"][0]["rationale"] = ""
        invalid["errors"][0]["accepted"] = True

        with self.assertRaisesRegex(CHECKER.ValidationError, "baseline entry fields"):
            CHECKER.validate_report(
                report(error),
                invalid,
                required_paths={"src/lib.rs"},
                repository_root=REPOSITORY_ROOT,
            )

    def test_required_paths_cover_production_and_ci_but_not_fixtures(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            paths = [
                ".github/dependabot.yml",
                ".github/workflows/security.yml",
                ".github/actions/check/action.yml",
                "Cargo.toml",
                "build.rs",
                "scripts/check.py",
                "scripts/install.sh",
                "src/lib.rs",
                "tests/fixtures/unsafe/src/lib.rs",
                "tests/test_check.py",
            ]
            for relative_path in paths:
                path = root / relative_path
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text("fixture", encoding="utf-8")

            self.assertEqual(
                CHECKER.required_scan_paths(root),
                {
                    ".github/actions/check/action.yml",
                    ".github/dependabot.yml",
                    ".github/workflows/security.yml",
                    "Cargo.toml",
                    "build.rs",
                    "scripts/check.py",
                    "scripts/install.sh",
                    "src/lib.rs",
                },
            )

    def test_cli_rejects_invalid_json_without_a_traceback(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            report_path = root / "report.json"
            baseline_path = root / "baseline.json"
            report_path.write_text("not JSON", encoding="utf-8")
            baseline_path.write_text(json.dumps(baseline()), encoding="utf-8")
            stderr = io.StringIO()

            with redirect_stderr(stderr):
                self.assertEqual(
                    CHECKER.main(
                        [
                            "--report",
                            str(report_path),
                            "--baseline",
                            str(baseline_path),
                            "--repository-root",
                            str(root),
                        ]
                    ),
                    1,
                )
            self.assertIn("could not read Semgrep report", stderr.getvalue())
            self.assertNotIn("Traceback", stderr.getvalue())


if __name__ == "__main__":
    unittest.main()
