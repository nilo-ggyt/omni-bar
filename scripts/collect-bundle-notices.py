#!/usr/bin/env python3
"""Collect exact locked-source notices for the native normal/build dependency closure, offline."""

from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
import re
import subprocess


ROOT = Path(__file__).resolve().parent.parent
TARGET = "aarch64-apple-darwin"
SAFE_COMPONENT = re.compile(r"[A-Za-z0-9_.+-]+\Z")
NOTICE_PREFIXES = ("LICENSE", "COPYING", "NOTICE", "COPYRIGHT")


def command(*arguments: str) -> str:
    """Read declared tool output without a shell, implicit network access or credential output."""
    # Any failed locked/toolchain query aborts packaging rather than producing incomplete attribution.
    return subprocess.run(arguments, cwd=ROOT, check=True, capture_output=True, text=True).stdout


def closure(metadata: dict) -> list[dict]:
    """Select native production/build inputs; unrelated comparison members and dev fixtures stay out."""
    packages = {package["id"]: package for package in metadata["packages"]}
    roots = [package["id"] for package in packages.values() if package["name"] == "omni-native-runtime"]
    if len(roots) != 1:
        # An ambiguous or missing application root cannot define a reproducible notice inventory.
        raise ValueError("expected exactly one native application package")
    edges = {}
    for node in metadata["resolve"]["nodes"]:
        children = set()
        for dependency in node["deps"]:
            # A future or missing edge classification cannot silently omit a potentially delivered input.
            kinds = {kind["kind"] for kind in dependency["dep_kinds"]}
            if not kinds or kinds - {None, "build", "dev"}:
                # Updating the pinned toolchain requires reviewing unfamiliar dependency classifications.
                raise ValueError("unsupported Cargo dependency kind")
            if kinds & {None, "build"}:
                # Include build scripts/macros conservatively even when their code is not shipped at runtime.
                children.add(dependency["pkg"])
        edges[node["id"]] = children
    visited = set()
    pending = roots[:]
    while pending:
        # Opaque Cargo IDs identify packages; cycles/shared dependencies are visited only once.
        identifier = pending.pop()
        if identifier in visited:
            # A shared transitive input needs one notice directory rather than repeated traversal.
            continue
        visited.add(identifier)
        pending.extend(edges[identifier] - visited)
    return sorted((packages[identifier] for identifier in visited), key=lambda p: (p["name"], p["version"]))


def checked_text(path: Path, owner: Path) -> bytes:
    """Read a finite UTF-8 notice within its declared source owner, rejecting outbound symlinks."""
    resolved = path.resolve(strict=True)
    owner = owner.resolve(strict=True)
    if resolved != owner and owner not in resolved.parents:
        # A source notice must not turn into a read of private files outside the dependency checkout.
        raise ValueError("notice resolves outside its source owner")
    if not resolved.is_file() or resolved.stat().st_size > 2_000_000:
        # Copyright documents are small text inputs; a binary/oversized replacement needs review.
        raise ValueError("notice is not a bounded regular text file")
    data = resolved.read_bytes()
    data.decode("utf-8")
    if b"\0" in data:
        # Valid UTF-8 bytes alone do not make an embedded-NUL binary a reviewable license document.
        raise ValueError("notice contains binary NUL bytes")
    return data


def source_owner(package: dict, source: Path) -> Path:
    """Locate a workspace notice owner without treating a Cargo registry parent as crate source."""
    if (package.get("source") or "").startswith("registry+"):
        # Registry archives are standalone roots; never read sibling packages or user directories.
        return source
    for candidate in (source, *tuple(source.parents)[:12]):
        if (candidate / ".git").exists() or (candidate / "Cargo.lock").is_file():
            # Local/Git workspace members may inherit their actual checkout-root copyright text.
            return candidate
    # A source without a recognizable workspace may still supply its own package-local notice.
    return source


def package_notices(package: dict) -> dict[str, bytes]:
    """Preserve upstream source notices, using only an explicit pinned exception for objc2 archives."""
    source = Path(package["manifest_path"]).parent
    owner = source_owner(package, source)
    repository = (package.get("repository") or "").rstrip("/").removesuffix(".git")
    notices = {}
    roots = [source] if source == owner else [source, owner]
    for notice_root in roots:
        # Preserve package-local and checkout-root notices, including future independently owned plugins.
        for path in sorted(notice_root.iterdir()):
            if path.is_file() and path.name.upper().startswith(NOTICE_PREFIXES):
                # Keep every attribution/exception text rather than selecting only one OR arm.
                data = checked_text(path, owner)
                name = path.name
                if name in notices and notices[name] != data:
                    # Distinct workspace/package notices with equal names must both remain available.
                    name = "WORKSPACE-" + name
                if name in notices and notices[name] != data:
                    # A conflicting preexisting prefixed filename also requires explicit source review.
                    raise ValueError("ambiguous workspace notice filename")
                notices[name] = data
    if package.get("license_file"):
        # Explicit Cargo license-file metadata is authoritative and remains confined to its owner.
        path = Path(package["license_file"])
        if not path.is_absolute():
            # Older metadata may retain the package-relative spelling of an explicit license file.
            path = source / path
        data = checked_text(path, owner)
        name = path.name
        if name in notices and notices[name] != data:
            # A nested explicit license file must not overwrite a distinct root attribution document.
            name = "CARGO-LICENSE-FILE-" + name
        if name in notices and notices[name] != data:
            # Ambiguous source naming needs review rather than silently discarding either text.
            raise ValueError("ambiguous explicit license notice filename")
        notices[name] = data
    if notices:
        # A package's actual source texts include its copyright/permission notices unchanged.
        return notices
    vcs = source / ".cargo_vcs_info.json"
    if repository == "https://github.com/madsmtm/objc2" and vcs.is_file():
        # These published crates omit the repository-root notice; bind the supplement to archive VCS identity.
        revision = json.loads(checked_text(vcs, source))["git"]["sha1"]
        supplement_root = ROOT / "licenses/objc2"
        records = json.loads(checked_text(supplement_root / "sources.json", supplement_root))
        record = next((record for record in records if record["revision"] == revision), None)
        if record is None:
            # A new upstream revision requires review; do not reuse an unrelated license snapshot.
            raise ValueError("objc2 source revision has no reviewed notice supplement")
        data = checked_text(supplement_root / record["file"], supplement_root)
        if hashlib.sha256(data).hexdigest() != record["sha256"]:
            # Provenance changes must be deliberate instead of silently altering binary attribution.
            raise ValueError("objc2 notice supplement digest mismatch")
        return {
            "UPSTREAM-LICENSE.md": data,
            "MIT-TERMS.txt": checked_text(ROOT / "licenses/MIT-terms.txt", ROOT),
        }
    # Missing upstream text remains a failing packaging obligation, not a synthesized copyright claim.
    raise ValueError(f"no reviewed notice texts for {package['name']} {package['version']}")


def collect(output: Path, manifest: Path = ROOT / "Cargo.toml") -> dict:
    """Preflight the actual build manifest's notices before writing an offline attribution tree."""
    if output.exists():
        # The caller owns cleanup of its generated bundle; never recursively delete an arbitrary argument.
        raise ValueError("notice output already exists; supply a new generated directory")
    # A caller may supply a distinct build manifest; notices must follow that exact dependency graph,
    # rather than attributing a different Git revision merely because the host root is the default.
    manifest = manifest.resolve(strict=True)
    if not manifest.is_file():
        # An invalid manifest cannot fall back to cwd and attach the wrong dependencies to a signed bundle.
        raise ValueError("build manifest is not a regular file")
    metadata = json.loads(command("cargo", "+1.99.0", "metadata", "--manifest-path", str(manifest),
                                  "--format-version", "1", "--locked", "--offline",
                                  "--filter-platform", TARGET))
    packages = closure(metadata)
    prepared = []
    components = set()
    for package in packages:
        # Validate output components before any mkdir or write can escape the generated directory.
        component = package["name"] + "-" + package["version"]
        if not SAFE_COMPONENT.fullmatch(component):
            # Unknown Cargo naming syntax requires a reviewed path policy rather than normalization.
            raise ValueError("unsupported package notice directory name")
        if component in components:
            # Equal names/versions from different sources need explicit attribution rather than overwrite.
            raise ValueError("ambiguous duplicate package notice directory")
        components.add(component)
        notices = package_notices(package)
        for name in notices:
            # Validate every metadata-derived filename before creating even a partial output directory.
            if Path(name).name != name or name in (".", ".."):
                # A notice filename is a single component, never a second destination chosen by metadata.
                raise ValueError("unsupported notice filename")
        prepared.append((package, component, notices))
    sysroot = Path(command("rustc", "+1.99.0", "--print", "sysroot").strip())
    # Include the installed compiler's complete standard-library notice inventory, not compiler/tool notices.
    rust_notice = checked_text(sysroot / "share/doc/rust/COPYRIGHT-library.html", sysroot)
    output.mkdir(parents=True)
    index = ["# Native application dependency notices", "",
             "Exact locked normal/build inputs for aarch64-apple-darwin. Build-only inputs are included",
             "conservatively; this list does not claim that each crate is linked into the binary.", "",
             "Project-owned source retains Copyright (c) 2026 nilo vance under MIT.", ""]
    inventory = []
    for package, component, notices in prepared:
        # Each exact version gets its unchanged source texts and explicit original metadata attribution.
        destination = output / component
        destination.mkdir()
        files = []
        for name, data in notices.items():
            # Preflighted single filenames stay inside this exact package's generated directory.
            (destination / name).write_bytes(data)
            files.append({"name": name, "sha256": hashlib.sha256(data).hexdigest()})
        index.extend([f"## {package['name']} {package['version']}", "",
                      f"Declared license: `{package.get('license') or 'license-file'}`.", "",
                      "Upstream authors (Cargo metadata): " + "; ".join(package.get("authors", [])), ""])
        for file in files:
            # Local links keep notices usable offline when viewing the installed application bundle.
            index.append(f"- [{file['name']}]({component}/{file['name']})")
        index.append("")
        inventory.append({"name": package["name"], "version": package["version"],
                          "license": package.get("license"), "source": package.get("source"),
                          "repository": package.get("repository"), "files": files})
    (output / "Rust-standard-library.html").write_bytes(rust_notice)
    index.extend(["## Rust 1.99.0 standard library", "",
                  "[Complete installed standard-library copyright/permission notices](Rust-standard-library.html)", ""])
    (output / "README.md").write_text("\n".join(index), encoding="utf-8")
    (output / "inventory.json").write_text(json.dumps(inventory, indent=2) + "\n", encoding="utf-8")
    return {"packages": len(packages), "notice_files": sum(len(item[2]) for item in prepared),
            "rust_library_notice_bytes": len(rust_notice)}


def main() -> None:
    """Create only explicitly requested generated output; help parses without Cargo/filesystem mutation."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--manifest-path", type=Path, default=ROOT / "Cargo.toml")
    arguments = parser.parse_args()
    # Manifest or notice preflight failure aborts packaging; partial attribution cannot receive a complete result.
    print(json.dumps(collect(arguments.output, arguments.manifest_path)))


if __name__ == "__main__":
    # Packaging invokes this after compilation; no download, license choice prompt or publication occurs.
    main()
