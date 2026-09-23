#!/usr/bin/env python3
"""Install the pinned Windows emulator releases into plugins/<id>/bin/win-x64.

Usage: python plugins/support/install_runtimes.py [azahar melonds rpcs3 ymir ares]
Requires Python 3.11+ and Windows tar.exe (for RPCS3's BCJ2-compressed 7z).
Existing installations are left intact.
"""

import argparse
import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import tempfile
import urllib.request
import zipfile


class RuntimeInstaller:
    def __init__(self):
        self.root = Path(__file__).resolve().parents[1]
        self.releases = json.loads(Path(__file__).with_name("runtimes.json").read_text())

    def install(self, name):
        release = self.releases[name]
        target = self.root / name / "bin" / "win-x64"
        if target.exists():
            if not (target / release["executable"]).is_file():
                raise RuntimeError(f"Incomplete runtime at {target}; move it aside before retrying")
            print(f"{name}: already installed at {target}")
            return
        with tempfile.TemporaryDirectory(prefix=f"polymer-{name}-") as scratch:
            scratch = Path(scratch)
            archive = scratch / release["url"].rsplit("/", 1)[1]
            print(f"{name}: downloading {release['version']}", flush=True)
            urllib.request.urlretrieve(release["url"], archive)
            with archive.open("rb") as stream:
                digest = hashlib.file_digest(stream, "sha256").hexdigest()
            if digest != release["sha256"]:
                raise RuntimeError(f"{name}: downloaded archive failed SHA-256 verification")
            unpacked = scratch / "unpacked"
            unpacked.mkdir()
            if archive.suffix == ".zip":
                with zipfile.ZipFile(archive) as contents:
                    contents.extractall(unpacked)
            else:
                subprocess.run(["tar", "-xf", str(archive), "-C", str(unpacked)], check=True)
            source = unpacked / release["directory"]
            if not (source / release["executable"]).is_file():
                raise RuntimeError(f"{name}: release does not contain {release['executable']}")
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copytree(source, target)
            print(f"{name}: installed {release['version']} at {target}")

    def run(self):
        parser = argparse.ArgumentParser(description=__doc__)
        parser.add_argument("plugins", nargs="*", help="Plugin IDs (defaults to all five)")
        names = parser.parse_args().plugins or list(self.releases)
        for name in names:
            if name not in self.releases:
                parser.error(f"Unknown plugin {name}; choose from {', '.join(self.releases)}")
        for name in names:
            self.install(name)


if __name__ == "__main__":
    RuntimeInstaller().run()
