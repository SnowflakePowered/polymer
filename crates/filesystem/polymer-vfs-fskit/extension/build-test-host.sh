#!/bin/sh
set -eu

# Packages the real Rust integration test; does not install or enable it.
# Usage: build-test-host.sh identity host-profile extension-profile [output-directory]
identity=${1:?Provide a signing identity hash}
host_profile=${2:?Provide the host provisioning profile}
extension_profile=${3:?Provide the extension provisioning profile}
extension_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
workspace_dir=$(CDPATH= cd -- "$extension_dir/../../../.." && pwd)
output_dir=${4:-"$workspace_dir/target/macos/verification"}
mkdir -p "$output_dir"
output_dir=$(CDPATH= cd -- "$output_dir" && pwd)
"$extension_dir/build.sh" "$output_dir"
cargo test --manifest-path "$workspace_dir/Cargo.toml" -p polymer-vfs-fskit \
    --test mount --no-run --message-format=json > "$output_dir/test-build.json"

python3 - "$extension_dir" "$output_dir" "$host_profile" "$extension_profile" <<'PY'
import json
import pathlib
import plistlib
import shutil
import subprocess
import sys

source, output, host_profile, extension_profile = map(pathlib.Path, sys.argv[1:])
app = output / "PolymerFSKitTest.app"
if app.exists():
    raise SystemExit(f"Choose a fresh output directory; {app} already exists")
profiles = []
for path, identifier, capability in [
    (host_profile, "org.polymer", "mount"),
    (extension_profile, "org.polymer.filesystem", "fsmodule"),
]:
    profile = plistlib.loads(subprocess.check_output(["security", "cms", "-D", "-i", str(path)]))
    team, = profile["TeamIdentifier"]
    entitlements = profile["Entitlements"]
    if (entitlements.get("com.apple.application-identifier") != f"{team}.{identifier}"
            or entitlements.get(f"com.apple.developer.fskit.{capability}") is not True):
        raise SystemExit(f"Profile does not authorize {identifier}'s FSKit entitlement")
    profiles.append((path, team, identifier))
if profiles[0][1] != profiles[1][1]:
    raise SystemExit("Host and extension profiles must use the same team")

executables = [entry["executable"] for line in (output / "test-build.json").read_text().splitlines()
               if (entry := json.loads(line)).get("reason") == "compiler-artifact"
               and entry.get("target", {}).get("name") == "mount" and entry.get("executable")]
executable, = executables
(app / "Contents/MacOS").mkdir(parents=True)
shutil.copy2(executable, app / "Contents/MacOS/PolymerFSKitTest")
extension = app / "Contents/Extensions/PolymerFilesystem.appex"
shutil.copytree(output / "PolymerFilesystem.appex", extension)
(app / "Contents/Info.plist").write_bytes(plistlib.dumps({
    "CFBundleExecutable": "PolymerFSKitTest",
    "CFBundleIdentifier": "org.polymer",
    "CFBundleName": "Polymer FSKit Test",
    "CFBundlePackageType": "APPL",
    "CFBundleVersion": "1",
    "CFBundleShortVersionString": "0.1.0",
    "LSMinimumSystemVersion": "27.0",
}))
for bundle, name, (profile, team, identifier) in zip(
    [app, extension], ["Host", "Extension"], profiles
):
    shutil.copy2(profile, bundle / "Contents/embedded.provisionprofile")
    entitlements = plistlib.loads((source / f"{name}.entitlements").read_bytes())
    entitlements.update({"com.apple.application-identifier": f"{team}.{identifier}",
                         "com.apple.developer.team-identifier": team})
    (output / f"{name}.entitlements").write_bytes(plistlib.dumps(entitlements))
PY

bundle="$output_dir/PolymerFSKitTest.app"
codesign --force --sign "$identity" --options runtime \
    --entitlements "$output_dir/Extension.entitlements" \
    "$bundle/Contents/Extensions/PolymerFilesystem.appex"
codesign --force --sign "$identity" --options runtime \
    --entitlements "$output_dir/Host.entitlements" "$bundle"
codesign --verify --deep --strict "$bundle"
