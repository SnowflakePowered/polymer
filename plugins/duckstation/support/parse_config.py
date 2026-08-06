#!/usr/bin/env python3
"""Generate the DuckStation plugin's configuration documents from its source.

DuckStation reads settings through call sites rather than a declaration table —
`si.GetBoolValue("Console", "Enable8MBRAM", false)` carries the section, the key
and the default in one call — so the settings surface is extracted from the
calls themselves. Controllers are the opposite: fully declarative
`ControllerBindingInfo` tables, each entry carrying a `GenericInputBinding` that
names the standard-pad control it corresponds to. That second column is what
makes one consolespec mapping serve every controller type.

Outputs, into --out (default ../config):

    duckstation.toml   targets + sections + options   (core/settings.cpp)
    actions.toml       action map                     (core/hotkeys.cpp)
    input.toml         ports + per-layout bindings    (core/*_controller.cpp)

Usage:

    python parse_config.py --duckstation E:/coding/duckstation
"""

from __future__ import annotations

import argparse
import re
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "support"))
import consolespec
import cppconfig
from cppconfig import *  # noqa: F403 — shared C++/TOML machinery

cppconfig.DEFINES = {"_WIN32": True, "WIN32": True}
cppconfig.UNDEFINES = {
    "__ANDROID__", "ANDROID", "__APPLE__", "__linux__", "__FreeBSD__",
}
cppconfig.NOOP_MACROS = ("TRANSLATE_NOOP", "TRANSLATE_DISAM_NOOP", "FSUI_NSTR")

SETTINGS_FILE = "settings.ini"
BOOL_STYLE = "lower"

# DuckStation's own standard-pad vocabulary (`GenericInputBinding`) already says
# what each controller control corresponds to on a normal gamepad. Mapping the
# consolespec element vocabulary onto *that* gives every controller type at
# once, instead of a table per type. Each element is still checked against the
# layout it is emitted for, so a control the device does not have is skipped.
ELEMENT_TO_GENERIC = {
    "ButtonA": "Cross",
    "ButtonB": "Circle",
    "ButtonX": "Square",
    "ButtonY": "Triangle",
    "ButtonSelect": "Select",
    "ButtonStart": "Start",
    "ButtonGuide": "System",
    "ButtonL": "L1",
    "ButtonR": "R1",
    "TriggerLeft": "L2",
    "TriggerRight": "R2",
    "ButtonClickL": "L3",
    "ButtonClickR": "R3",
    "DirectionalN": "DPadUp",
    "DirectionalS": "DPadDown",
    "DirectionalW": "DPadLeft",
    "DirectionalE": "DPadRight",
    "AxisLeftAnalogNegativeX": "LeftStickLeft",
    "AxisLeftAnalogPositiveX": "LeftStickRight",
    "AxisLeftAnalogNegativeY": "LeftStickDown",
    "AxisLeftAnalogPositiveY": "LeftStickUp",
    "AxisRightAnalogNegativeX": "RightStickLeft",
    "AxisRightAnalogPositiveX": "RightStickRight",
    "AxisRightAnalogNegativeY": "RightStickDown",
    "AxisRightAnalogPositiveY": "RightStickUp",
}

# The machine these ports belong to and the machinespec input group they sit
# in. DuckStation exposes eight pad slots where a PlayStation has two, because
# the extra six are the two multitaps; the group check reports that rather than
# treating it as an error.
MACHINE = "SONY_PSX"
PORT_GROUP = "controller"
PORTS = 8

# inputspec layout → the DuckStation controller type that realizes it. `Type`
# is what the port section stores; the binding names come from that type's
# table.
LAYOUTS = {
    "PSX_CONTROLLER": "DigitalController",
    "DUALSHOCK_CONTROLLER": "AnalogController",
}

# Snowglass's global action vocabulary → DuckStation's hotkey name. DuckStation
# ships defaults for some of these in SetDefaultHotkeyConfig; the rest get a
# fallback that is checked against every parsed default for collisions.
ACTION_VOCABULARY = {
    "load_state": {"hotkey": "LoadSelectedSaveState"},
    "save_state": {"hotkey": "SaveSelectedSaveState"},
    "screenshot": {"hotkey": "Screenshot"},
    "pause": {"hotkey": "TogglePause"},
    "fast_forward": {"hotkey": "FastForward"},
    "toggle_fullscreen": {"hotkey": "ToggleFullscreen"},
    "frame_advance": {"hotkey": "FrameStep", "fallback": ("f6", "Keyboard/F6")},
    "reset": {"hotkey": "Reset", "fallback": ("f7", "Keyboard/F7")},
    "exit": {"hotkey": "PowerOff", "fallback": ("f8", "Keyboard/F8")},
}

# `si.<method>("Section", "Key", <default>)` — the method names the type.
SETTING_METHODS = {
    "GetBoolValue": "bool",
    "GetIntValue": "int",
    "GetUIntValue": "int",
    "GetFloatValue": "float",
    "GetDoubleValue": "float",
    "GetStringValue": "string",
    "GetStringViewValue": "string",
    "GetSmallStringValue": "string",
    "GetTinyStringValue": "string",
}

# Sections owned by input.toml rather than the definition document.
INPUT_SECTIONS = ("ControllerPorts", "Hotkeys", "InputSources")


def parse_settings(tree_root: Path, report: Report) -> list[dict]:
    """Every `si.GetXValue("Section", "Key", default)` call in settings.cpp."""
    text = load_source(tree_root / "src/core/settings.cpp", report)
    scope = Scope()
    settings: dict[tuple[str, str], dict] = {}
    pattern = re.compile(r"\.(" + "|".join(SETTING_METHODS) + r")\s*\(")
    for found in pattern.finditer(text):
        close = match_braces(text, found.end() - 1, "(", ")")
        arguments = split_top_level(text[found.end(): close - 1])
        if len(arguments) < 2:
            continue
        section = parse_string_literal(arguments[0])
        key = parse_string_literal(arguments[1])
        if section is None or key is None:
            continue
        if section in INPUT_SECTIONS or re.fullmatch(r"Pad\d+", section):
            continue
        default = None
        if len(arguments) > 2:
            try:
                default = evaluate_expression(arguments[2], scope)
            except Unresolved:
                default = None
        entry = {
            "section": section,
            "key": key,
            "type": SETTING_METHODS[found.group(1)],
            "default": default,
        }
        previous = settings.get((section, key))
        if previous and previous != entry:
            report.note(f"settings: {section}/{key} is read more than one way; "
                        "keeping the first")
            continue
        settings[(section, key)] = entry
    if not settings:
        raise Bail("no settings calls found; check --duckstation")
    return list(settings.values())


def parse_hotkeys(tree_root: Path, report: Report) -> tuple[list[dict], dict[str, str]]:
    """The hotkey vocabulary and the defaults DuckStation ships for it."""
    text = load_source(tree_root / "src/core/hotkeys.cpp", report)
    found = re.search(r"s_hotkey_list\[\]\s*=\s*\{", text)
    if not found:
        raise Bail("hotkeys.cpp no longer defines s_hotkey_list")
    body = text[found.end(): match_braces(text, found.end() - 1) - 1]
    hotkeys = []
    for entry in split_top_level(body):
        entry = entry.strip()
        if not entry.startswith("{"):
            continue
        fields = split_top_level(entry[1: match_braces(entry, 0) - 1])
        if len(fields) < 3:
            continue
        name = parse_string_literal(strip_noop_macros(fields[0]))
        category = parse_string_literal(strip_noop_macros(fields[1]))
        title = parse_string_literal(strip_noop_macros(fields[2]))
        if name:
            hotkeys.append({"name": name, "category": category, "title": title})
    if not hotkeys:
        raise Bail("s_hotkey_list parsed empty")

    defaults: dict[str, str] = {}
    start = re.search(r"SetDefaultHotkeyConfig\s*\([^)]*\)\s*\{", text)
    if not start:
        raise Bail("hotkeys.cpp no longer defines SetDefaultHotkeyConfig")
    scope = text[start.end(): match_braces(text, start.end() - 1) - 1]
    for call in re.finditer(r"SetStringValue\s*\(", scope):
        close = match_braces(scope, call.end() - 1, "(", ")")
        arguments = split_top_level(scope[call.end(): close - 1])
        if len(arguments) != 3:
            continue
        section = parse_string_literal(arguments[0])
        key = parse_string_literal(arguments[1])
        value = parse_string_literal(arguments[2])
        if section == "Hotkeys" and key and value:
            defaults[key] = value
    report.note(f"actions: {len(hotkeys)} hotkeys declared, {len(defaults)} with defaults")
    return hotkeys, defaults


def parse_binding_tables(tree_root: Path, report: Report) -> dict[str, dict[str, str]]:
    """Controller type → {GenericInputBinding: ini binding name}.

    Each `ControllerBindingInfo` entry is written through a local BUTTON/AXIS/
    MOTOR macro whose first argument is the ini name and whose last is the
    generic control it stands for.
    """
    tables: dict[str, dict[str, str]] = {}
    variables: dict[str, dict[str, str]] = {}
    for path in sorted((tree_root / "src/core").glob("*controller*.cpp")):
        text = load_source(path, report)
        # Some tables are file-static, others are class members
        # (`AnalogController::s_binding_info`); the INFO entry names the latter
        # by its unqualified form, so key on that.
        for found in re.finditer(r"ControllerBindingInfo\s+([\w:]+)\s*\[\]\s*=\s*\{", text):
            variable = found.group(1).split("::")[-1]
            body = text[found.end(): match_braces(text, found.end() - 1) - 1]
            entries: dict[str, str] = {}
            for call in re.finditer(r"\b(BUTTON|AXIS|MOTOR|MODE_LED|HALFAXIS)\s*\(", body):
                close = match_braces(body, call.end() - 1, "(", ")")
                fields = split_top_level(body[call.end(): close - 1])
                if len(fields) < 2:
                    continue
                name = parse_string_literal(strip_noop_macros(fields[0]))
                generic = re.match(r"GenericInputBinding::(\w+)", fields[-1].strip())
                if name and generic:
                    entries.setdefault(generic.group(1), name)
            if entries:
                variables[f"{path.name}:{variable}"] = entries
        # `ControllerInfo X::INFO = {ControllerType::Y, "Name", ..., table, {}}`
        for found in re.finditer(r"ControllerInfo\s+[\w:]*INFO\w*\s*=\s*\{", text):
            fields = split_top_level(
                text[found.end(): match_braces(text, found.end() - 1) - 1]
            )
            if len(fields) < 6:
                continue
            name = parse_string_literal(strip_noop_macros(fields[1]))
            key = f"{path.name}:{fields[5].strip().split('::')[-1]}"
            if name and key in variables:
                tables[name] = variables[key]
    if not tables:
        raise Bail("no controller binding tables parsed")
    report.note("input: binding tables for " + ", ".join(sorted(tables)))
    return tables


def build_definition(settings: list[dict], revision: str | None, report: Report) -> str:
    grouped: dict[str, list[dict]] = {}
    for setting in settings:
        grouped.setdefault(setting["section"], []).append(setting)

    lines = [
        "# Generated by support/parse_config.py. Do not edit by hand.",
        f"# DuckStation revision: {revision or 'unknown'}",
        "",
        "[meta]",
        'emulator = "duckstation"',
        f'source_revision = "{revision or "unknown"}"',
        "",
        f"[bool_style.{BOOL_STYLE}]",
        'true = "true"',
        'false = "false"',
        "",
        target_header("settings"),
        'format = "ini"',
        f"path = {toml_string(SETTINGS_FILE)}",
        "",
    ]
    lines += [target_header("settings", section) for section in sorted(grouped)]
    lines.append("")

    emitted = 0
    for section in sorted(grouped):
        target = target_id("settings", section)
        lines += [f"[sections.{toml_key(slugify(section))}]",
                  f"title = {toml_string(section)}",
                  f"target = {toml_string(target)}", ""]
        used: set[str] = set()
        for setting in sorted(grouped[section], key=lambda s: s["key"].lower()):
            key = snake_case(setting["key"])
            if key in used:
                continue
            used.add(key)
            emit = {"key": setting["key"]}
            if setting["type"] == "bool":
                emit["bool_style"] = BOOL_STYLE
            lines.append(f"[[sections.{toml_key(slugify(section))}.options]]")
            lines.append(f"key = {toml_string(key)}")
            lines.append(f"title = {toml_string(humanize(setting['key']))}")
            lines.append(f"type = {toml_string(setting['type'])}")
            if setting["default"] is not None:
                lines.append(f"default = {toml_value(setting['default'])}")
            lines.append(f"emit = {toml_value(emit)}")
            lines.append("")
            emitted += 1
    report.note(f"definition: {emitted} options across {len(grouped)} sections")
    return "\n".join(lines).rstrip() + "\n"


def build_actions(hotkeys: list[dict], defaults: dict[str, str], revision: str | None,
                  report: Report) -> str:
    known = {hotkey["name"] for hotkey in hotkeys}
    taken = {canonical_chord(value) for value in defaults.values()}
    lines = [
        "# Generated by support/parse_config.py. Do not edit by hand.",
        f"# DuckStation revision: {revision or 'unknown'}",
        "",
        target_header("settings"),
        'format = "ini"',
        f"path = {toml_string(SETTINGS_FILE)}",
        "",
        target_header("settings", "Hotkeys"),
        "",
        "[actions]",
        f'target = {toml_string(target_id("settings", "Hotkeys"))}',
        # DuckStation reads the keyboard through Qt, so window messages reach it.
        'transport = "win32-message"',
        'keynames = "duckstation"',
        "",
        "[actions.map]",
    ]
    resolved = 0
    for action, spec in sorted(ACTION_VOCABULARY.items()):
        hotkey = spec["hotkey"]
        if hotkey not in known:
            report.error(f"action `{action}`: `{hotkey}` is not in s_hotkey_list")
            continue
        expression = defaults.get(hotkey)
        if expression is None:
            fallback = spec.get("fallback")
            if fallback is None:
                report.error(f"action `{action}`: `{hotkey}` has no default and no fallback")
                continue
            chord, expression = fallback
            if chord in taken:
                report.error(f"action `{action}`: fallback `{expression}` collides "
                             "with a DuckStation default")
                continue
            taken.add(chord)
            report.note(f"action `{action}`: `{hotkey}` is unbound upstream; using {expression}")
        else:
            chord = canonical_chord(expression)
        lines.append(f"{action} = "
                     f"{toml_value({'key': chord, 'expression': expression, 'name': hotkey})}")
        resolved += 1
    report.note(f"actions: {resolved} of {len(ACTION_VOCABULARY)} vocabulary entries resolved, "
                f"{len(known) - resolved} DuckStation hotkeys unused")
    return "\n".join(lines).rstrip() + "\n"


def canonical_chord(expression: str) -> str:
    """`Keyboard/F1` → the snowglass key spec the shell synthesizes."""
    text = expression.split("/", 1)[-1] if expression.startswith("Keyboard/") else expression
    return "+".join(part.strip().lower() for part in text.split("+") if part.strip())


def build_input(tables: dict[str, dict[str, str]], spec_root: Path,
                revision: str | None, report: Report) -> str:
    projection = consolespec.load_projection()
    lines = [
        "# Generated by support/parse_config.py. Do not edit by hand.",
        f"# DuckStation revision: {revision or 'unknown'}",
        "",
        target_header("settings"),
        'format = "ini"',
        f"path = {toml_string(SETTINGS_FILE)}",
        "",
    ]
    lines += [target_header("settings", f"Pad{port}")
              for port in range(1, PORTS + 1)]
    lines += ["", "[input]", 'dialect = "duckstation"',
              'device = "SDL-{index}"', ""]

    bound = 0
    for layout, controller in LAYOUTS.items():
        table = tables.get(controller)
        if table is None:
            report.error(f"input: no binding table for controller type `{controller}`")
            continue
        lines.append(f"[input.layouts.{toml_key(layout)}]")
        device = consolespec.load_inputspec(spec_root, layout)
        for element, generic in sorted(ELEMENT_TO_GENERIC.items()):
            binding = table.get(generic)
            if binding is None:
                continue
            # The generic table is shared across controller types, so an
            # element DuckStation names for this type but the device does not
            # have is a disagreement between two vocabularies, not a typo here.
            if element not in device:
                report.note(f"input: {layout} has no `{element}`; DuckStation's "
                            f"{controller} binds it as `{binding}`")
                continue
            entry = device.elements[element]
            if projection.slot(layout, entry) is None:
                report.warn(f"input: `{entry.path}` reaches no SDL slot; "
                            f"omitted from {layout}")
                continue
            lines.append(f"{toml_key(entry.path)} = {toml_string(binding)}")
            bound += 1
        lines.append("")

    machine = consolespec.load_machinespec(spec_root, MACHINE)
    consolespec.check_group(machine, PORT_GROUP, list(LAYOUTS), PORTS, report,
                            "input")
    for port in range(1, PORTS + 1):
        lines.append(f"[platforms.{toml_key(MACHINE)}.ports."
                     f"{toml_key(PORT_GROUP)}.{port - 1}]")
        lines.append(f"target = {toml_string(target_id('settings', f'Pad{port}'))}")
        lines.append(f"layouts = {toml_value(list(LAYOUTS))}")
        lines.append('emit.unplugged = [{ key = "Type", value = "None" }]')
        for layout, controller in LAYOUTS.items():
            lines.append(f"emit.{toml_key(layout)} = "
                         f"{toml_value([{'key': 'Type', 'value': controller}])}")
        lines.append("")
    report.note(f"input: {bound} bindings across {len(LAYOUTS)} layouts, "
                f"{PORTS} {MACHINE}/{PORT_GROUP} ports")
    return "\n".join(lines).rstrip() + "\n"


def revision_of(root: Path) -> str | None:
    try:
        result = subprocess.run(["git", "-C", str(root), "rev-parse", "--short", "HEAD"],
                                capture_output=True, text=True, check=True, timeout=15)
    except (OSError, subprocess.SubprocessError):
        return None
    return result.stdout.strip() or None


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--duckstation", required=True, type=Path)
    parser.add_argument("--out", type=Path, default=Path(__file__).parent.parent / "config")
    parser.add_argument("--consolespec", type=Path)
    parser.add_argument("--strict", action="store_true")
    parser.add_argument("--dry-run", action="store_true")
    arguments = parser.parse_args(argv)

    root = arguments.duckstation.expanduser().resolve()
    if not (root / "src/core").is_dir():
        print(f"error: {root} does not look like a DuckStation checkout", file=sys.stderr)
        return 2
    try:
        spec_root = (arguments.consolespec.expanduser().resolve()
                     if arguments.consolespec else consolespec.find_root())
    except consolespec.SpecError as error:
        print(f"error: {error}", file=sys.stderr)
        return 2

    report = Report()
    revision = revision_of(root)
    try:
        settings = parse_settings(root, report)
        hotkeys, defaults = parse_hotkeys(root, report)
        tables = parse_binding_tables(root, report)
        documents = {
            "duckstation.toml": build_definition(settings, revision, report),
            "actions.toml": build_actions(hotkeys, defaults, revision, report),
            "input.toml": build_input(tables, spec_root, revision, report),
        }
    except (Bail, consolespec.SpecError) as error:
        print(f"error: {error}", file=sys.stderr)
        report.emit()
        return 1

    if not arguments.dry_run:
        arguments.out.mkdir(parents=True, exist_ok=True)
        for name, text in documents.items():
            (arguments.out / name).write_text(text, encoding="utf-8", newline="\n")
    report.emit()
    print(f"wrote {', '.join(sorted(documents))} to "
          f"{'(dry run)' if arguments.dry_run else arguments.out}")
    print(f"{len(report.notes)} notes, {len(report.warnings)} warnings, "
          f"{len(report.errors)} errors")
    if report.errors:
        return 1
    return 1 if arguments.strict and report.warnings else 0


if __name__ == "__main__":
    sys.exit(main())
