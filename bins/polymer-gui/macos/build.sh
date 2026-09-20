#!/bin/sh
set -eu

# Builds a signed development GUI bundle using explicit provisioning inputs.
# Usage: build.sh identity host-profile extension-profile [output-directory]
identity=${1:?Provide a signing identity hash}
host_profile=${2:?Provide the host provisioning profile}
extension_profile=${3:?Provide the extension provisioning profile}
script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
workspace_dir=$(CDPATH= cd -- "$script_dir/../../.." && pwd)
extension_dir="$workspace_dir/crates/filesystem/polymer-vfs-fskit/extension"
output_dir=${4:-"$workspace_dir/target/macos/gui"}
mkdir -p "$output_dir"
output_dir=$(CDPATH= cd -- "$output_dir" && pwd)
"$extension_dir/build.sh" "$output_dir"
cargo build --manifest-path "$workspace_dir/Cargo.toml" -p polymer-gui \
    -p polymer-hook-metal -p polymer-hook-appkit -p polymer-hook-sdl3


python3 - "$extension_dir" "$output_dir" "$host_profile" "$extension_profile" "$workspace_dir" <<'PY'
import json
import pathlib
import plistlib
import shutil
import subprocess
import sys

source, output, host_profile, extension_profile, workspace = map(pathlib.Path, sys.argv[1:])
app = output / "Polymer.app"
if app.exists():
    raise SystemExit(f"Choose a fresh output directory; {app} already exists")
profiles = []
for path, identifier, capability in [
    (host_profile, "gg.polymer", "mount"),
    (extension_profile, "gg.polymer.filesystem", "fsmodule"),
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

executable = workspace / "target/debug/polymer-gui"
(app / "Contents/MacOS").mkdir(parents=True)
shutil.copy2(executable, app / "Contents/MacOS/polymer-gui")
for name in ["metal", "appkit", "sdl3"]:
    shutil.copy2(workspace / f"target/debug/libpolymer_hook_{name}.dylib", app / f"Contents/MacOS/libpolymer_hook_{name}.dylib")
# Bundle SDL rather than relying on a Homebrew installation at launch.
frameworks = app / "Contents/Frameworks"
frameworks.mkdir()
loads = subprocess.check_output(["otool", "-L", str(executable)], text=True)
sdl, = [line.strip().split(" (")[0] for line in loads.splitlines() if "libSDL3" in line]
shutil.copy2(sdl, frameworks / "libSDL3.dylib")
# Guest artifacts resolve beside the host executable. Share the bundled runtime.
(app / "Contents/MacOS/libSDL3.dylib").symlink_to("../Frameworks/libSDL3.dylib")
subprocess.check_call(["install_name_tool", "-change", sdl, "@executable_path/../Frameworks/libSDL3.dylib", str(app / "Contents/MacOS/polymer-gui")])
extension = app / "Contents/Extensions/PolymerFilesystem.appex"
shutil.copytree(output / "PolymerFilesystem.appex", extension)
(app / "Contents/Info.plist").write_bytes(plistlib.dumps({
    "CFBundleExecutable": "polymer-gui",
    "CFBundleIdentifier": "gg.polymer",
    "CFBundleName": "Polymer",
    "CFBundlePackageType": "APPL",
    "CFBundleVersion": "1",
    "CFBundleShortVersionString": "0.1.0",
    "LSMinimumSystemVersion": "27.0",
    "NSBluetoothAlwaysUsageDescription": "Polymer connects to game controllers.",
}))
for bundle, name, (profile, team, identifier) in zip(
    [app, extension], ["Host", "Extension"], profiles
):
    shutil.copy2(profile, bundle / "Contents/embedded.provisionprofile")
    entitlements = plistlib.loads((source / f"{name}.entitlements").read_bytes())
    entitlements.update({"com.apple.application-identifier": f"{team}.{identifier}",
                         "com.apple.developer.team-identifier": team})
    if name == "Host":
        entitlements.update({"com.apple.security.cs.allow-jit": True,
                             "com.apple.security.cs.allow-unsigned-executable-memory": True})
    (output / f"{name}.entitlements").write_bytes(plistlib.dumps(entitlements))
PY

bundle="$output_dir/Polymer.app"
for library in "$bundle"/Contents/MacOS/*.dylib "$bundle"/Contents/Frameworks/*.dylib; do
    codesign --force --sign "$identity" --options runtime "$library"
done
codesign --force --sign "$identity" --options runtime \
    --entitlements "$output_dir/Extension.entitlements" \
    "$bundle/Contents/Extensions/PolymerFilesystem.appex"
codesign --force --sign "$identity" --options runtime \
    --entitlements "$output_dir/Host.entitlements" "$bundle"
codesign --verify --deep --strict "$bundle"
