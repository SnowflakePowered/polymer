#!/bin/sh
set -eu

# Build a reviewable .appex; installation/signing belongs to the containing app.
# Usage: build.sh [output-directory]
extension_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
workspace_dir=$(CDPATH= cd -- "$extension_dir/../../../.." && pwd)
output_dir=${1:-"$workspace_dir/target/macos"}
sdk_path=${SDKROOT:-$(xcrun --sdk macosx --show-sdk-path)}
cargo build --manifest-path "$workspace_dir/Cargo.toml" -p polymer-vfs-fskit
bundle="$output_dir/PolymerFilesystem.appex"
mkdir -p "$bundle/Contents/MacOS"
cp "$extension_dir/Info.plist" "$bundle/Contents/Info.plist"
xcrun swiftc -parse-as-library -application-extension -target arm64-apple-macos27.0 -sdk "$sdk_path" \
    -import-objc-header "$extension_dir/PolymerFileSystem.h" \
    "$extension_dir/PolymerExtension.swift" \
    "$workspace_dir/target/debug/libpolymer_vfs_fskit.a" \
    -framework FSKit -framework Foundation -framework Security -framework Metal -framework IOSurface \
    -framework SystemConfiguration -lc++ -liconv -lz \
    -Xlinker -e -Xlinker _NSExtensionMain \
    -o "$bundle/Contents/MacOS/PolymerFilesystem"
