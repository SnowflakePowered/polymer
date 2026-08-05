#!/usr/bin/env python3
"""Generate the Cemu plugin's configuration documents from Cemu's source.

Cemu is the odd one of the four. Its settings live in an XML document, not an
INI, so the definition document's targets nest to arbitrary depth. Its hotkeys
are stored as *two space-separated integers in one element* — a wxWidgets
keycode and a controller button index — which is a fifth binding encoding beyond
the four the design's survey lists. And its controller bindings are keyed by
number rather than name: `<mapping>7</mapping>` is `VPADController::ButtonId`
seven, so the binding table is an integer mapping on both sides.

Outputs, into --out (default ../config):

    cemu.toml      targets + sections + options   (config/CemuConfig.{h,cpp})
    actions.toml   action map                     (gui/wxgui/wxCemuConfig.{h,cpp})
    input.toml     ports + per-layout bindings    (input/emulated/*.h)

Usage:

    python parse_config.py --cemu E:/coding/Cemu
"""

from __future__ import annotations

import argparse
import re
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "support"))
import cppconfig
from cppconfig import *  # noqa: F403 — shared C++/TOML machinery

cppconfig.DEFINES = {"_WIN32": True, "WIN32": True}
cppconfig.UNDEFINES = {
    "__linux__", "__APPLE__", "CEMU_DEBUG_ASSERT", "ANDROID", "__ANDROID__",
}

SETTINGS_FILE = "settings.xml"
BOOL_STYLE = "lower"

# Cemu's emulated controller types, by the string its profile writes into
# `<emulated_controller><type>`, paired with the Stone layout each realizes.
LAYOUTS = {
    "WIIU_PRO_CONTROLLER": "Wii U Pro Controller",
    "WIIU_DRC_CONTROLLER": "Wii U GamePad",
    "WII_REMOTE_CONTROLLER": "Wiimote",
}

# Stone element → the `ButtonId` enumerator Cemu's profile stores as a number.
# Cemu's own names are the vocabulary; the parser turns them into the integers
# the file actually holds, so a reordered enum changes the emitted numbers.
VPAD_BINDINGS = {
    "ButtonA": "kButtonId_A",
    "ButtonB": "kButtonId_B",
    "ButtonX": "kButtonId_X",
    "ButtonY": "kButtonId_Y",
    "ButtonL": "kButtonId_L",
    "ButtonR": "kButtonId_R",
    "TriggerLeft": "kButtonId_ZL",
    "TriggerRight": "kButtonId_ZR",
    "ButtonSelect": "kButtonId_Minus",
    "ButtonStart": "kButtonId_Plus",
    "ButtonClickL": "kButtonId_StickL",
    "ButtonClickR": "kButtonId_StickR",
    "DirectionalN": "kButtonId_Up",
    "DirectionalS": "kButtonId_Down",
    "DirectionalW": "kButtonId_Left",
    "DirectionalE": "kButtonId_Right",
    "AxisLeftAnalogPositiveY": "kButtonId_StickL_Up",
    "AxisLeftAnalogNegativeY": "kButtonId_StickL_Down",
    "AxisLeftAnalogNegativeX": "kButtonId_StickL_Left",
    "AxisLeftAnalogPositiveX": "kButtonId_StickL_Right",
    "AxisRightAnalogPositiveY": "kButtonId_StickR_Up",
    "AxisRightAnalogNegativeY": "kButtonId_StickR_Down",
    "AxisRightAnalogNegativeX": "kButtonId_StickR_Left",
    "AxisRightAnalogPositiveX": "kButtonId_StickR_Right",
    "ButtonGuide": "kButtonId_Home",
}

# Snowglass's global action vocabulary → Cemu's hotkey element name. Cemu has
# no save/load state, pause, frame advance or reset hotkey at all, so five of
# the nine simply cannot be expressed; the run reports which.
ACTION_VOCABULARY = {
    "toggle_fullscreen": "ToggleFullscreen",
    "screenshot": "TakeScreenshot",
    "fast_forward": "ToggleFastForward",
    "exit": "ExitApplication",
}


def parse_config_values(root: Path, report: Report) -> dict[str, dict]:
    """`ConfigValue<T> member{default};` declarations, keyed by member name."""
    text = load_source(root / "src/config/CemuConfig.h", report)
    scope = Scope()
    values: dict[str, dict] = {}
    pattern = re.compile(r"\bConfigValue(?:Bounds)?\s*<")
    for found in pattern.finditer(text):
        angle = found.end() - 1
        try:
            type_end = match_braces(text, angle, "<", ">")
        except Bail:
            continue
        ctype = text[angle + 1: type_end - 1].strip()
        declaration = re.match(r"\s*(\w+)\s*\{([^;]*)\}\s*;", text[type_end:])
        if not declaration:
            continue
        default = None
        expression = declaration.group(2).strip()
        if expression:
            try:
                default = evaluate_expression(expression, scope)
            except Unresolved:
                default = None
        values[declaration.group(1)] = {"ctype": ctype, "default": default}
    if not values:
        raise Bail("no ConfigValue declarations found; check --cemu")
    return values


def parse_xml_layout(root: Path, report: Report) -> list[dict]:
    """The XML path and member behind every `config.set("name", member)` call.

    Cemu writes its document with a parser handle per element, so the enclosing
    path is tracked through the `auto x = y.set("Node")` assignments rather than
    being visible at the call site.
    """
    text = load_source(root / "src/config/CemuConfig.cpp", report)
    start = re.search(r"XMLConfigParser\s+CemuConfig::Save\s*\([^)]*\)\s*\{", text)
    if not start:
        raise Bail("CemuConfig.cpp no longer defines CemuConfig::Save")
    body = text[start.end(): match_braces(text, start.end() - 1) - 1]

    handles: dict[str, tuple[str, ...]] = {}
    entries: list[dict] = []
    for found in re.finditer(
        r"(?:auto\s+(\w+)\s*=\s*)?(\w+)\s*\.set\s*(?:<[^>]*>)?\s*\(", body
    ):
        target, handle = found.group(1), found.group(2)
        close = match_braces(body, found.end() - 1, "(", ")")
        arguments = split_top_level(body[found.end(): close - 1])
        name = parse_string_literal(arguments[0]) if arguments else None
        if name is None:
            continue
        parent = handles.get(handle, ())
        if target:
            handles[target] = parent + (name,)
            continue
        if len(arguments) < 2:
            continue
        member = re.match(r"^([\w.]+?)(?:\.GetValue\(\))?(?:\.c_str\(\))?$",
                          arguments[1].strip())
        if not member:
            continue
        entries.append({"path": parent, "name": name, "member": member.group(1)})
    if not entries:
        raise Bail("CemuConfig::Save yielded no settings")
    return entries


def parse_hotkeys(root: Path, report: Report) -> dict[str, str | None]:
    """Cemu's hotkey element names, and the default each one carries."""
    text = load_source(root / "src/gui/wxgui/wxCemuConfig.cpp", report)
    hotkeys: dict[str, str | None] = {}
    for found in re.finditer(
        r"hotkeys\.\w+\s*=\s*xml_hotkeys\.get\s*\(", text
    ):
        close = match_braces(text, found.end() - 1, "(", ")")
        arguments = split_top_level(text[found.end(): close - 1])
        name = parse_string_literal(arguments[0]) if arguments else None
        if name is None:
            continue
        key = None
        if len(arguments) > 1:
            keycode = re.search(r"uKeyboardHotkey\{\s*(WXK_\w+)", arguments[1])
            if keycode:
                key = keycode.group(1)
        hotkeys[name] = key
    if not hotkeys:
        raise Bail("wxCemuConfig.cpp yielded no hotkeys")
    report.note(f"actions: {len(hotkeys)} hotkeys declared, "
                f"{sum(1 for k in hotkeys.values() if k)} with a default key")
    return hotkeys


def parse_button_ids(root: Path, report: Report) -> dict[str, int]:
    """`VPADController::ButtonId`, whose ordinals are what a profile stores."""
    text = load_source(root / "src/input/emulated/VPADController.h", report)
    found = re.search(r"\benum\s+ButtonId\s*\{", text)
    if not found:
        raise Bail("VPADController.h no longer declares `enum ButtonId`")
    body = text[found.end(): match_braces(text, found.end() - 1) - 1]
    ids: dict[str, int] = {}
    following = 0
    for entry in split_top_level(body):
        member = re.match(r"^(\w+)\s*(?:=\s*(.+))?$", entry.strip(), re.S)
        if not member:
            continue
        if member.group(2):
            try:
                following = int(evaluate_expression(member.group(2), Scope()))
            except (Unresolved, TypeError, ValueError):
                continue
        ids[member.group(1)] = following
        following += 1
    if "kButtonId_A" not in ids:
        raise Bail("`enum ButtonId` parsed without kButtonId_A")
    return ids


def build_definition(values: dict[str, dict], entries: list[dict],
                     revision: str | None, report: Report) -> str:
    grouped: dict[tuple[str, ...], list[dict]] = {}
    unresolved = 0
    for entry in entries:
        declared = values.get(entry["member"])
        if declared is None:
            unresolved += 1
            continue
        grouped.setdefault(entry["path"], []).append({**entry, **declared})
    if unresolved:
        report.note(f"definition: {unresolved} XML entries have no ConfigValue "
                    "declaration (computed or container members)")

    lines = [
        "# Generated by support/parse_config.py. Do not edit by hand.",
        f"# Cemu revision: {revision or 'unknown'}",
        "",
        "[meta]",
        'emulator = "cemu"',
        f'source_revision = "{revision or "unknown"}"',
        "",
        f"[bool_style.{BOOL_STYLE}]",
        'true = "true"',
        'false = "false"',
        "",
        target_header("settings"),
        'format = "xml"',
        f"path = {toml_string(SETTINGS_FILE)}",
        "",
    ]
    # XML nests deeper than an INI, which is the whole reason targets recurse.
    for path in sorted(grouped):
        for depth in range(1, len(path) + 1):
            header = target_header("settings", *path[:depth])
            if header not in lines:
                lines.append(header)
    lines.append("")

    emitted = 0
    for path in sorted(grouped):
        slug = slugify("-".join(path) or "general")
        lines += [f"[sections.{toml_key(slug)}]",
                  f"title = {toml_string(' / '.join(path) or 'General')}",
                  f"target = {toml_string(target_id('settings', *path))}", ""]
        used: set[str] = set()
        for entry in sorted(grouped[path], key=lambda e: e["name"].lower()):
            key = snake_case(entry["name"])
            if key in used:
                continue
            used.add(key)
            kind = cemu_type(entry["ctype"])
            emit = {"key": entry["name"]}
            if kind == "bool":
                emit["bool_style"] = BOOL_STYLE
            lines.append(f"[[sections.{toml_key(slug)}.options]]")
            lines.append(f"key = {toml_string(key)}")
            lines.append(f"title = {toml_string(humanize(entry['name']))}")
            lines.append(f"type = {toml_string(kind)}")
            if isinstance(entry["default"], (bool, int, float, str)):
                lines.append(f"default = {toml_value(entry['default'])}")
            lines.append(f"emit = {toml_value(emit)}")
            lines.append("")
            emitted += 1
    report.note(f"definition: {emitted} options across {len(grouped)} sections")
    return "\n".join(lines).rstrip() + "\n"


def cemu_type(ctype: str) -> str:
    simple = ctype.split("::")[-1].strip()
    if simple == "bool":
        return "bool"
    if simple in ("std::string", "string", "fs::path"):
        return "string"
    if simple in ("float", "double"):
        return "float"
    if re.fullmatch(r"u?int\d*|sint\d+|uint\d+|size_t|long", simple):
        return "int"
    return "int"


def build_actions(hotkeys: dict[str, str | None], revision: str | None,
                  report: Report) -> str:
    lines = [
        "# Generated by support/parse_config.py. Do not edit by hand.",
        f"# Cemu revision: {revision or 'unknown'}",
        "#",
        "# Cemu stores each hotkey as two space-separated integers in one element:",
        "# a wxWidgets keycode and a controller button index. `encoding` names that",
        "# pairing so the emitter writes the keyboard half and leaves the pad half",
        "# at Cemu's own -1 sentinel.",
        "",
        target_header("settings"),
        'format = "xml"',
        f"path = {toml_string(SETTINGS_FILE)}",
        "",
        target_header("settings", "Hotkeys"),
        "",
        "[actions]",
        f'target = {toml_string(target_id("settings", "Hotkeys"))}',
        # Cemu is wxWidgets, so it reads the keyboard through window messages.
        'transport = "win32-message"',
        'keynames = "wxwidgets"',
        'encoding = "wx-keycode-pair"',
        "",
        "[actions.map]",
    ]
    resolved = 0
    for action, name in sorted(ACTION_VOCABULARY.items()):
        if name not in hotkeys:
            report.error(f"action `{action}`: `{name}` is not a Cemu hotkey")
            continue
        entry = {"name": name}
        if hotkeys[name]:
            entry["wx_key"] = hotkeys[name]
        lines.append(f"{action} = {toml_value(entry)}")
        resolved += 1
    missing = sorted({"load_state", "save_state", "pause", "frame_advance", "reset"})
    report.note(f"actions: {resolved} of {len(ACTION_VOCABULARY)} mapped; Cemu has no "
                f"hotkey for {', '.join(missing)}")
    report.warn("actions: Cemu supports 4 of the 9 vocabulary actions; the rest have "
                "no equivalent and are omitted rather than faked")
    return "\n".join(lines).rstrip() + "\n"


def build_input(ids: dict[str, int], revision: str | None, report: Report) -> str:
    lines = [
        "# Generated by support/parse_config.py. Do not edit by hand.",
        f"# Cemu revision: {revision or 'unknown'}",
        "#",
        "# Cemu keeps one profile file per port, and its binding keys are the",
        "# ordinals of VPADController::ButtonId rather than names.",
        "",
    ]
    for port in range(8):
        lines += [target_header(f"profile-{port}"),
                  'format = "xml"',
                  f'path = {toml_string(f"controllerProfiles/controller{port}.xml")}',
                  "",
                  target_header(f"profile-{port}", "emulated_controller"),
                  target_header(f"profile-{port}", "emulated_controller", "controller"),
                  target_header(f"profile-{port}", "emulated_controller", "controller",
                                "mappings"),
                  ""]

    lines += ["[input]", 'dialect = "cemu"', 'device = "SDL/{index}"', ""]

    bound = 0
    for layout in LAYOUTS:
        lines.append(f"[input.layouts.{toml_key(layout)}]")
        for element, enumerator in sorted(VPAD_BINDINGS.items()):
            ordinal = ids.get(enumerator)
            if ordinal is None:
                report.error(f"input: `{enumerator}` is not in VPADController::ButtonId")
                continue
            if element not in SDL_PROJECTABLE:
                report.warn(f"input: `{element}` has no SDL projection in stone.rs; "
                            f"omitted from {layout}")
                continue
            lines.append(f"{toml_key(element)} = {toml_string(str(ordinal))}")
            bound += 1
        lines.append("")

    for port in range(8):
        lines.append(f"[platforms.NINTENDO_WIIU.ports.{port}]")
        lines.append(f"target = {toml_string(target_id(f'profile-{port}', 'emulated_controller', 'controller', 'mappings'))}")
        lines.append(f"layouts = {toml_value(list(LAYOUTS))}")
        for layout, kind in LAYOUTS.items():
            lines.append(f"emit.{toml_key(layout)} = " + toml_value([
                {"target": target_id(f"profile-{port}", "emulated_controller"),
                 "key": "type", "value": kind}
            ]))
        lines.append("")
    report.note(f"input: {bound} bindings across {len(LAYOUTS)} layouts, 8 ports")
    report.note("input: bindings are ButtonId ordinals; a reordered enum upstream "
                "silently changes every emitted number, so regenerate on version bumps")
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
    parser.add_argument("--cemu", required=True, type=Path)
    parser.add_argument("--out", type=Path, default=Path(__file__).parent.parent / "config")
    parser.add_argument("--strict", action="store_true")
    parser.add_argument("--dry-run", action="store_true")
    arguments = parser.parse_args(argv)

    root = arguments.cemu.expanduser().resolve()
    if not (root / "src/config").is_dir():
        print(f"error: {root} does not look like a Cemu checkout", file=sys.stderr)
        return 2

    report = Report()
    revision = revision_of(root)
    try:
        values = parse_config_values(root, report)
        entries = parse_xml_layout(root, report)
        hotkeys = parse_hotkeys(root, report)
        ids = parse_button_ids(root, report)
        documents = {
            "cemu.toml": build_definition(values, entries, revision, report),
            "actions.toml": build_actions(hotkeys, revision, report),
            "input.toml": build_input(ids, revision, report),
        }
    except Bail as error:
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
