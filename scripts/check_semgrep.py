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
    "source",
    "message",
    "rationale",
}
POSITION_FIELDS = {"line", "column"}
SCANNED_CODE_SUFFIXES = {".cjs", ".js", ".mjs", ".py", ".sh", ".ts", ".tsx", ".yaml", ".yml"}
REVIEW_REQUIRED = "REVIEW REQUIRED: explain why this warning is safe."


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

    for field in ("level", "type", "path", "source", "message"):
        if not isinstance(signature.get(field), str) or not signature[field]:
            raise ValidationError(f"{label}.{field} must be a non-empty string")
    if not isinstance(signature.get("code"), int):
        raise ValidationError(f"{label}.code must be an integer")

    return {
        "code": signature["code"],
        "level": signature["level"],
        "type": signature["type"],
        "path": signature["path"],
        "source": signature["source"],
        "message": signature["message"],
    }


def _source_line(repository_root: Path, relative_path: str, line: int) -> str:
    root = repository_root.resolve()
    try:
        source_path = (root / relative_path).resolve(strict=True)
        source_path.relative_to(root)
        lines = source_path.read_text(encoding="utf-8").splitlines()
    except (OSError, RuntimeError, ValueError) as error:
        raise ValidationError(
            f"could not read Semgrep error source {relative_path!r}: {error}"
        ) from error
    if line > len(lines):
        raise ValidationError(
            f"Semgrep error line {line} exceeds {relative_path!r} length {len(lines)}"
        )
    return lines[line - 1]


def _stable_message(message: str, path: str, line: int) -> str:
    location = f"{path}:{line}:"
    return message.replace(location, f"{path}:<LINE>:", 1)


def normalize_semgrep_error(
    value: object,
    repository_root: Path,
) -> dict[str, object]:
    error = _mapping(value, "Semgrep error")
    error_type = error.get("type")
    if not isinstance(error_type, list) or not error_type or not isinstance(error_type[0], str):
        raise ValidationError("Semgrep error type must contain a named error kind")

    spans = error.get("spans")
    if not isinstance(spans, list) or not spans:
        raise ValidationError("Semgrep error must contain at least one source span")
    span = _mapping(spans[0], "Semgrep error span")
    raw_start = _mapping(span.get("start"), "Semgrep error span start")
    raw_end = _mapping(span.get("end"), "Semgrep error span end")
    start = _position(
        {"line": raw_start.get("line"), "column": raw_start.get("col")},
        "Semgrep error span start",
    )
    _position(
        {"line": raw_end.get("line"), "column": raw_end.get("col")},
        "Semgrep error span end",
    )
    path = error.get("path")
    message = error.get("message")
    if not isinstance(path, str) or not path:
        raise ValidationError("Semgrep error path must be a non-empty string")
    if not isinstance(message, str) or not message:
        raise ValidationError("Semgrep error message must be a non-empty string")

    normalized = {
        "code": error.get("code"),
        "level": error.get("level"),
        "type": error_type[0],
        "path": path,
        "source": _source_line(repository_root, path, start["line"]),
        "message": _stable_message(message, path, start["line"]),
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
        if rationale == REVIEW_REQUIRED:
            raise ValidationError(f"baseline.errors[{index}] still requires review")
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


def _validate_scan_health(document: dict[str, Any]) -> int:
    engine = document.get("engine_requested")
    if engine != "OSS":
        raise ValidationError(f"Semgrep engine differs: expected 'OSS', got {engine!r}")

    skipped_rules = document.get("skipped_rules")
    if not isinstance(skipped_rules, list):
        raise ValidationError("Semgrep report.skipped_rules must be a list")
    if skipped_rules:
        raise ValidationError(f"Semgrep scan has {len(skipped_rules)} skipped rules")

    timing = _mapping(document.get("time"), "Semgrep report.time")
    fixpoint_timeouts = timing.get("fixpoint_timeouts")
    if not isinstance(fixpoint_timeouts, list):
        raise ValidationError("Semgrep report.time.fixpoint_timeouts must be a list")
    if fixpoint_timeouts:
        raise ValidationError(
            f"Semgrep scan has {len(fixpoint_timeouts)} fixpoint timeouts"
        )
    return len(skipped_rules)


def _validate_findings(document: dict[str, Any]) -> None:
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


def _validate_scanned_paths(
    document: dict[str, Any],
    required_paths: set[str],
) -> int:
    paths = _mapping(document.get("paths"), "Semgrep report.paths")
    scanned = paths.get("scanned")
    if not isinstance(scanned, list) or not all(isinstance(path, str) for path in scanned):
        raise ValidationError("Semgrep report.paths.scanned must be a list of strings")
    missing_paths = sorted(required_paths - set(scanned))
    if missing_paths:
        raise ValidationError(f"required paths were not scanned: {missing_paths}")
    return len(scanned)


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
    repository_root: Path,
) -> dict[str, int]:
    document = _mapping(report, "Semgrep report")
    version, reviewed_errors = _baseline_errors(baseline)
    if document.get("version") != version:
        raise ValidationError(
            f"Semgrep version differs: expected {version}, got {document.get('version')!r}"
        )

    skipped_rules = _validate_scan_health(document)
    _validate_findings(document)

    errors = document.get("errors")
    if not isinstance(errors, list):
        raise ValidationError("Semgrep report.errors must be a list")
    actual = Counter(
        _canonical(normalize_semgrep_error(error, repository_root)) for error in errors
    )
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

    scanned = _validate_scanned_paths(document, required_paths)

    return {
        "errors": len(errors),
        "scanned": scanned,
        "skipped_rules": skipped_rules,
    }


def build_baseline(
    report: object,
    baseline: object,
    *,
    required_paths: set[str],
    repository_root: Path,
) -> dict[str, object]:
    document = _mapping(report, "Semgrep report")
    version = document.get("version")
    if not isinstance(version, str) or not version:
        raise ValidationError("Semgrep report.version must be a non-empty string")
    _validate_scan_health(document)
    _validate_findings(document)
    _validate_scanned_paths(document, required_paths)

    errors = document.get("errors")
    if not isinstance(errors, list):
        raise ValidationError("Semgrep report.errors must be a list")
    normalized = [normalize_semgrep_error(error, repository_root) for error in errors]
    canonical = [_canonical(error) for error in normalized]
    duplicates = [item for item, count in Counter(canonical).items() if count > 1]
    if duplicates:
        raise ValidationError(f"Semgrep report contains duplicate errors: {duplicates}")

    _, reviewed = _baseline_errors(baseline)
    document_errors = _mapping(baseline, "baseline").get("errors")
    if not isinstance(document_errors, list):
        raise ValidationError("baseline.errors must be a list")
    rationales = {
        _canonical(signature): _mapping(
            document_errors[index], f"baseline.errors[{index}]"
        )["rationale"]
        for index, signature in enumerate(reviewed)
    }
    reviewed_order = {
        _canonical(signature): index for index, signature in enumerate(reviewed)
    }

    def baseline_order(error: dict[str, object]) -> tuple[int, int | str]:
        canonical = _canonical(error)
        if canonical in reviewed_order:
            return (0, reviewed_order[canonical])
        return (1, canonical)

    entries = [
        {
            **error,
            "rationale": rationales.get(_canonical(error), REVIEW_REQUIRED),
        }
        for error in sorted(normalized, key=baseline_order)
    ]
    return {"schemaVersion": 1, "semgrepVersion": version, "errors": entries}


def _write_json(path: Path, value: object) -> None:
    try:
        path.write_text(f"{json.dumps(value, indent=2)}\n", encoding="utf-8")
    except OSError as error:
        raise ValidationError(f"could not write Semgrep baseline {path}: {error}") from error


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
    parser.add_argument(
        "--write-baseline",
        type=Path,
        help="write a reviewed baseline candidate instead of validating the report",
    )
    args = parser.parse_args(argv)

    try:
        report = _load_json(args.report, "Semgrep report")
        baseline = _load_json(args.baseline, "Semgrep baseline")
        required_paths = required_scan_paths(args.repository_root)
        if args.write_baseline is not None:
            generated = build_baseline(
                report,
                baseline,
                required_paths=required_paths,
                repository_root=args.repository_root,
            )
            _write_json(args.write_baseline, generated)
            generated_errors = generated.get("errors")
            if not isinstance(generated_errors, list):
                raise ValidationError("generated baseline.errors must be a list")
            requires_review = any(
                _mapping(entry, "generated baseline entry").get("rationale")
                == REVIEW_REQUIRED
                for entry in generated_errors
            )
            if requires_review:
                raise ValidationError(
                    f"wrote {args.write_baseline}; replace every {REVIEW_REQUIRED!r} rationale"
                )
            print(f"Semgrep baseline candidate written to {args.write_baseline}")
            return 0
        summary = validate_report(
            report,
            baseline,
            required_paths=required_paths,
            repository_root=args.repository_root,
        )
    except ValidationError as error:
        print(f"Semgrep report validation failed: {error}", file=sys.stderr)
        return 1

    print(
        f"Semgrep report validated: {summary['scanned']} paths scanned, "
        f"{summary['errors']} reviewed parser warnings, "
        f"{summary['skipped_rules']} skipped rules"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
