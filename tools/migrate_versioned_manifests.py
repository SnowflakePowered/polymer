#!/usr/bin/env python3
"""Move legacy versioned-volume identity from .manifest.db into Git history."""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import sys
import uuid


VOLUME_REF = "refs/snowglass/volume/v1"
VERSIONED_COLLECTIONS = ("saverepo", "memorycards")


def git(volume: Path, *args: str, input_bytes: bytes | None = None) -> bytes:
    process = subprocess.run(
        ["git", "-C", str(volume), *args],
        input=input_bytes,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    if process.returncode:
        detail = process.stderr.decode(errors="replace").strip()
        raise RuntimeError(f"git {' '.join(args)} failed for {volume}: {detail}")
    return process.stdout


def volumes(path: Path) -> list[Path]:
    if (path / ".manifest.db").is_file():
        return [path]
    found: list[Path] = []
    for collection in VERSIONED_COLLECTIONS:
        root = path / collection
        if root.is_dir():
            found.extend(database.parent for database in root.rglob(".manifest.db"))
    return found


def identity(volume: Path) -> tuple[str, str, str | None, int] | None:
    database = volume / ".manifest.db"
    connection = sqlite3.connect(f"file:{database.as_posix()}?mode=ro", uri=True)
    try:
        row = connection.execute(
            "SELECT id, platform, label, created FROM nand LIMIT 1"
        ).fetchone()
    finally:
        connection.close()
    if row is None:
        return None
    volume_id, platform, label, created = row
    volume_id = str(uuid.UUID(volume_id))
    return volume_id, str(platform), None if label is None else str(label), int(created)


def configure_repository(volume: Path, volume_id: str, platform: str) -> None:
    git(volume, "init", "--quiet")
    git(volume, "config", "core.logAllRefUpdates", "always")
    git(
        volume,
        "config",
        "gc.refs/snowglass/archive/v1/recently-deleted.reflogExpireUnreachable",
        "30 days",
    )

    git_dir = Path(git(volume, "rev-parse", "--absolute-git-dir").decode().strip())
    marker_path = git_dir / "snowglass.json"
    marker = {"schema": 1, "platform": platform, "volume_id": volume_id}
    if marker_path.exists():
        existing = json.loads(marker_path.read_text(encoding="utf-8"))
        if existing != marker:
            raise RuntimeError(f"repository marker conflicts at {marker_path}")
    else:
        marker_path.write_text(json.dumps(marker, indent=2) + "\n", encoding="utf-8")

    exclude = git_dir / "info" / "exclude"
    existing = exclude.read_text(encoding="utf-8") if exclude.exists() else ""
    policy = "\n# snowglass canonical storage\n*\n"
    if "# snowglass canonical storage" not in existing:
        exclude.parent.mkdir(parents=True, exist_ok=True)
        with exclude.open("a", encoding="utf-8") as output:
            output.write(policy)


def hash_blob(volume: Path, value: str) -> str:
    return git(volume, "hash-object", "-w", "--stdin", input_bytes=value.encode()).decode().strip()


def has_volume_ref(volume: Path) -> bool:
    return (
        subprocess.run(
            ["git", "-C", str(volume), "rev-parse", "--verify", "--quiet", VOLUME_REF],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            check=False,
        ).returncode
        == 0
    )


def migrate(volume: Path, dry_run: bool) -> str:
    info = identity(volume)
    if info is None:
        return "unbranded"
    volume_id, platform, label, created = info
    if dry_run:
        if has_volume_ref(volume):
            return "already migrated"
        return f"would migrate {platform} {volume_id}"

    configure_repository(volume, volume_id, platform)
    if has_volume_ref(volume):
        return "already migrated"

    values = {"id": volume_id, "platform": platform, "created": str(created)}
    if label is not None:
        values["label"] = label
    entries = "".join(
        f"100644 blob {hash_blob(volume, value)}\t{key}\n"
        for key, value in sorted(values.items())
    )
    tree = git(volume, "mktree", input_bytes=entries.encode()).decode().strip()
    environment = os.environ.copy()
    environment.update(
        {
            "GIT_AUTHOR_NAME": "Snowglass",
            "GIT_AUTHOR_EMAIL": "history@snowglass.invalid",
            "GIT_COMMITTER_NAME": "Snowglass",
            "GIT_COMMITTER_EMAIL": "history@snowglass.invalid",
        }
    )
    process = subprocess.run(
        ["git", "-C", str(volume), "commit-tree", tree],
        input=b"Snowglass volume migrated\n",
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        env=environment,
        check=False,
    )
    if process.returncode:
        detail = process.stderr.decode(errors="replace").strip()
        raise RuntimeError(f"git commit-tree failed for {volume}: {detail}")
    commit = process.stdout.decode().strip()
    git(volume, "update-ref", "--create-reflog", VOLUME_REF, commit)
    return f"migrated {platform} {volume_id}"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "paths",
        nargs="+",
        type=Path,
        help="storage-library roots or individual versioned-volume directories",
    )
    parser.add_argument("--dry-run", action="store_true")
    arguments = parser.parse_args()

    selected = sorted({volume.resolve() for path in arguments.paths for volume in volumes(path)})
    if not selected:
        parser.error("no versioned volumes with .manifest.db were found")
    failures = 0
    for volume in selected:
        try:
            print(f"{volume}: {migrate(volume, arguments.dry_run)}")
        except Exception as error:
            failures += 1
            print(f"{volume}: {error}", file=sys.stderr)
    return 1 if failures else 0


if __name__ == "__main__":
    raise SystemExit(main())
