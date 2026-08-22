#!/usr/bin/env python3
from __future__ import annotations

import argparse
import errno
import hashlib
import json
import os
import stat
import sys
import tempfile
from pathlib import Path
from typing import Any


MANIFEST_FIELDS = {
    "schemaVersion",
    "semgrepVersion",
    "license",
    "licenseUrl",
    "packs",
}
PACK_FIELDS = {"id", "url", "file", "canonicalSha256", "bytes", "rules"}
LICENSE = "Semgrep Rules License v1.0"
LICENSE_URL = "https://semgrep.dev/legal/rules-license/"
SEMGREP_VERSION = "1.173.0"
MAX_PACK_BYTES = 8 * 1024 * 1024
PACK_SPECS = {
    "p/default": {
        "url": "https://semgrep.dev/c/p/default",
        "file": "default.yml",
    },
    "p/security-audit": {
        "url": "https://semgrep.dev/c/p/security-audit",
        "file": "security-audit.yml",
    },
}


class PackError(ValueError):
    pass


def _mapping(value: object, label: str) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise PackError(f"{label} must be a JSON object")
    return value


def _exact_fields(value: dict[str, Any], expected: set[str], label: str) -> None:
    actual = set(value)
    if actual != expected:
        raise PackError(
            f"{label} fields differ: missing={sorted(expected - actual)}, "
            f"unknown={sorted(actual - expected)}"
        )


def validate_manifest(value: object) -> dict[str, object]:
    manifest = _mapping(value, "Semgrep pack manifest")
    _exact_fields(manifest, MANIFEST_FIELDS, "Semgrep pack manifest")
    if type(manifest.get("schemaVersion")) is not int or manifest["schemaVersion"] != 1:
        raise PackError("Semgrep pack manifest.schemaVersion must be 1")
    version = manifest.get("semgrepVersion")
    if version != SEMGREP_VERSION:
        raise PackError(
            f"Semgrep pack manifest.semgrepVersion must be {SEMGREP_VERSION}"
        )
    if manifest.get("license") != LICENSE or manifest.get("licenseUrl") != LICENSE_URL:
        raise PackError("Semgrep pack manifest license metadata differs")

    packs = manifest.get("packs")
    if not isinstance(packs, list):
        raise PackError("Semgrep pack manifest.packs must be a list")
    normalized: dict[str, dict[str, object]] = {}
    for index, value in enumerate(packs):
        pack = _mapping(value, f"Semgrep pack manifest.packs[{index}]")
        _exact_fields(pack, PACK_FIELDS, f"Semgrep pack manifest.packs[{index}]")
        pack_id = pack.get("id")
        if not isinstance(pack_id, str) or pack_id not in PACK_SPECS:
            raise PackError(f"unexpected Semgrep pack id: {pack_id!r}")
        if pack_id in normalized:
            raise PackError(f"duplicate Semgrep pack id: {pack_id}")
        spec = PACK_SPECS[pack_id]
        if pack.get("url") != spec["url"] or pack.get("file") != spec["file"]:
            raise PackError(f"Semgrep pack source differs for {pack_id}")
        digest = pack.get("canonicalSha256")
        if (
            not isinstance(digest, str)
            or len(digest) != 64
            or any(character not in "0123456789abcdef" for character in digest)
        ):
            raise PackError(f"Semgrep pack canonicalSha256 is invalid for {pack_id}")
        size = pack.get("bytes")
        if type(size) is not int or size < 1 or size > MAX_PACK_BYTES:
            raise PackError(f"Semgrep pack byte size is invalid for {pack_id}")
        rules = pack.get("rules")
        if type(rules) is not int or rules < 1:
            raise PackError(f"Semgrep pack rule count is invalid for {pack_id}")
        normalized[pack_id] = {
            "id": pack_id,
            "url": spec["url"],
            "file": spec["file"],
            "canonicalSha256": digest,
            "bytes": size,
            "rules": rules,
        }

    if set(normalized) != set(PACK_SPECS):
        raise PackError(
            f"Semgrep pack set differs: expected={sorted(PACK_SPECS)}, "
            f"actual={sorted(normalized)}"
        )
    return {
        "schemaVersion": 1,
        "semgrepVersion": SEMGREP_VERSION,
        "license": LICENSE,
        "licenseUrl": LICENSE_URL,
        "packs": [normalized[pack_id] for pack_id in PACK_SPECS],
    }


def _validate_pack_content(pack_id: str, content: bytes) -> tuple[str, int, int]:
    if not content.startswith(b"rules:\n"):
        raise PackError(f"Semgrep pack {pack_id} is not a rules YAML document")
    if len(content) > MAX_PACK_BYTES:
        raise PackError(f"Semgrep pack {pack_id} exceeds {MAX_PACK_BYTES} bytes")

    blocks: list[tuple[bytes, bytes]] = []
    current_id: bytes | None = None
    current_lines: list[bytes] = []
    for line in content.splitlines(keepends=True)[1:]:
        if line.startswith(b"- id: "):
            if current_id is not None:
                blocks.append((current_id, b"".join(current_lines)))
            current_id = line.removeprefix(b"- id: ").strip()
            if not current_id:
                raise PackError(f"Semgrep pack {pack_id} contains a blank rule id")
            current_lines = [line]
        elif current_id is None:
            if line.strip():
                raise PackError(
                    f"Semgrep pack {pack_id} has content before its first rule"
                )
        else:
            current_lines.append(line)
    if current_id is not None:
        blocks.append((current_id, b"".join(current_lines)))
    if not blocks:
        raise PackError(f"Semgrep pack {pack_id} does not contain rules")

    rule_ids = [rule_id for rule_id, _ in blocks]
    if len(set(rule_ids)) != len(rule_ids):
        raise PackError(f"Semgrep pack {pack_id} contains duplicate rule ids")
    canonical = b"rules:\n" + b"".join(
        block for _, block in sorted(blocks, key=lambda item: item[0])
    )
    return hashlib.sha256(canonical).hexdigest(), len(content), len(blocks)


def _read_pack(path: Path) -> bytes:
    descriptor: int | None = None
    try:
        metadata = path.stat(follow_symlinks=False)
        if not stat.S_ISREG(metadata.st_mode):
            raise PackError(f"Semgrep pack input is not a regular file: {path}")
        if metadata.st_size > MAX_PACK_BYTES:
            raise PackError(f"Semgrep pack input exceeds {MAX_PACK_BYTES} bytes: {path}")
        flags = os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0)
        descriptor = os.open(path, flags)
        opened = os.fstat(descriptor)
        if not stat.S_ISREG(opened.st_mode):
            raise PackError(f"Semgrep pack input is not a regular file: {path}")
        if (opened.st_dev, opened.st_ino) != (metadata.st_dev, metadata.st_ino):
            raise PackError(f"Semgrep pack input changed while opening: {path}")
        if opened.st_size > MAX_PACK_BYTES:
            raise PackError(f"Semgrep pack input exceeds {MAX_PACK_BYTES} bytes: {path}")
        with os.fdopen(descriptor, "rb", closefd=True) as handle:
            descriptor = None
            return handle.read(MAX_PACK_BYTES + 1)
    except OSError as error:
        if error.errno == errno.ELOOP:
            raise PackError(f"Semgrep pack input is not a regular file: {path}") from error
        raise PackError(f"could not read Semgrep pack input {path}: {error}") from error
    finally:
        if descriptor is not None:
            os.close(descriptor)


def _atomic_write(path: Path, content: bytes) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary_path: Path | None = None
    try:
        with tempfile.NamedTemporaryFile(dir=path.parent, delete=False) as temporary:
            temporary_path = Path(temporary.name)
            temporary.write(content)
            temporary.flush()
            os.fsync(temporary.fileno())
        os.replace(temporary_path, path)
    except OSError as error:
        if temporary_path is not None:
            temporary_path.unlink(missing_ok=True)
        raise PackError(f"could not write {path}: {error}") from error


def verify_packs(manifest: object, input_directory: Path) -> list[Path]:
    normalized = validate_manifest(manifest)
    verified = []
    for value in normalized["packs"]:
        pack = _mapping(value, "normalized Semgrep pack")
        pack_id = str(pack["id"])
        path = input_directory / str(pack["file"])
        content = _read_pack(path)
        digest, size, rules = _validate_pack_content(pack_id, content)
        if (
            digest != pack["canonicalSha256"]
            or size != pack["bytes"]
            or rules != pack["rules"]
        ):
            raise PackError(
                f"Semgrep pack integrity differs for {pack_id}: "
                f"expected canonicalSha256={pack['canonicalSha256']} "
                f"bytes={pack['bytes']} rules={pack['rules']}, "
                f"got canonicalSha256={digest} bytes={size} rules={rules}"
            )
        verified.append(path)
    return verified


def refresh_manifest(manifest: object, input_directory: Path) -> dict[str, object]:
    normalized = validate_manifest(manifest)
    refreshed = []
    for value in normalized["packs"]:
        pack = _mapping(value, "normalized Semgrep pack")
        pack_id = str(pack["id"])
        content = _read_pack(input_directory / str(pack["file"]))
        digest, size, rules = _validate_pack_content(pack_id, content)
        refreshed.append(
            {**pack, "canonicalSha256": digest, "bytes": size, "rules": rules}
        )
    return {**normalized, "packs": refreshed}


def _load_json(path: Path) -> object:
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise PackError(f"could not read Semgrep pack manifest {path}: {error}") from error


def _write_json(path: Path, value: object) -> None:
    _atomic_write(path, f"{json.dumps(value, indent=2)}\n".encode())


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="Verify hash-pinned Semgrep rule packs")
    subparsers = parser.add_subparsers(dest="command", required=True)
    verify = subparsers.add_parser("verify", help="verify downloaded packs")
    verify.add_argument("--manifest", type=Path, required=True)
    verify.add_argument("--input-dir", type=Path, required=True)
    update = subparsers.add_parser("update", help="refresh pack hashes and sizes")
    update.add_argument("--manifest", type=Path, required=True)
    update.add_argument("--input-dir", type=Path, required=True)
    args = parser.parse_args(argv)

    try:
        manifest = _load_json(args.manifest)
        if args.command == "verify":
            verified = verify_packs(manifest, args.input_dir)
            print(f"Verified {len(verified)} Semgrep rule packs")
        else:
            refreshed = refresh_manifest(manifest, args.input_dir)
            if refreshed == validate_manifest(manifest):
                print("Semgrep rule pack hashes are current")
            else:
                _write_json(args.manifest, refreshed)
                print("Updated Semgrep rule pack hashes")
    except PackError as error:
        print(f"Semgrep pack operation failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
