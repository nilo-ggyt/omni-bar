"""Exercise attribution completeness and source/output boundaries without Cargo or network access."""

from __future__ import annotations

import hashlib
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch


# Load the executable collector as a module; importing it must not create artifacts or run Cargo.
SPEC = importlib.util.spec_from_file_location(
    "bundle_notices", Path(__file__).resolve().parents[1] / "collect-bundle-notices.py"
)
assert SPEC is not None and SPEC.loader is not None
COLLECTOR = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(COLLECTOR)


def package(root: Path, name: str = "dependency", source: str | None = "registry+fixture") -> dict:
    """Describe an isolated fixture archive using only fields the production collector consumes."""
    # The source directory exists before any notice lookup and is removed with the test's temporary owner.
    root.mkdir(parents=True, exist_ok=True)
    return {"id": name, "name": name, "version": "1.0.0", "source": source,
            "manifest_path": str(root / "Cargo.toml"), "repository": None,
            "license": "MIT", "license_file": None, "authors": ["Fixture author"]}


def graph(packages: list[dict], edges: dict[str, list[tuple[str, str | None]]]) -> dict:
    """Build a minimal Cargo graph with explicit edge kinds, including shared/cyclic cases."""
    # Keep each declared dependency category visible so a dev edge cannot masquerade as production.
    nodes = [{"id": item["id"], "deps": [
        {"pkg": target, "dep_kinds": [{"kind": kind}]} for target, kind in edges.get(item["id"], [])
    ]} for item in packages]
    return {"packages": packages, "resolve": {"nodes": nodes}}


class BundleNoticeTests(unittest.TestCase):
    """Require exact production attribution and fail before writing an incomplete notice bundle."""

    def test_normal_build_closure_excludes_dev_and_unrelated_roots(self) -> None:
        """Only inputs reachable from the native application define its dependency attribution."""
        with tempfile.TemporaryDirectory() as directory:
            # A cycle and a shared input must terminate once, while a dev-only dependency stays excluded.
            root = Path(directory)
            names = ("omni-native-runtime", "runtime", "builder", "shared", "fixture", "comparison")
            packages = [package(root / name, name) for name in names]
            metadata = graph(packages, {
                "omni-native-runtime": [("runtime", None), ("builder", "build"), ("fixture", "dev")],
                "runtime": [("shared", None)], "builder": [("shared", None)],
                "shared": [("runtime", None)], "comparison": [("fixture", None)],
            })
            selected = COLLECTOR.closure(metadata)
            self.assertEqual({item["name"] for item in selected}, set(names[:4]))
            self.assertEqual(len(selected), 4)

    def test_unknown_dependency_kind_fails_closed(self) -> None:
        """An unfamiliar toolchain graph cannot silently omit a potentially delivered dependency."""
        with tempfile.TemporaryDirectory() as directory:
            # A synthetic future edge category must produce a review failure before traversal.
            root = Path(directory)
            packages = [package(root / "app", "omni-native-runtime"), package(root / "other")]
            metadata = graph(packages, {"omni-native-runtime": [("dependency", "future-kind")]})
            with self.assertRaisesRegex(ValueError, "dependency kind"):
                COLLECTOR.closure(metadata)

    def test_outbound_notice_symlink_is_not_read(self) -> None:
        """A dependency may not attribute itself using a private file outside its source owner."""
        with tempfile.TemporaryDirectory() as directory:
            # Use a harmless sibling fixture to prove rejection without reading personal filesystem data.
            root = Path(directory)
            item = package(root / "archive")
            outside = root / "private.txt"
            outside.write_text("Fixture private content", encoding="utf-8")
            (root / "archive/LICENSE").symlink_to(outside)
            with self.assertRaisesRegex(ValueError, "outside"):
                COLLECTOR.package_notices(item)

    def test_binary_oversized_and_invalid_text_are_rejected(self) -> None:
        """A filename resembling a license is insufficient when its contents are not bounded text."""
        with tempfile.TemporaryDirectory() as directory:
            # Each replacement exercises a different invalid input without allocating an unbounded fixture.
            root = Path(directory)
            notice = root / "LICENSE"
            for data, error in ((b"MIT\0binary", ValueError), (b"\xff", UnicodeDecodeError),
                                (b"x" * 2_000_001, ValueError)):
                with self.subTest(size=len(data), error=error.__name__):
                    # Failed validation returns no bytes to the generated application's attribution writer.
                    notice.write_bytes(data)
                    with self.assertRaises(error):
                        COLLECTOR.checked_text(notice, root)

    def test_workspace_and_explicit_notices_preserve_distinct_texts(self) -> None:
        """Future plugins may inherit workspace attribution and declare an additional nested license."""
        with tempfile.TemporaryDirectory() as directory:
            # Three equal basenames carry different authorship; none may overwrite the others.
            root = Path(directory)
            item = package(root / "crates/plugin", "future-plugin", None)
            (root / "Cargo.lock").write_text("fixture", encoding="utf-8")
            (root / "LICENSE").write_text("Workspace attribution", encoding="utf-8")
            (root / "crates/plugin/LICENSE").write_text("Package attribution", encoding="utf-8")
            nested = root / "crates/plugin/legal/LICENSE"
            nested.parent.mkdir()
            nested.write_text("Explicit attribution", encoding="utf-8")
            item["license_file"] = "legal/LICENSE"
            notices = COLLECTOR.package_notices(item)
            self.assertEqual(set(notices.values()), {
                b"Workspace attribution", b"Package attribution", b"Explicit attribution"
            })

    def test_missing_and_unreviewed_objc_notice_fail(self) -> None:
        """Missing upstream text cannot become invented attribution or an unpinned supplement."""
        with tempfile.TemporaryDirectory() as directory:
            # An ordinary missing notice fails independently of the explicit objc2 archive exception.
            root = Path(directory)
            item = package(root / "archive")
            with self.assertRaisesRegex(ValueError, "no reviewed notice"):
                COLLECTOR.package_notices(item)
            item["repository"] = "https://github.com/madsmtm/objc2"
            (root / "archive/.cargo_vcs_info.json").write_text(
                json.dumps({"git": {"sha1": "unreviewed-revision"}}), encoding="utf-8"
            )
            # A new upstream revision must be reviewed before an old supplement can be reused.
            with self.assertRaisesRegex(ValueError, "revision"):
                COLLECTOR.package_notices(item)

    def test_reviewed_objc_supplement_checks_content_digest(self) -> None:
        """Recorded provenance only authorizes the exact upstream text that was actually reviewed."""
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            item = package(root / "archive")
            item["repository"] = "https://github.com/madsmtm/objc2"
            (root / "archive/.cargo_vcs_info.json").write_text(
                json.dumps({"git": {"sha1": "reviewed"}}), encoding="utf-8"
            )
            # Supply a small synthetic reviewed notice, then replace its bytes after recording its hash.
            supplement = root / "licenses/objc2"
            supplement.mkdir(parents=True)
            notice = b"Reviewed upstream attribution"
            (supplement / "LICENSE.md").write_bytes(notice)
            (root / "licenses/MIT-terms.txt").write_bytes(b"Fixture MIT permission terms")
            (supplement / "sources.json").write_text(json.dumps([{
                "revision": "reviewed", "file": "LICENSE.md", "sha256": hashlib.sha256(notice).hexdigest()
            }]), encoding="utf-8")
            with patch.object(COLLECTOR, "ROOT", root):
                self.assertEqual(COLLECTOR.package_notices(item)["UPSTREAM-LICENSE.md"], notice)
                (supplement / "LICENSE.md").write_bytes(b"Unreviewed replacement")
                with self.assertRaisesRegex(ValueError, "digest"):
                    COLLECTOR.package_notices(item)

    def test_missing_notice_leaves_no_partial_output(self) -> None:
        """A failed preflight must not leave a notice directory that packaging could mistake for complete."""
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            item = package(root / "app", "omni-native-runtime")
            metadata = graph([item], {})
            output = root / "bundle/ThirdPartyNotices"
            with patch.object(COLLECTOR, "command", return_value=json.dumps(metadata)):
                with self.assertRaisesRegex(ValueError, "no reviewed notice"):
                    COLLECTOR.collect(output)
            self.assertFalse(output.exists())

    def test_existing_output_is_preserved_without_tool_invocation(self) -> None:
        """An accidental existing destination never authorizes deletion or overwriting unrelated data."""
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            sentinel = root / "keep.txt"
            sentinel.write_bytes(b"Preserve existing content")
            with patch.object(COLLECTOR, "command") as command:
                with self.assertRaisesRegex(ValueError, "already exists"):
                    COLLECTOR.collect(root)
                command.assert_not_called()
            self.assertEqual(sentinel.read_bytes(), b"Preserve existing content")


if __name__ == "__main__":
    # The fixtures create only owned temporary directories and perform no publication or network access.
    unittest.main()
