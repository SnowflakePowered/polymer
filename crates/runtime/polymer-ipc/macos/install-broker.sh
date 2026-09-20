#!/bin/sh
set -eu

# Development registration; a packaged app must ship/register this helper too.
script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
workspace_dir=$(CDPATH= cd -- "$script_dir/../../../.." && pwd)
cargo build --manifest-path "$workspace_dir/Cargo.toml" -p polymer-ipc --bin polymer-ipc-broker
broker_path="$workspace_dir/target/debug/polymer-ipc-broker"
agent_path="$HOME/Library/LaunchAgents/org.polymer.ipc.plist"
mkdir -p "$(dirname -- "$agent_path")"
python3 - "$broker_path" "$agent_path" <<'PY'
import pathlib
import plistlib
import sys

binary, agent = map(pathlib.Path, sys.argv[1:])
agent.write_bytes(plistlib.dumps({
    "Label": "org.polymer.ipc",
    "ProgramArguments": [str(binary)],
    "MachServices": {"org.polymer.ipc": True},
    "ProcessType": "Interactive",
}))
PY
launchctl bootout "gui/$(id -u)/org.polymer.ipc" 2>/dev/null || true
launchctl bootstrap "gui/$(id -u)" "$agent_path"
printf '%s\n' "Registered org.polymer.ipc from $broker_path"
