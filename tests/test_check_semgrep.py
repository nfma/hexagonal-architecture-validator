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


def baseline_entry(error: dict[str, object]) -> dict[str, object]:
    entry = CHECKER.normalize_semgrep_error(error)
    return {**entry, "rationale": "Semgrep cannot parse a valid GitHub matrix expression."}


def baseline(*errors: dict[str, object]) -> dict[str, object]:
    return {
        "schemaVersion": 1,
        "semgrepVersion": "1.173.0",
        "errors": [baseline_entry(error) for error in errors],
    }


def report(*errors: dict[str, object]) -> dict[str, object]:
    return {
        "version": "1.173.0",
        "results": [],
        "errors": list(errors),
        "paths": {"scanned": ["src/lib.rs"]},
    }


class SemgrepReportCheckerTests(unittest.TestCase):
    def test_accepts_an_exact_reviewed_error_set(self) -> None:
        error = known_error()

        summary = CHECKER.validate_report(
            report(error), baseline(error), required_paths={"src/lib.rs"}
        )

        self.assertEqual(summary, {"errors": 1, "scanned": 1})

    def test_rejects_a_new_parser_warning(self) -> None:
        reviewed = known_error()
        new_error = known_error(line=41)

        with self.assertRaisesRegex(CHECKER.ValidationError, "unexpected errors"):
            CHECKER.validate_report(
                report(reviewed, new_error),
                baseline(reviewed),
                required_paths={"src/lib.rs"},
            )

    def test_rejects_a_stale_baseline_entry(self) -> None:
        reviewed = known_error()

        with self.assertRaisesRegex(CHECKER.ValidationError, "missing reviewed errors"):
            CHECKER.validate_report(
                report(), baseline(reviewed), required_paths={"src/lib.rs"}
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
                semgrep_report, baseline(), required_paths={"src/lib.rs"}
            )

    def test_rejects_an_unscanned_required_path(self) -> None:
        with self.assertRaisesRegex(CHECKER.ValidationError, "required paths were not scanned"):
            CHECKER.validate_report(
                report(),
                baseline(),
                required_paths={"src/lib.rs", "src/main.rs"},
            )

    def test_rejects_blank_rationales_and_unknown_baseline_fields(self) -> None:
        error = known_error()
        invalid = baseline(error)
        invalid["errors"][0]["rationale"] = ""
        invalid["errors"][0]["accepted"] = True

        with self.assertRaisesRegex(CHECKER.ValidationError, "baseline entry fields"):
            CHECKER.validate_report(
                report(error), invalid, required_paths={"src/lib.rs"}
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
