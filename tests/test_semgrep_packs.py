from __future__ import annotations

import importlib.util
import io
import json
import tempfile
import unittest
from contextlib import redirect_stderr
from pathlib import Path
from unittest import mock


REPOSITORY_ROOT = Path(__file__).resolve().parents[1]
SCRIPT_PATH = REPOSITORY_ROOT / "scripts" / "semgrep_packs.py"
SPEC = importlib.util.spec_from_file_location("semgrep_packs", SCRIPT_PATH)
if SPEC is None or SPEC.loader is None:
    raise RuntimeError("could not load Semgrep pack helper")
PACKS = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(PACKS)


CONTENTS = {
    "https://semgrep.dev/c/p/default": b"rules:\n- id: default.rule\n",
    "https://semgrep.dev/c/p/security-audit": b"rules:\n- id: audit.rule\n",
}


def manifest() -> dict[str, object]:
    packs = []
    for pack_id, spec in PACKS.PACK_SPECS.items():
        content = CONTENTS[spec["url"]]
        digest, size, rules = PACKS._validate_pack_content(pack_id, content)
        packs.append(
            {
                "id": pack_id,
                "url": spec["url"],
                "file": spec["file"],
                "canonicalSha256": digest,
                "bytes": size,
                "rules": rules,
            }
        )
    return {
        "schemaVersion": 1,
        "semgrepVersion": "1.173.0",
        "license": PACKS.LICENSE,
        "licenseUrl": PACKS.LICENSE_URL,
        "packs": packs,
    }


def write_packs(directory: Path, contents: dict[str, bytes] | None = None) -> None:
    selected = CONTENTS if contents is None else contents
    for spec in PACKS.PACK_SPECS.values():
        (directory / spec["file"]).write_bytes(selected[spec["url"]])


class SemgrepPackTests(unittest.TestCase):
    def test_committed_manifest_matches_the_closed_schema(self) -> None:
        committed = json.loads(
            (REPOSITORY_ROOT / ".semgrep" / "packs.lock.json").read_text(
                encoding="utf-8"
            )
        )

        normalized = PACKS.validate_manifest(committed)

        self.assertEqual(normalized, committed)

    def test_verifies_only_integrity_matched_regular_files(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            inputs = Path(directory)
            write_packs(inputs)

            verified = PACKS.verify_packs(manifest(), inputs)

            self.assertEqual(
                [path.name for path in verified],
                ["default.yml", "security-audit.yml"],
            )

    def test_rejects_integrity_drift(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            inputs = Path(directory)
            write_packs(inputs)
            (inputs / "security-audit.yml").write_bytes(
                CONTENTS["https://semgrep.dev/c/p/security-audit"] + b"# changed\n"
            )

            with self.assertRaisesRegex(PACKS.PackError, "integrity differs"):
                PACKS.verify_packs(manifest(), inputs)

    def test_rejects_size_and_rule_count_drift(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            inputs = Path(directory)
            write_packs(inputs)

            for field in ("bytes", "rules"):
                changed = manifest()
                changed["packs"][0][field] += 1
                with self.subTest(field=field):
                    with self.assertRaisesRegex(PACKS.PackError, "integrity differs"):
                        PACKS.verify_packs(changed, inputs)

    def test_rule_order_does_not_change_the_canonical_digest(self) -> None:
        first = b"rules:\n- id: b\n  pattern: b()\n- id: a\n  pattern: a()\n"
        second = b"rules:\n- id: a\n  pattern: a()\n- id: b\n  pattern: b()\n"

        first_digest, first_size, first_rules = PACKS._validate_pack_content(
            "p/default", first
        )
        second_digest, second_size, second_rules = PACKS._validate_pack_content(
            "p/default", second
        )

        self.assertEqual(first_digest, second_digest)
        self.assertEqual(first_size, second_size)
        self.assertEqual(first_rules, second_rules)

    def test_rejects_symlinked_pack_inputs(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            inputs = Path(directory)
            write_packs(inputs)
            default = inputs / "default.yml"
            target = inputs / "target.yml"
            default.rename(target)
            default.symlink_to(target)

            with self.assertRaisesRegex(PACKS.PackError, "not a regular file"):
                PACKS.verify_packs(manifest(), inputs)

    def test_rejects_pack_replaced_between_metadata_and_open(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            inputs = Path(directory)
            write_packs(inputs)
            default = inputs / "default.yml"
            original_open = PACKS.os.open

            def replace_then_open(path: Path, flags: int) -> int:
                selected = Path(path)
                selected.unlink()
                selected.write_bytes(b"rules:\n- id: replacement\n")
                return original_open(selected, flags)

            with mock.patch.object(
                PACKS.os, "open", side_effect=replace_then_open
            ), self.assertRaisesRegex(PACKS.PackError, "changed while opening"):
                PACKS.verify_packs(manifest(), inputs)

    def test_rejects_manifest_source_filename_and_schema_drift(self) -> None:
        mutants = []
        boolean_schema = manifest()
        boolean_schema["schemaVersion"] = True
        mutants.append(boolean_schema)
        changed_version = manifest()
        changed_version["semgrepVersion"] = "latest"
        mutants.append(changed_version)
        changed_url = manifest()
        changed_url["packs"][0]["url"] = "https://example.com/default.yml"
        mutants.append(changed_url)
        traversal = manifest()
        traversal["packs"][0]["file"] = "../default.yml"
        mutants.append(traversal)
        unknown = manifest()
        unknown["packs"][0]["accepted"] = True
        mutants.append(unknown)

        for mutant in mutants:
            with self.assertRaises(PACKS.PackError):
                PACKS.validate_manifest(mutant)

    def test_refresh_updates_only_hashes_and_sizes(self) -> None:
        changed = {**CONTENTS, "https://semgrep.dev/c/p/default": b"rules:\n- id: new\n"}
        with tempfile.TemporaryDirectory() as directory:
            inputs = Path(directory)
            write_packs(inputs, changed)

            refreshed = PACKS.refresh_manifest(manifest(), inputs)

            self.assertEqual(refreshed["license"], PACKS.LICENSE)
            self.assertEqual(
                refreshed["packs"][0]["bytes"],
                len(changed["https://semgrep.dev/c/p/default"]),
            )
            digest, _, rules = PACKS._validate_pack_content(
                "p/default", changed["https://semgrep.dev/c/p/default"]
            )
            self.assertEqual(refreshed["packs"][0]["canonicalSha256"], digest)
            self.assertEqual(refreshed["packs"][0]["rules"], rules)

    def test_atomic_write_removes_temporary_file_after_fsync_failure(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            target = root / "packs.lock.json"

            with mock.patch.object(PACKS.os, "fsync", side_effect=OSError("simulated")):
                with self.assertRaisesRegex(PACKS.PackError, "could not write"):
                    PACKS._atomic_write(target, b"{}\n")

            self.assertEqual(list(root.iterdir()), [])

    def test_rejects_non_rules_documents_and_oversized_packs(self) -> None:
        for content in (b"not-rules: []\n", b"rules:\n" + b"x" * PACKS.MAX_PACK_BYTES):
            with self.assertRaises(PACKS.PackError):
                PACKS._validate_pack_content("p/default", content)

    def test_rejects_duplicate_rule_ids(self) -> None:
        duplicate = b"rules:\n- id: duplicate\n  pattern: a()\n- id: duplicate\n  pattern: b()\n"

        with self.assertRaisesRegex(PACKS.PackError, "duplicate rule ids"):
            PACKS._validate_pack_content("p/default", duplicate)

    def test_cli_rejects_invalid_json_without_a_traceback(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            manifest_path = root / "packs.lock.json"
            manifest_path.write_text("not JSON", encoding="utf-8")
            stderr = io.StringIO()

            with redirect_stderr(stderr):
                result = PACKS.main(
                    [
                        "verify",
                        "--manifest",
                        str(manifest_path),
                        "--input-dir",
                        str(root / "packs"),
                    ]
                )

            self.assertEqual(result, 1)
            self.assertIn("could not read", stderr.getvalue())
            self.assertNotIn("Traceback", stderr.getvalue())

    def test_scheduled_workflow_detects_drift_without_write_permissions(self) -> None:
        workflow = (
            REPOSITORY_ROOT / ".github" / "workflows" / "update-semgrep-rules.yml"
        ).read_text(encoding="utf-8")

        self.assertIn("permissions:\n  contents: read", workflow)
        self.assertIn(
            "git diff --exit-code -- .semgrep/packs.lock.json",
            workflow,
        )
        self.assertNotIn("contents: write", workflow)
        self.assertNotIn("pull-requests: write", workflow)
        self.assertNotIn("create-pull-request", workflow)


if __name__ == "__main__":
    unittest.main()
