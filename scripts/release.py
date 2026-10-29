#!/usr/bin/env python3
"""Prepare a local SemVer release; never push or publish."""

from __future__ import annotations

import argparse
from dataclasses import dataclass
import os
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
VERSION_FILES = (
    Path("Cargo.toml"),
    Path("Cargo.lock"),
    Path("pyproject.toml"),
    Path("deploy/kubernetes-release/kustomization.yaml"),
    Path("deploy/kubernetes-public-release/kustomization.yaml"),
    Path("README.md"),
    Path("docs/container.md"),
    Path("docs/orchestrators.md"),
    Path("docs-site/index.html"),
)
MARKER = "brainwire-release-version"
VERSION_RE = re.compile(
    r"^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)"
    r"(?:-([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?$"
)


@dataclass(frozen=True)
class Version:
    major: int
    minor: int
    patch: int
    prerelease: str | None = None

    @classmethod
    def parse(cls, value: str) -> Version:
        match = VERSION_RE.fullmatch(value)
        if not match:
            raise ValueError(f"unsupported SemVer version: {value}")
        prerelease = match.group(4)
        if prerelease:
            for identifier in prerelease.split("."):
                if identifier.isdigit() and len(identifier) > 1 and identifier.startswith("0"):
                    raise ValueError(f"numeric prerelease identifier has a leading zero: {identifier}")
        return cls(int(match.group(1)), int(match.group(2)), int(match.group(3)), prerelease)

    def __str__(self) -> str:
        core = f"{self.major}.{self.minor}.{self.patch}"
        return f"{core}-{self.prerelease}" if self.prerelease else core


def choose_bump(messages: list[str]) -> str:
    if not messages:
        raise ValueError("no commits since the latest release tag; add a change before creating another version")
    if any(
        re.search(r"^[a-z]+(?:\([^)]*\))?!:", message, re.MULTILINE)
        or re.search(r"^BREAKING(?: CHANGE|-CHANGE):", message, re.MULTILINE)
        for message in messages
    ):
        return "major"
    if any(re.search(r"^feat(?:\([^)]*\))?:", message, re.MULTILINE) for message in messages):
        return "minor"
    # Every requested delivery gets a version; fixes, docs, CI and other changes
    # that do not declare a feature or breaking change use the patch lane.
    return "patch"


def next_version(current: Version, bump: str, channel: str | None = None) -> Version:
    if bump == "major":
        core = (current.major + 1, 0, 0)
    elif bump == "minor":
        core = (current.major, current.minor + 1, 0)
    elif bump == "patch":
        core = (current.major, current.minor, current.patch + 1)
    else:
        raise ValueError(f"unknown SemVer bump: {bump}")

    if channel:
        # Consecutive beta/RC builds of the same patch increment the prerelease
        # counter; feature/breaking commits start a new minor/major candidate.
        same_core_patch = (
            current.prerelease
            and bump == "patch"
            and current.prerelease.startswith(f"{channel}.")
        )
        if same_core_patch:
            core = (current.major, current.minor, current.patch)
            suffix = current.prerelease[len(channel) + 1 :]
            if suffix.isdigit():
                return Version(*core, f"{channel}.{int(suffix) + 1}")
        return Version(*core, f"{channel}.1")
    return Version(*core)


def git(*args: str, check: bool = True) -> str:
    result = subprocess.run(
        ["git", *args], cwd=ROOT, text=True, capture_output=True, check=False
    )
    if check and result.returncode:
        raise RuntimeError(result.stderr.strip() or result.stdout.strip())
    return result.stdout.strip()


def workspace_version() -> Version:
    manifest = (ROOT / "Cargo.toml").read_text(encoding="utf-8")
    package = re.search(r"(?ms)^\[workspace\.package\]\s*(.*?)(?=^\[|\Z)", manifest)
    version = re.search(r'(?m)^version\s*=\s*"([^"]+)"\s*$', package.group(1)) if package else None
    if not version:
        raise ValueError("Cargo.toml has no [workspace.package].version")
    return Version.parse(version.group(1))


def latest_release_tag(current: Version) -> str:
    tag = git("describe", "--tags", "--match", "v[0-9]*", "--abbrev=0")
    if tag != f"v{current}":
        raise RuntimeError(
            f"Cargo says {current}, but the nearest release tag is {tag}; reconcile versions first"
        )
    git("merge-base", "--is-ancestor", tag, "HEAD")
    return tag


def unreleased_messages(tag: str) -> list[str]:
    raw = git("log", "--format=%B%x00", f"{tag}..HEAD")
    return [message.strip() for message in raw.split("\x00") if message.strip()]


def python_version(version: Version) -> str:
    core = f"{version.major}.{version.minor}.{version.patch}"
    if version.prerelease is None:
        return core
    match = re.fullmatch(r"(beta|rc)\.(\d+)", version.prerelease)
    if match is None:
        raise ValueError("Python packaging supports beta.N and rc.N prereleases")
    channel = "b" if match.group(1) == "beta" else "rc"
    return f"{core}{channel}{match.group(2)}"


def replace_workspace_version(text: str, old: str, new: str) -> str:
    updated, count = re.subn(
        rf'(?m)^(version\s*=\s*"){re.escape(old)}("\s*)$',
        rf"\g<1>{new}\g<2>",
        text,
        count=1,
    )
    if count != 1:
        raise ValueError("could not find the workspace package version in Cargo.toml")
    return updated


def replace_lock_versions(text: str, old: str, new: str) -> str:
    names: set[str] = set()
    blocks = re.split(r"(?m)(?=^\[\[package\]\]$)", text)
    updated_blocks: list[str] = []
    for block in blocks:
        name = re.search(r'(?m)^name = "(brainwire-[^"]+)"$', block)
        if name:
            names.add(name.group(1))
            block, count = re.subn(
                rf'(?m)^(version = "){re.escape(old)}("\s*)$',
                rf"\g<1>{new}\g<2>",
                block,
                count=1,
            )
            if count != 1:
                raise ValueError(f"Cargo.lock has a stale version for {name.group(1)}")
        updated_blocks.append(block)
    if len(names) != 7:
        raise ValueError(f"expected seven Brainwire workspace crates in Cargo.lock, found {len(names)}")
    return "".join(updated_blocks)


def replace_marked_versions(text: str, old_tag: str, new_tag: str, path: Path) -> str:
    lines = text.splitlines(keepends=True)
    found = 0
    for index, line in enumerate(lines):
        if MARKER not in line:
            continue
        updated, count = re.subn(rf"(?<![A-Za-z0-9])v?{re.escape(old_tag[1:])}(?![0-9])", new_tag, line)
        if count != 1:
            raise ValueError(f"{path}:{index + 1} marker must identify exactly one current version")
        lines[index] = updated
        found += 1
    if found == 0:
        raise ValueError(f"no {MARKER} annotations found in {path}")
    return "".join(lines)


def updated_files(old: Version, new: Version) -> dict[Path, str]:
    old_tag, new_tag = f"v{old}", f"v{new}"
    result: dict[Path, str] = {}
    for relative in VERSION_FILES:
        path = ROOT / relative
        result[relative] = path.read_text(encoding="utf-8")

    result[Path("Cargo.toml")] = replace_workspace_version(
        result[Path("Cargo.toml")], str(old), str(new)
    )
    result[Path("Cargo.lock")] = replace_lock_versions(
        result[Path("Cargo.lock")], str(old), str(new)
    )
    result[Path("pyproject.toml")] = replace_workspace_version(
        result[Path("pyproject.toml")], python_version(old), python_version(new)
    )

    for kustomization in (
        Path("deploy/kubernetes-release/kustomization.yaml"),
        Path("deploy/kubernetes-public-release/kustomization.yaml"),
    ):
        result[kustomization], count = re.subn(
            rf"(?m)^(\s+newTag:\s*){re.escape(old_tag)}(\s*)$",
            rf"\g<1>{new_tag}\g<2>",
            result[kustomization],
            count=1,
        )
        if count != 1:
            raise ValueError(f"{kustomization} must pin exactly {old_tag}")

    for relative in (
        Path("README.md"),
        Path("docs/container.md"),
        Path("docs/orchestrators.md"),
        Path("docs-site/index.html"),
    ):
        result[relative] = replace_marked_versions(result[relative], old_tag, new_tag, relative)
    return result


def clean_worktree() -> bool:
    return not git("status", "--porcelain=v1")


def run_local_release_checks() -> None:
    env = os.environ.copy()
    target_dir = Path(env.get("CARGO_TARGET_DIR", "target"))
    commands = (
        ["cargo", "fmt", "--all", "--", "--check"],
        # Build binaries used by integration tests. Partial artifact-cache
        # restores may contain tests without their CARGO_BIN_EXE_* executables.
        ["cargo", "build", "--locked", "--offline", "--workspace", "--bins"],
        ["cargo", "test", "--workspace", "--locked", "--offline"],
        ["cargo", "clippy", "--workspace", "--all-targets", "--locked", "--offline", "--", "-D", "warnings"],
        [sys.executable, "-m", "unittest", "discover", "-s", "examples/python", "-p", "test_*.py"],
        [sys.executable, "-m", "unittest", "discover", "-s", "scripts", "-p", "test_*.py"],
        [
            sys.executable,
            "crates/server/tests/python_ingestion.py",
            "--binary",
            str(target_dir / "debug" / "brainwire"),
            "--rows",
            "100",
            "--batch-size",
            "25",
            "--query-mib",
            "32",
        ],
        [sys.executable, "scripts/check_docs_site.py"],
    )
    env.setdefault("CARGO_BUILD_JOBS", "2")
    env["CARGO_INCREMENTAL"] = "0"
    env["CARGO_PROFILE_DEV_DEBUG"] = "0"
    env["CARGO_PROFILE_TEST_DEBUG"] = "0"
    env["CXXFLAGS"] = "-g0"
    for command in commands:
        print("+", " ".join(command), flush=True)
        subprocess.run(command, cwd=ROOT, env=env, check=True)


def apply_release(files: dict[Path, str], new: Version) -> None:
    originals = {relative: (ROOT / relative).read_text(encoding="utf-8") for relative in files}
    commit_created = False
    try:
        for relative, text in files.items():
            (ROOT / relative).write_text(text, encoding="utf-8")
        run_local_release_checks()
        subprocess.run(["git", "add", *map(str, files)], cwd=ROOT, check=True)
        subprocess.run(
            ["git", "commit", "-m", f"chore(release): v{new}"], cwd=ROOT, check=True
        )
        commit_created = True
        subprocess.run(
            ["git", "tag", "-a", f"v{new}", "-m", f"Brainwire v{new}"],
            cwd=ROOT,
            check=True,
        )
    except Exception:
        # Preserve a completed release commit if local tag creation itself fails.
        if not commit_created:
            for relative, text in originals.items():
                (ROOT / relative).write_text(text, encoding="utf-8")
            subprocess.run(["git", "reset", "--", *map(str, files)], cwd=ROOT, check=False)
        raise


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bump", choices=("auto", "patch", "minor", "major"), default="auto")
    parser.add_argument("--prerelease", choices=("beta", "rc"))
    parser.add_argument("--apply", action="store_true", help="run local checks, create a release commit and tag; never push")
    args = parser.parse_args()

    try:
        current = workspace_version()
        base_tag = latest_release_tag(current)
        messages = unreleased_messages(base_tag)
        if not messages:
            raise RuntimeError(f"no changes have landed since {base_tag}; refusing an empty version bump")
        bump = choose_bump(messages) if args.bump == "auto" else args.bump
        new = next_version(current, bump, args.prerelease)
        if git("tag", "--list", f"v{new}"):
            raise RuntimeError(f"tag v{new} already exists; release tags are never moved")
        files = updated_files(current, new)
        print(f"Current: v{current}\nCommits since {base_tag}: {len(messages)}\nBump: {bump}\nNext: v{new}")
        print("Files to update:")
        for relative in files:
            print(f"  {relative}")
        if not args.apply:
            if not clean_worktree():
                print("Preview only: --apply will refuse until the working tree is clean.")
            else:
                print("Preview only: rerun with --apply to run local checks and create a local commit/tag.")
            return
        if not clean_worktree():
            raise RuntimeError("--apply requires a clean worktree; commit or stash unrelated changes first")
        if git("branch", "--show-current") != "main":
            raise RuntimeError("--apply creates releases from main only; switch to main first")
        apply_release(files, new)
        print(f"Created local release v{new}. Nothing was pushed or published.")
        print("When you choose to publish to the still-private remote: git push origin main --follow-tags")
    except (OSError, ValueError, RuntimeError, subprocess.CalledProcessError) as error:
        raise SystemExit(f"release preparation failed: {error}") from error


if __name__ == "__main__":
    main()
