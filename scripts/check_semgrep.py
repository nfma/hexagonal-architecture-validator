#!/usr/bin/env python3
from __future__ import annotations

import argparse
import json
import sys
from collections import Counter
from pathlib import Path
from typing import Any


BASELINE_FIELDS = {"schemaVersion", "semgrepVersion", "errors"}
ERROR_FIELDS = {
    "code",
    "level",
    "type",
    "path",
    "start",
    "end",
    "message",
    "rationale",
}
POSITION_FIELDS = {"line", "column"}
SCANNED_CODE_SUFFIXES = {".cjs", ".js", ".mjs", ".py", ".sh", ".ts", ".tsx", ".yaml", ".yml"}


class ValidationError(ValueError):
    pass


def _mapping(value: object, label: str) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise ValidationError(f"{label} must be a JSON object")
    return value


def _exact_fields(value: dict[str, Any], expected: set[str], label: str) -> None:
    actual = set(value)
    if actual != expected:
        raise ValidationError(
            f"{label} fields differ: missing={sorted(expected - actual)}, "
            f"unknown={sorted(actual - expected)}"
        )


def _position(value: object, label: str) -> dict[str, int]:
    position = _mapping(value, label)
    _exact_fields(position, POSITION_FIELDS, label)
    line = position.get("line")
    column = position.get("column")
    if not isinstance(line, int) or line < 1:
        raise ValidationError(f"{label}.line must be a positive integer")
    if not isinstance(column, int) or column < 1:
        raise ValidationError(f"{label}.column must be a positive integer")
    return {"line": line, "column": column}


def _signature(value: object, label: str) -> dict[str, object]:
    signature = _mapping(value, label)
    expected = ERROR_FIELDS - {"rationale"}
    _exact_fields(signature, expected, label)

    for field in ("level", "type", "path", "message"):
        if not isinstance(signature.get(field), str) or not signature[field]:
            raise ValidationError(f"{label}.{field} must be a non-empty string")
    if not isinstance(signature.get("code"), int):
        raise ValidationError(f"{label}.code must be an integer")

    return {
        "code": signature["code"],
        "level": signature["level"],
        "type": signature["type"],
        "path": signature["path"],
        "start": _position(signature.get("start"), f"{label}.start"),
        "end": _position(signature.get("end"), f"{label}.end"),
        "message": signature["message"],
    }


def normalize_semgrep_error(value: object) -> dict[str, object]:
    error = _mapping(value, "Semgrep error")
    error_type = error.get("type")
    if not isinstance(error_type, list) or not error_type or not isinstance(error_type[0], str):
        raise ValidationError("Semgrep error type must contain a named error kind")

    spans = error.get("spans")
    if not isinstance(spans, list) or not spans:
        raise ValidationError("Semgrep error must contain at least one source span")
    span = _mapping(spans[0], "Semgrep error span")
    start = _mapping(span.get("start"), "Semgrep error span start")
    end = _mapping(span.get("end"), "Semgrep error span end")

    normalized = {
        "code": error.get("code"),
        "level": error.get("level"),
        "type": error_type[0],
        "path": error.get("path"),
        "start": {"line": start.get("line"), "column": start.get("col")},
        "end": {"line": end.get("line"), "column": end.get("col")},
        "message": error.get("message"),
    }
    return _signature(normalized, "normalized Semgrep error")


def _canonical(value: dict[str, object]) -> str:
    return json.dumps(value, sort_keys=True, separators=(",", ":"))


def _baseline_errors(baseline: object) -> tuple[str, list[dict[str, object]]]:
    document = _mapping(baseline, "baseline")
    _exact_fields(document, BASELINE_FIELDS, "baseline")
    if document.get("schemaVersion") != 1:
        raise ValidationError("baseline.schemaVersion must be 1")
    version = document.get("semgrepVersion")
    if not isinstance(version, str) or not version:
        raise ValidationError("baseline.semgrepVersion must be a non-empty string")
    entries = document.get("errors")
    if not isinstance(entries, list):
        raise ValidationError("baseline.errors must be a list")

    signatures = []
    for index, value in enumerate(entries):
        entry = _mapping(value, f"baseline.errors[{index}]")
        _exact_fields(entry, ERROR_FIELDS, f"baseline entry fields at index {index}")
        rationale = entry.get("rationale")
        if not isinstance(rationale, str) or not rationale.strip():
            raise ValidationError(f"baseline.errors[{index}].rationale must not be blank")
        signatures.append(
            _signature(
                {key: entry[key] for key in ERROR_FIELDS - {"rationale"}},
                f"baseline.errors[{index}]",
            )
        )

    canonical = [_canonical(signature) for signature in signatures]
    duplicates = [item for item, count in Counter(canonical).items() if count > 1]
    if duplicates:
        raise ValidationError(f"baseline contains duplicate errors: {duplicates}")
    return version, signatures


def required_scan_paths(repository_root: Path) -> set[str]:
    if not repository_root.is_dir():
        raise ValidationError(f"repository root does not exist: {repository_root}")

    candidates = [repository_root / "Cargo.toml", repository_root / "build.rs"]
    candidates.extend((repository_root / "src").rglob("*.rs"))
    candidates.append(repository_root / ".github" / "dependabot.yml")
    for directory in (
        repository_root / ".github" / "workflows",
        repository_root / ".github" / "actions",
        repository_root / "scripts",
    ):
        if directory.is_dir():
            candidates.extend(
                path
                for path in directory.rglob("*")
                if path.is_file() and path.suffix in SCANNED_CODE_SUFFIXES
            )

    return {
        path.relative_to(repository_root).as_posix()
        for path in candidates
        if path.is_file()
    }


def validate_report(
    report: object,
    baseline: object,
    *,
    required_paths: set[str],
) -> dict[str, int]:
    document = _mapping(report, "Semgrep report")
    version, reviewed_errors = _baseline_errors(baseline)
    if document.get("version") != version:
        raise ValidationError(
            f"Semgrep version differs: expected {version}, got {document.get('version')!r}"
        )

    results = document.get("results")
    if not isinstance(results, list):
        raise ValidationError("Semgrep report.results must be a list")
    if results:
        finding_ids = sorted(
            str(result.get("check_id", "unknown"))
            for result in results
            if isinstance(result, dict)
        )
        raise ValidationError(f"Semgrep findings are not allowed: {finding_ids}")

    errors = document.get("errors")
    if not isinstance(errors, list):
        raise ValidationError("Semgrep report.errors must be a list")
    actual = Counter(_canonical(normalize_semgrep_error(error)) for error in errors)
    expected = Counter(_canonical(error) for error in reviewed_errors)
    unexpected = sorted((actual - expected).elements())
    missing = sorted((expected - actual).elements())
    if unexpected or missing:
        details = []
        if unexpected:
            details.append(f"unexpected errors={unexpected}")
        if missing:
            details.append(f"missing reviewed errors={missing}")
        raise ValidationError("; ".join(details))

    paths = _mapping(document.get("paths"), "Semgrep report.paths")
    scanned = paths.get("scanned")
    if not isinstance(scanned, list) or not all(isinstance(path, str) for path in scanned):
        raise ValidationError("Semgrep report.paths.scanned must be a list of strings")
    missing_paths = sorted(required_paths - set(scanned))
    if missing_paths:
        raise ValidationError(f"required paths were not scanned: {missing_paths}")

    return {"errors": len(errors), "scanned": len(scanned)}


def _load_json(path: Path, label: str) -> object:
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise ValidationError(f"could not read {label} {path}: {error}") from error


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="Validate a Semgrep JSON report")
    parser.add_argument("--report", type=Path, required=True)
    parser.add_argument("--baseline", type=Path, required=True)
    parser.add_argument("--repository-root", type=Path, required=True)
    args = parser.parse_args(argv)

    try:
        report = _load_json(args.report, "Semgrep report")
        baseline = _load_json(args.baseline, "Semgrep baseline")
        summary = validate_report(
            report,
            baseline,
            required_paths=required_scan_paths(args.repository_root),
        )
    except ValidationError as error:
        print(f"Semgrep report validation failed: {error}", file=sys.stderr)
        return 1

    print(
        f"Semgrep report validated: {summary['scanned']} paths scanned, "
        f"{summary['errors']} reviewed parser warnings"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
