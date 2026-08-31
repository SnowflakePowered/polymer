#!/usr/bin/env python3
"""Generate the Dolphin plugin's configuration documents from Dolphin's source.

Dolphin declares its entire configuration surface as data: `Config::Info<T>`
constants carrying (system, section, key, default), an INI filename per system,
a hotkey label table, and a controller group/input table. That is enough to
derive the plugin's definition document mechanically, so nothing is hand-copied
and an upstream rename fails here instead of silently emitting dead keys.

Outputs, into --out (default ../config):

    dolphin.toml    targets                            (Core/Config/*.cpp)
    sections/*.toml one rooted document per config system (Core/Config/*.cpp)
    actions.toml    action map                     (Core/HotkeyManager.{h,cpp})
    input.toml      port modes + binding keys      (Core/HW/GCPadEmu.{h,cpp})

Usage:

    python parse_config.py --dolphin E:/coding/dolphin
    python parse_config.py --dolphin ../../../dolphin --systems all --strict
"""

from __future__ import annotations

import argparse
import ast
import re
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "support"))
import consolespec
import sdlpad
import cppconfig
from cppconfig import *  # noqa: F403 — shared C++/TOML machinery

FILESYSTEM_OPTION_KEYS = {
    "agp_cart_a_path", "agp_cart_b_path", "gci_folder_a_path",
    "gci_folder_a_path_override", "gci_folder_b_path",
    "gci_folder_b_path_override", "memcard_a_path", "memcard_b_path",
    "perf_map_dir", "bios", "gb_player_rom", "rom1", "rom2", "rom3",
    "rom4", "saves_path", "current_state_path", "dump_path", "load_path",
    "nand_root_path", "resource_pack_path", "skylanders_collection_path",
    "wfs_path", "wii_sd_card_path", "wii_sd_card_sync_folder",
}


# Snowglass's global action vocabulary → the Dolphin hotkey that implements it.
# `fallback` is used when Dolphin ships no default binding for that hotkey; the
# script rejects a fallback that collides with any binding it parsed, so this
# table cannot silently double-bind a key.
ACTION_VOCABULARY = {
    "load_state": {"hotkey": "HK_LOAD_STATE_SLOT_SELECTED", "fallback": ("ctrl+f1", "@(Ctrl+F1)")},
    "save_state": {"hotkey": "HK_SAVE_STATE_SLOT_SELECTED", "fallback": ("ctrl+f2", "@(Ctrl+F2)")},
    "reset": {"hotkey": "HK_RESET", "fallback": ("ctrl+f3", "@(Ctrl+F3)")},
    "frame_advance": {"hotkey": "HK_FRAME_ADVANCE", "fallback": ("ctrl+f4", "@(Ctrl+F4)")},
    "pause": {"hotkey": "HK_PLAY_PAUSE"},
    "fast_forward": {"hotkey": "HK_TOGGLE_THROTTLE"},
    "screenshot": {"hotkey": "HK_SCREENSHOT"},
    "toggle_fullscreen": {"hotkey": "HK_FULLSCREEN"},
    # Dolphin's HK_EXIT has no default binding and quits the whole application;
    # stopping emulation is what the shell means by `exit`.
    "exit": {"hotkey": "HK_STOP"},
}

# consolespec binding key → the Dolphin control it drives, per inputspec layout. Each
# entry is validated twice: against the group/input tables parsed out of Dolphin's
# controller sources, so an upstream rename is an error rather than a binding that
# writes to nothing, and against the element vocabulary consolespec expands for
# the layout, so a binding for something the device does not have is an error too.
# consolespec models a Wii Remote and a Wii Remote with a nunchuk as two
# layouts, while Dolphin models one controller with a swappable extension. Each
# layout still declares its bindings in full: attaching the nunchuk adds a
# second accelerometer, which forces the remote's own to say which side it is
# on, so the two layouts do not agree on what to call it. Inheriting the first
# table into the second would carry keys the second device does not have.
WII_REMOTE_BINDINGS = {
    "button.a": "Buttons/A",
    "button.b": "Buttons/B",
    # The remote's numbered buttons: consolespec's `0` is the one printed "1".
    "button.0": "Buttons/1",
    "button.1": "Buttons/2",
    "button.select": "Buttons/-",
    "button.start": "Buttons/+",
    "directional.n": "D-Pad/Up",
    "directional.s": "D-Pad/Down",
    "directional.w": "D-Pad/Left",
    "directional.e": "D-Pad/Right",
    "pointer.x+": "IR/Right",
    "pointer.x-": "IR/Left",
    "pointer.y+": "IR/Up",
    "pointer.y-": "IR/Down",
    "analog.accelerometer.x+": "IMUAccelerometer/Right",
    "analog.accelerometer.x-": "IMUAccelerometer/Left",
    "analog.accelerometer.y+": "IMUAccelerometer/Forward",
    "analog.accelerometer.y-": "IMUAccelerometer/Backward",
    "analog.accelerometer.z+": "IMUAccelerometer/Up",
    "analog.accelerometer.z-": "IMUAccelerometer/Down",
    "analog.gyroscope.x+": "IMUGyroscope/Roll Right",
    "analog.gyroscope.x-": "IMUGyroscope/Roll Left",
    "analog.gyroscope.y+": "IMUGyroscope/Pitch Up",
    "analog.gyroscope.y-": "IMUGyroscope/Pitch Down",
    "analog.gyroscope.z+": "IMUGyroscope/Yaw Right",
    "analog.gyroscope.z-": "IMUGyroscope/Yaw Left",
}

# The remote's own accelerometer, once a nunchuk gives the device a second one.
# consolespec aligns the remote's `right` and the nunchuk's `left`, so neither
# layout's accelerometer binding path works in the other's table.
WII_REMOTE_ACCELEROMETER_ALIGNED = {
    key.replace("analog.accelerometer.", "analog.accelerometer.right."): control
    for key, control in WII_REMOTE_BINDINGS.items()
    if key.startswith("analog.accelerometer.")
}

WII_NUNCHUK_BINDINGS = {
    **{
        key: control
        for key, control in WII_REMOTE_BINDINGS.items()
        if not key.startswith("analog.accelerometer.")
    },
    **WII_REMOTE_ACCELEROMETER_ALIGNED,
    "button.c": "Nunchuk/Buttons/C",
    "button.z": "Nunchuk/Buttons/Z",
    "analog.stick.left.y+": "Nunchuk/Stick/Up",
    "analog.stick.left.y-": "Nunchuk/Stick/Down",
    "analog.stick.left.x-": "Nunchuk/Stick/Left",
    "analog.stick.left.x+": "Nunchuk/Stick/Right",
    # The nunchuk carries an accelerometer of its own, which the Wii Remote
    # layout has no element for at all.
    "analog.accelerometer.left.x+": "Nunchuk/IMUAccelerometer/Right",
    "analog.accelerometer.left.x-": "Nunchuk/IMUAccelerometer/Left",
    "analog.accelerometer.left.y+": "Nunchuk/IMUAccelerometer/Forward",
    "analog.accelerometer.left.y-": "Nunchuk/IMUAccelerometer/Backward",
    "analog.accelerometer.left.z+": "Nunchuk/IMUAccelerometer/Up",
    "analog.accelerometer.left.z-": "Nunchuk/IMUAccelerometer/Down",
}

GAMECUBE_BINDINGS = {
    "button.a": "Buttons/A",
    "button.b": "Buttons/B",
    "button.x": "Buttons/X",
    "button.y": "Buttons/Y",
    "button.z": "Buttons/Z",
    "button.start": "Buttons/Start",
    "directional.n": "D-Pad/Up",
    "directional.s": "D-Pad/Down",
    "directional.w": "D-Pad/Left",
    "directional.e": "D-Pad/Right",
    "button.l": "Triggers/L",
    "button.r": "Triggers/R",
    "trigger.left": "Triggers/L-Analog",
    "trigger.right": "Triggers/R-Analog",
    "analog.stick.left.y+": "Main Stick/Up",
    "analog.stick.left.y-": "Main Stick/Down",
    "analog.stick.left.x-": "Main Stick/Left",
    "analog.stick.left.x+": "Main Stick/Right",
    "analog.stick.right.y+": "C-Stick/Up",
    "analog.stick.right.y-": "C-Stick/Down",
    "analog.stick.right.x-": "C-Stick/Left",
    "analog.stick.right.x+": "C-Stick/Right",
}


# The Dolphin backends snowglass has a capture hook for, by the CONFIG_NAME each
# one writes into Dolphin.ini. Nothing in the generated document depends on this:
# mapping a backend onto hooks — and rejecting one that maps to nothing — is the
# TS module's job. It is reported so the author knows which names to cover.
SUPPORTED_BACKENDS = ("Vulkan", "D3D", "D3D12", "OGL")
BACKEND_LABELS = {
    "Vulkan": "Vulkan",
    "D3D": "Direct3D 11",
    "D3D12": "Direct3D 12",
    "OGL": "OpenGL",
}
GFX_BACKEND_KEY = ("Main", "Core", "GFXBackend")
SPLIT_SUBSECTION_SYSTEMS = {"GFX"}

# SDL exposes pointer coordinates as the two legacy axes after its six standard
# gamepad axes. Motion controls are named by SDLGamepad's sensor prefix and the
# direction strings in SDL_AXES_ACCELEROMETER / SDL_AXES_GYRO.
SDL_POINTER_AND_MOTION_MAPPING = {
    "pointer.x-": "Axis 6-",
    "pointer.x+": "Axis 6+",
    "pointer.y-": "Axis 7-",
    "pointer.y+": "Axis 7+",
    "analog.accelerometer.x-": "Accel Left",
    "analog.accelerometer.x+": "Accel Right",
    "analog.accelerometer.y-": "Accel Backward",
    "analog.accelerometer.y+": "Accel Forward",
    "analog.accelerometer.z-": "Accel Down",
    "analog.accelerometer.z+": "Accel Up",
    "analog.gyroscope.x-": "Gyro Roll Left",
    "analog.gyroscope.x+": "Gyro Roll Right",
    "analog.gyroscope.y-": "Gyro Pitch Down",
    "analog.gyroscope.y+": "Gyro Pitch Up",
    "analog.gyroscope.z-": "Gyro Yaw Left",
    "analog.gyroscope.z+": "Gyro Yaw Right",
    "analog.accelerometer.left.x-": "Accel L Left",
    "analog.accelerometer.left.x+": "Accel L Right",
    "analog.accelerometer.left.y-": "Accel L Backward",
    "analog.accelerometer.left.y+": "Accel L Forward",
    "analog.accelerometer.left.z-": "Accel L Down",
    "analog.accelerometer.left.z+": "Accel L Up",
    "analog.accelerometer.right.x-": "Accel R Left",
    "analog.accelerometer.right.x+": "Accel R Right",
    "analog.accelerometer.right.y-": "Accel R Backward",
    "analog.accelerometer.right.y+": "Accel R Forward",
    "analog.accelerometer.right.z-": "Accel R Down",
    "analog.accelerometer.right.z+": "Accel R Up",
}

# Config systems that describe controllers rather than settings. input.toml owns
# their files and their port keys, so the definition document leaves them alone —
# otherwise both documents declare a root for GCPadNew.ini and both write
# `Wiimote1/Source`, which lowering rejects as duplicates.
INPUT_SYSTEMS = ("GCPad", "WiiPad", "GCKeyboard")

# Dolphin writes booleans through `ValueToString(bool)` in Common/StringUtil.cpp,
# which is `value ? "True" : "False"` for every key. The document declares that
# spelling once (§3.4 of the crate design) and every option refers to it.
BOOL_STYLE = "upper"


# --------------------------------------------------------------------------
# Dolphin structures
# --------------------------------------------------------------------------


@dataclass
class InfoDef:
    system: str
    section: str
    key: str
    ctype: str
    default_expression: str | None
    cpp_name: str
    origin: str


def parse_constants(root: Path, report: Report) -> Scope:
    """Named compile-time constants, for defaults that reference them.

    Headers are read without preprocessing: a constant defined differently in
    two #ifdef branches becomes ambiguous and is dropped, which is the safe
    reading. The config sources are preprocessed, because that is where
    platform-conditional defaults such as DEFAULT_CPU_THREAD live.
    """
    declarations: list[tuple[str, str]] = []
    for path in sorted(root.glob("Source/Core/**/*.h")):
        try:
            declarations += collect_constants(
                strip_comments(path.read_text(encoding="utf-8", errors="replace"))
            )
        except OSError as error:
            report.warn(f"unreadable {path}: {error}")
    for path in sorted((root / "Source/Core/Core/Config").glob("*.cpp")):
        declarations += collect_constants(load_source(path, report))
    return resolve_constants(declarations, Scope())


def parse_config_systems(root: Path, report: Report) -> dict[str, str]:
    """System name → config-root-relative INI path, via Dolphin's own chain."""
    loader = load_source(root / "Source/Core/Core/ConfigLoaders/BaseConfigLoader.cpp", report)
    system_to_index = dict(
        re.findall(r"\{\s*Config::System::(\w+)\s*,\s*(F_\w+_IDX)\s*\}", loader)
    )
    if not system_to_index:
        raise Bail("BaseConfigLoader.cpp no longer contains a system_to_ini table")

    file_util = load_source(root / "Source/Core/Common/FileUtil.cpp", report)
    index_to_macro = dict(
        re.findall(r"s_user_paths\[(F_\w+_IDX)\]\s*=\s*s_user_paths\[D_CONFIG_IDX\]\s*\+\s*(\w+)\s*;",
                   file_util)
    )
    paths_header = strip_comments(
        (root / "Source/Core/Common/CommonPaths.h").read_text(encoding="utf-8", errors="replace")
    )
    macro_to_name = dict(re.findall(r"#define\s+(\w+)\s+\"([^\"]+)\"", paths_header))
    config_dir = macro_to_name.get("CONFIG_DIR")
    if not config_dir:
        raise Bail("CommonPaths.h no longer defines CONFIG_DIR")

    systems: dict[str, str] = {}
    for system, index in sorted(system_to_index.items()):
        macro = index_to_macro.get(index)
        if macro is None:
            report.note(f"System::{system} has no Config-relative path ({index}); skipped")
            continue
        filename = macro_to_name.get(macro)
        if filename is None:
            report.note(f"System::{system} macro {macro} is not a filename; skipped")
            continue
        systems[system] = f"{config_dir}/{filename}"
    return systems


def parse_infos(root: Path, report: Report) -> list[InfoDef]:
    """Collect every `Info<T>` definition, in both forms Dolphin uses.

    Most are named constants — `const Info<bool> GFX_VSYNC{{…}, false};` — but
    per-channel settings are built as anonymous array elements inside an
    accessor, `Info<bool>{{System::Main, "Core", "AdapterRumble0"}, true},`.
    Matching only the named form silently drops 41 of them, `SIDevice0-3`
    among them.
    """
    infos: list[InfoDef] = []
    pattern = re.compile(r"\bInfo\s*<")
    for path in sorted((root / "Source/Core/Core/Config").glob("*.cpp")):
        text = load_source(path, report)
        for found in pattern.finditer(text):
            angle = found.end() - 1
            try:
                type_end = match_braces(text, angle, "<", ">")
            except Bail:
                report.warn(f"{path.name}: unbalanced Info<> near offset {angle}")
                continue
            ctype = text[angle + 1: type_end - 1].strip()
            # `Info<T> NAME{` declares one; `Info<T>{` builds one in place.
            # Anything else — `Info<T>&` in a signature, `Info<T>*` in an array
            # type — is a mention rather than a definition.
            named = re.match(r"\s*(\w+)\s*\{", text[type_end:])
            anonymous = None if named else re.match(r"\s*\{", text[type_end:])
            if not named and not anonymous:
                continue
            offset = (named or anonymous).end()
            brace = type_end + offset - 1
            label = named.group(1) if named else None
            try:
                end = match_braces(text, brace)
            except Bail:
                report.warn(f"{path.name}: unbalanced initializer for {label or ctype}")
                continue
            arguments = split_top_level(text[brace + 1: end - 1])
            if not arguments:
                continue
            location = re.match(
                r"\{\s*System::(\w+)\s*,\s*(\"(?:[^\"\\]|\\.)*\")\s*,\s*(\"(?:[^\"\\]|\\.)*\")\s*\}",
                arguments[0].strip(),
            )
            if not location:
                if named:
                    report.note(f"{path.name}: {label} has a non-literal location")
                continue
            section = parse_string_literal(location.group(2)) or ""
            key = parse_string_literal(location.group(3)) or ""
            infos.append(InfoDef(
                system=location.group(1),
                section=section,
                key=key,
                ctype=ctype,
                default_expression=arguments[1] if len(arguments) > 1 else None,
                cpp_name=label or key,
                origin=path.name,
            ))
    if not infos:
        raise Bail("no Config::Info definitions found; check --dolphin")
    duplicates = len(infos) - len({(i.system, i.section, i.key) for i in infos})
    if duplicates:
        report.warn(f"{duplicates} Config::Info definitions share a (system, section, key)")
    return infos


# --------------------------------------------------------------------------
# Definition document
# --------------------------------------------------------------------------


def build_option(info: InfoDef, enums: dict[str, list[EnumDef]], scope: Scope,
                 report: Report) -> dict | None:
    """Translate one Config::Info into a definition-document option."""
    ctype = base_type(info.ctype)
    target = target_id(slugify(info.system), info.section)
    option: dict = {
        "key": snake_case(info.key),
        "title": humanize(info.key),
        "emit": {"target": target, "key": info.key},
    }

    if ctype == "bool":
        option["type"] = "bool"
        option["emit"]["bool_style"] = BOOL_STYLE
        default = try_default(info, scope, report)
        if isinstance(default, bool):
            option["default"] = default
        elif default is not None:
            report.warn(f"{info.cpp_name}: bool default is {default!r}; omitted")
        return option

    if ctype in INT_TYPES:
        radix, width = INT_TYPES[ctype]
        option["type"] = "int"
        if radix != "dec":
            option["emit"]["int_style"] = f"0x{{:0{width}x}}"
        default = try_default(info, scope, report)
        if isinstance(default, bool):
            default = int(default)
        if isinstance(default, int):
            option["default"] = default
        return option

    if ctype in FLOAT_TYPES:
        option["type"] = "float"
        default = try_default(info, scope, report)
        if isinstance(default, (int, float)) and not isinstance(default, bool):
            option["default"] = float(default)
        return option

    if ctype in STRING_TYPES:
        option["type"] = ("path" if option["key"] in FILESYSTEM_OPTION_KEYS
                          else "string")
        default = try_default(info, scope, report)
        if isinstance(default, str):
            option["default"] = default
        return option

    enum = resolve_enum(ctype, enums, report, info.origin)
    if enum is None:
        report.note(f"{info.origin}: {info.cpp_name} has unsupported type `{ctype}`; skipped")
        return None

    radix, width = integer_repr(enum.underlying)
    option["type"] = "select"
    option["domain"] = [
        {
            "key": name,
            "label": humanize(name),
            "value": format_integer(value, radix, width),
        }
        for name, value in enum.members
    ]
    default = try_default(info, scope, report)
    if isinstance(default, int) and not isinstance(default, bool):
        for name, value in enum.members:
            if value == default:
                option["default"] = name
                break
        else:
            report.warn(f"{info.cpp_name}: default {default} is outside enum {enum.name}")
    return option


def try_default(info: InfoDef, scope: Scope, report: Report):
    if info.default_expression is None:
        return None
    try:
        return evaluate_expression(info.default_expression, scope)
    except Unresolved as error:
        report.note(f"{info.origin}: {info.cpp_name} default not constant ({error})")
        return None


def parse_system_titles(root: Path, report: Report) -> dict[str, str]:
    """Dolphin's own display name per config system, for section titles."""
    text = load_source(root / "Source/Core/Common/Config/Config.cpp", report)
    found = re.search(r"system_to_name\s*=\s*\{", text)
    if not found:
        raise Bail("Config.cpp no longer defines system_to_name")
    body = text[found.end(): match_braces(text, found.end() - 1) - 1]
    titles = {}
    for system, title in re.findall(r"\{\s*System::(\w+)\s*,\s*(\"(?:[^\"\\]|\\.)*\")\s*\}", body):
        literal = parse_string_literal(title)
        if literal:
            titles[system] = literal
    if not titles:
        raise Bail("system_to_name parsed empty")
    return titles


def parse_bool_style(root: Path, report: Report) -> tuple[str, str]:
    """The two strings Dolphin writes for a bool, from ValueToString(bool)."""
    text = strip_comments(
        (root / "Source/Core/Common/StringUtil.cpp").read_text(encoding="utf-8", errors="replace")
    )
    found = re.search(
        r"ValueToString\s*\(\s*bool\s+\w+\s*\)\s*\{\s*return\s+\w+\s*\?\s*"
        r"(\"(?:[^\"\\]|\\.)*\")\s*:\s*(\"(?:[^\"\\]|\\.)*\")\s*;",
        text,
    )
    if not found:
        raise Bail("StringUtil.cpp no longer defines ValueToString(bool) as a ternary")
    true_text = parse_string_literal(found.group(1))
    false_text = parse_string_literal(found.group(2))
    if true_text is None or false_text is None:
        raise Bail("ValueToString(bool) does not return string literals")
    if (true_text, false_text) != ("True", "False"):
        report.warn(f"bool style is now {true_text!r}/{false_text!r}; "
                    f"the `{BOOL_STYLE}` style name no longer describes it")
    return true_text, false_text


def parse_video_backends(root: Path, report: Report) -> list[str]:
    """Each video backend's CONFIG_NAME, the string Dolphin.ini stores."""
    names: list[str] = []
    for header in sorted(root.glob("Source/Core/VideoBackends/*/VideoBackend.h")):
        text = strip_comments(header.read_text(encoding="utf-8", errors="replace"))
        found = re.search(r"CONFIG_NAME\s*=\s*(\"(?:[^\"\\]|\\.)*\")", text)
        if not found:
            report.note(f"{header.parent.name}: no CONFIG_NAME; skipped")
            continue
        literal = parse_string_literal(found.group(1))
        if literal:
            names.append(literal)
    if not names:
        raise Bail("no VideoBackends CONFIG_NAME constants found")
    return names


def build_gfx_backend_option(option: dict, backends: list[str], report: Report) -> dict:
    """Turn GFXBackend into a select over the backends in this Dolphin build.

    Which backends a Dolphin build actually ships is a property of the install —
    D3D is Windows-only, Metal is macOS-only, and any of them can be compiled
    out — while upstream computes the default at startup rather than storing it
    as a constant. The generator already reads the concrete build's backend
    sources, so expose that set directly and let onLaunch choose one.
    """
    supported = [name for name in backends if name in SUPPORTED_BACKENDS]
    if not supported:
        raise Bail("no Dolphin video backend has a snowglass capture hook")
    option["type"] = "select"
    option["domain"] = [
        {"key": name, "label": BACKEND_LABELS[name], "value": name}
        for name in supported
    ]
    option.pop("default", None)
    report.note(f"backends: {len(backends)} in the source tree "
                f"({', '.join(backends)}); {len(supported)} capturable "
                f"({', '.join(supported)}); the TS module maps these onto hooks")
    return option


def build_definition(infos: list[InfoDef], systems: dict[str, str], titles: dict[str, str],
                     enums: dict[str, list[EnumDef]], scope: Scope, backends: list[str],
                     bool_style: tuple[str, str], claimed: set[tuple[str, str]],
                     wanted: set[str], revision: str | None,
                     report: Report) -> dict[str, str]:
    grouped: dict[tuple[str, str], list[InfoDef]] = {}
    skipped = 0
    for info in infos:
        if info.system not in wanted:
            continue
        if info.system not in systems:
            report.warn(f"System::{info.system} has no INI path; {info.cpp_name} skipped")
            continue
        if (target_id(slugify(info.system), info.section), info.key) in claimed:
            skipped += 1
            continue
        grouped.setdefault((info.system, info.section), []).append(info)
    if skipped:
        report.note(f"definition: {skipped} settings omitted because a port owns them "
                    "(input.toml writes them per port)")

    main_lines = [
        "# Generated by support/parse_config.py. Do not edit by hand.",
        f"# Dolphin revision: {revision or 'unknown'}",
        "",
        "[meta]",
        'emulator = "dolphin"',
        f'source_revision = "{revision or "unknown"}"',
        "",
        f"[bool_style.{BOOL_STYLE}]",
        f"true = {toml_string(bool_style[0])}",
        f"false = {toml_string(bool_style[1])}",
        "",
    ]

    # One root per file, with each INI section nested inside it. The node name is
    # the table key, so a section needs no body at all.
    for system in sorted({system for system, _ in grouped}):
        main_lines.append(target_header(slugify(system)))
        main_lines.append(
            f'output = {{ format = "ini", path = {toml_string(systems[system])} }}')
        main_lines.append("")
        for other, section in sorted(grouped):
            if other == system:
                main_lines.append(target_header(slugify(system), section))
        main_lines.append("")

    emitted = 0
    backend_path: str | None = None
    system_lines: dict[str, list[str]] = {}
    child_lines: dict[str, list[str]] = {}
    for (system, section), members in sorted(grouped.items()):
        # Sections nest by config system, so an option's stored path reads
        # `graphics.hardware.vsync`. The key stays a slug because it is the
        # persistence prefix; the target id mirrors the file it writes to.
        section_key = f"{slugify(system)}.{slugify(section)}"
        target = target_id(slugify(system), section)
        options: list[dict] = []
        used: set[str] = set()
        for info in sorted(members, key=lambda item: item.key.lower()):
            option = build_option(info, enums, scope, report)
            if option is None:
                continue
            if (info.system, info.section, info.key) == GFX_BACKEND_KEY:
                option = build_gfx_backend_option(option, backends, report)
                backend_path = f"{section_key}.{option['key']}"
            key = option["key"]
            if key in used:
                suffix = 2
                while f"{key}_{suffix}" in used:
                    suffix += 1
                report.note(f"duplicate option key `{key}` in {section_key}; renamed")
                key = f"{key}_{suffix}"
                option["key"] = key
            used.add(key)
            options.append(option)
        if not options:
            continue
        system_slug = slugify(system)
        root_lines = system_lines.setdefault(system, [
            "# Generated by support/parse_config.py. Do not edit by hand.",
            f"# Dolphin revision: {revision or 'unknown'}",
            "",
            f"[sections.{toml_key(system_slug)}]",
            f"title = {toml_string(titles.get(system, humanize(system)))}",
            "",
        ])
        section_slug = slugify(section)
        if system in SPLIT_SUBSECTION_SYSTEMS:
            filename = f"sections/{system_slug}/{section_slug}.toml"
            if filename in child_lines:
                raise Bail(f"definition: duplicate subsection output name {filename!r}")
            lines = [
                "# Generated by support/parse_config.py. Do not edit by hand.",
                f"# Dolphin revision: {revision or 'unknown'}",
                "",
            ]
            child_lines[filename] = lines
        else:
            lines = root_lines
        lines.append(f"[sections.{toml_key(system_slug)}.{toml_key(section_slug)}]")
        lines.append(f"title = {toml_string(section)}")
        lines.append(f"target = {toml_string(target)}")
        lines.append("")
        for option in options:
            lines.append(f"[[sections.{toml_key(system_slug)}."
                         f"{toml_key(section_slug)}.options]]")
            for name in ("key", "title", "type"):
                lines.append(f"{name} = {toml_value(option[name])}")
            if "domain" in option:
                lines.append(f"domain = {toml_value(option['domain'])}")
            if "default" in option:
                lines.append(f"default = {toml_value(option['default'])}")
            # `target` is inherited from the section, so state it only when an
            # option writes somewhere else.
            emit = {k: v for k, v in option["emit"].items()
                    if k != "target" or v != target}
            lines.append(f"emit = {toml_value(emit)}")
            lines.append("")
            emitted += 1

    if backend_path is None and "Main" in wanted:
        report.warn("no GFXBackend option found; the TS module has nothing to map onto hooks")

    report.note(f"definition: {emitted} options across {len(grouped)} sections")
    documents = {
        filename: "\n".join(lines).rstrip() + "\n"
        for filename, lines in child_lines.items()
    }
    documents.update({
        f"sections/{slugify(system)}.toml": "\n".join(lines).rstrip() + "\n"
        for system, lines in system_lines.items()
    })
    documents["dolphin.toml"] = "\n".join(main_lines).rstrip() + "\n"
    return documents


# --------------------------------------------------------------------------
# Hotkeys → actions.toml
# --------------------------------------------------------------------------


def parse_hotkey_order(root: Path, report: Report) -> list[str]:
    header = load_source(root / "Source/Core/Core/HotkeyManager.h", report)
    found = re.search(r"\benum\s+Hotkey\s*\{", header)
    if not found:
        raise Bail("HotkeyManager.h no longer declares `enum Hotkey`")
    body = header[found.end(): match_braces(header, found.end() - 1) - 1]
    names = []
    for entry in split_top_level(body):
        member = re.match(r"^(HK_\w+)\s*(?:=.*)?$", entry.strip(), re.S)
        if member:
            names.append(member.group(1))
    if not names:
        raise Bail("`enum Hotkey` parsed empty")
    return names


def parse_hotkey_labels(text: str) -> list[str]:
    found = re.search(r"s_hotkey_labels\s*(?:=\s*)?\{", text)
    if not found:
        raise Bail("HotkeyManager.cpp no longer defines s_hotkey_labels")
    body = text[found.end(): match_braces(text, found.end() - 1) - 1].strip()
    if body.startswith("{"):
        body = body[1: match_braces(body, 0) - 1]
    labels = []
    for entry in split_top_level(body):
        literal = parse_string_literal(strip_noop_macros(entry))
        if literal is not None:
            labels.append(literal)
    return labels


def parse_hotkey_groups(text: str) -> list[tuple[str, str, str]]:
    found = re.search(r"s_groups_info\s*=\s*\{", text)
    if not found:
        raise Bail("HotkeyManager.cpp no longer defines s_groups_info")
    body = text[found.end(): match_braces(text, found.end() - 1) - 1].strip()
    if body.startswith("{"):
        body = body[1: match_braces(body, 0) - 1]
    groups = []
    for entry in split_top_level(body):
        entry = entry.strip()
        if not entry.startswith("{"):
            continue
        fields = split_top_level(entry[1: match_braces(entry, 0) - 1])
        if len(fields) < 3:
            continue
        name = parse_string_literal(strip_noop_macros(fields[0]))
        if name is None:
            continue
        groups.append((name, fields[1].strip(), fields[2].strip()))
    if not groups:
        raise Bail("s_groups_info parsed empty")
    return groups


def parse_hotkey_defaults(text: str, order: list[str], report: Report) -> dict[str, str]:
    """Every default binding, including the save/load state slot loops."""
    found = re.search(r"void\s+HotkeyManager::LoadDefaults\s*\([^)]*\)\s*\{", text)
    if not found:
        raise Bail("HotkeyManager.cpp no longer defines LoadDefaults")
    body = text[found.end(): match_braces(text, found.end() - 1) - 1]
    index_of = {name: position for position, name in enumerate(order)}
    defaults: dict[str, str] = {}

    def record(target: str, expression: str, bindings: dict[str, int]) -> None:
        name = resolve_hotkey(target, index_of, bindings)
        if name is None:
            report.note(f"unresolved hotkey target `{target.strip()}` in LoadDefaults")
            return
        value = evaluate_binding(expression, bindings)
        if value is None:
            report.note(f"unresolved default expression for {name}: {expression.strip()}")
            return
        defaults[name] = value

    call = re.compile(r"set_key_expression\s*\(")
    loop = re.compile(r"for\s*\(\s*int\s+(\w+)\s*=\s*(\d+)\s*;\s*\1\s*<\s*(\d+)\s*;\s*\1\+\+\s*\)\s*\{")

    consumed: list[tuple[int, int]] = []
    for found_loop in loop.finditer(body):
        variable = found_loop.group(1)
        start, stop = int(found_loop.group(2)), int(found_loop.group(3))
        end = match_braces(body, found_loop.end() - 1)
        inner = body[found_loop.end(): end - 1]
        consumed.append((found_loop.start(), end))
        for value in range(start, stop):
            for inner_call in call.finditer(inner):
                close = match_braces(inner, inner_call.end() - 1, "(", ")")
                arguments = split_top_level(inner[inner_call.end(): close - 1])
                if len(arguments) == 2:
                    record(arguments[0], arguments[1], {variable: value})

    for found_call in call.finditer(body):
        if any(start <= found_call.start() < end for start, end in consumed):
            continue
        close = match_braces(body, found_call.end() - 1, "(", ")")
        arguments = split_top_level(body[found_call.end(): close - 1])
        if len(arguments) == 2:
            record(arguments[0], arguments[1], {})
    return defaults


def resolve_hotkey(expression: str, index_of: dict[str, int],
                   bindings: dict[str, int]) -> str | None:
    text = expression.strip()
    if text in index_of:
        return text
    offset = re.match(r"^(HK_\w+)\s*\+\s*(\w+)$", text)
    if offset and offset.group(1) in index_of and offset.group(2) in bindings:
        position = index_of[offset.group(1)] + bindings[offset.group(2)]
        for name, value in index_of.items():
            if value == position:
                return name
    return None


def evaluate_binding(expression: str, bindings: dict[str, int]) -> str | None:
    """Render `"F9"`, `hotkey_string({...})`, and `fmt::format("F{}", i + 1)`."""
    text = expression.strip()
    literal = parse_string_literal(text)
    if literal is not None:
        return literal.strip("`")

    formatted = re.match(r"^fmt::format\s*\(", text)
    if formatted:
        close = match_braces(text, formatted.end() - 1, "(", ")")
        arguments = split_top_level(text[formatted.end(): close - 1])
        template = parse_string_literal(arguments[0]) if arguments else None
        if template is None or template.count("{}") != len(arguments) - 1:
            return None
        rendered = template
        for argument in arguments[1:]:
            try:
                value = evaluate_expression(argument, Scope(dict(bindings)))
            except Unresolved:
                return None
            rendered = rendered.replace("{}", str(value), 1)
        return rendered

    joined = re.match(r"^hotkey_string\s*\(", text)
    if joined:
        close = match_braces(text, joined.end() - 1, "(", ")")
        inner = text[joined.end(): close - 1].strip()
        if inner.startswith("{"):
            inner = inner[1: match_braces(inner, 0) - 1]
        parts = []
        for entry in split_top_level(inner):
            part = evaluate_binding(entry, bindings)
            if part is None:
                return None
            parts.append(part)
        return "@(" + "+".join(parts) + ")"
    return None


def canonical_chord(expression: str) -> str:
    """Dolphin expression → the snowglass key spec the shell synthesizes."""
    text = expression.strip()
    if text.startswith("@(") and text.endswith(")"):
        text = text[2:-1]
    parts = [part.strip().strip("`").lower() for part in text.split("+") if part.strip()]
    aliases = {"return": "return", "escape": "escape", "esc": "escape", "subtract": "subtract",
               "add": "add", "tab": "tab"}
    return "+".join(aliases.get(part, part) for part in parts)


def build_actions(root: Path, revision: str | None, report: Report) -> str:
    text = load_source(root / "Source/Core/Core/HotkeyManager.cpp", report)
    order = parse_hotkey_order(root, report)
    labels = parse_hotkey_labels(text)
    if len(labels) != len(order):
        raise Bail(f"hotkey labels ({len(labels)}) disagree with enum Hotkey ({len(order)})")
    groups = parse_hotkey_groups(text)
    defaults = parse_hotkey_defaults(text, order, report)

    index_of = {name: position for position, name in enumerate(order)}
    group_of: dict[str, str] = {}
    for name, first, last in groups:
        if first not in index_of or last not in index_of:
            report.warn(f"hotkey group `{name}` names an unknown bound; skipped")
            continue
        for position in range(index_of[first], index_of[last] + 1):
            group_of[order[position]] = name

    ini_name = {name: f"{group_of[name]}/{labels[index_of[name]]}"
                for name in order if name in group_of}
    taken = {canonical_chord(value) for value in defaults.values()}

    lines = [
        "# Generated by support/parse_config.py. Do not edit by hand.",
        f"# Dolphin revision: {revision or 'unknown'}",
        "",
        target_header("hotkeys"),
        'output = { format = "ini", path = "Config/Hotkeys.ini" }',
        "",
        target_header("hotkeys", "Hotkeys"),
        "",
        "[actions]",
        f'target = {toml_string(target_id("hotkeys", "Hotkeys"))}',
        "",
        "[[actions.emit]]",
        'key = "Device"',
        'value = "DInput/0/Keyboard Mouse"',
        "",
        "[actions.map]",
    ]

    resolved = 0
    for action, spec in sorted(ACTION_VOCABULARY.items()):
        hotkey = spec["hotkey"]
        if hotkey not in ini_name:
            report.error(f"action `{action}`: {hotkey} is not in Dolphin's hotkey table")
            continue
        expression = defaults.get(hotkey)
        if expression is None:
            fallback = spec.get("fallback")
            if fallback is None:
                report.error(f"action `{action}`: {hotkey} has no default and no fallback")
                continue
            chord, expression = fallback
            if chord in taken:
                report.error(
                    f"action `{action}`: fallback `{expression}` collides with a Dolphin default"
                )
                continue
            taken.add(chord)
            report.note(f"action `{action}`: {hotkey} is unbound upstream; using {expression}")
        else:
            chord = canonical_chord(expression)
        entry = {"expression": expression, "name": ini_name[hotkey]}
        lines.append(f"{action} = {toml_value(entry)}")
        resolved += 1

    unmapped = sorted(set(ini_name) - {spec["hotkey"] for spec in ACTION_VOCABULARY.values()})
    report.note(f"actions: {resolved} of {len(ACTION_VOCABULARY)} vocabulary entries resolved, "
                f"{len(unmapped)} Dolphin hotkeys unused")
    return "\n".join(lines).rstrip() + "\n"


# --------------------------------------------------------------------------
# GC pad → input.toml
# --------------------------------------------------------------------------


def parse_char_constants(text: str) -> dict[str, str]:
    """`static constexpr const char* NAME = "value";` declarations."""
    constants: dict[str, str] = {}
    for name, value in re.findall(
        r"(?:static\s+)?constexpr\s+const\s+char\*\s+(\w+)\s*=\s*([^;]+);", text
    ):
        literal = parse_string_literal(strip_noop_macros(value))
        if literal is not None:
            constants[name] = literal
    return constants


def add_input_names(arguments: str, resolve) -> list[str]:
    """The control name an `AddInput(...)` call contributes, if it is literal.

    Overloads differ: most take a `Translatability` first, `ModifySettingsButton`
    takes the name directly. Skipping a leading enum token covers both.
    """
    fields = split_top_level(arguments)
    if fields and re.match(r"^(?:ControllerEmu::)?Translatability::\w+$", fields[0].strip()):
        fields = fields[1:]
    if not fields:
        return []
    name = resolve(fields[0])
    return [name] if name else []


def parse_named_directions(emu: str, constants: dict[str, str]) -> list[str]:
    found = re.search(r"named_directions\[\]\s*=\s*\{([^}]*)\}", emu)
    if not found:
        raise Bail("ControllerEmu.h no longer defines named_directions")
    directions = [
        constants.get(entry.strip()) or parse_string_literal(entry)
        for entry in split_top_level(found.group(1))
    ]
    if not all(directions):
        raise Bail("named_directions references an unresolved constant")
    return [d for d in directions if d]


def parse_group_classes(root: Path, report: Report) -> dict[str, list[str]]:
    """Control names each ControllerEmu group class adds on construction.

    A stick contributes four directions and a modifier before its owner adds
    anything; a gyroscope contributes six axes and nothing else. Reading that
    from the classes keeps it out of this script as a table to maintain.
    """
    emu = load_source(root / "Source/Core/InputCommon/ControllerEmu/ControllerEmu.h", report)
    constants = parse_char_constants(emu)
    directions = parse_named_directions(emu, constants)

    def resolve(expression: str) -> str | None:
        text = strip_noop_macros(expression).strip()
        return constants.get(text) or parse_string_literal(text)

    own: dict[str, list[str]] = {}
    bases: dict[str, str] = {}
    directory = root / "Source/Core/InputCommon/ControllerEmu/ControlGroup"
    for path in sorted(directory.glob("*.h")):
        for derived, base in re.findall(
            r"\bclass\s+(\w+)\s*:\s*public\s+(?:ControllerEmu::)?(\w+)",
            load_source(path, report),
        ):
            bases[derived] = base
    for path in sorted(directory.glob("*.cpp")):
        text = load_source(path, report)
        for found in re.finditer(r"\b(\w+)::\1\s*\(", text):
            name = found.group(1)
            brace = text.find("{", match_braces(text, found.end() - 1, "(", ")"))
            if brace < 0:
                continue
            body = text[brace + 1: match_braces(text, brace) - 1]
            controls = own.setdefault(name, [])
            for loop in re.finditer(
                r"for\s*\(\s*(?:const\s+)?auto&?\s*\w+\s*:\s*(\w+)\s*\)\s*\{?\s*AddInput", body
            ):
                if loop.group(1) == "named_directions":
                    controls.extend(directions)
            for call in re.finditer(r"(?<![>.])\bAddInput\s*\(", body):
                close = match_braces(body, call.end() - 1, "(", ")")
                controls.extend(add_input_names(body[call.end(): close - 1], resolve))
    if "AnalogStick" not in own:
        raise Bail("no ControllerEmu group classes parsed")

    # A base class runs first, so its inputs come first: an OctagonAnalogStick is
    # an AnalogStick that adds nothing of its own.
    def inherited(name: str, seen: frozenset[str] = frozenset()) -> list[str]:
        if name in seen:
            return []
        base = bases.get(name)
        prefix = inherited(base, seen | {name}) if base else []
        return prefix + own.get(name, [])

    return {name: inherited(name) for name in set(own) | set(bases)}


def parse_controller(root: Path, header: Path, source: Path, constructor: str,
                     group_classes: dict[str, list[str]], report: Report,
                     extra_constants: dict[str, str] | None = None):
    """One emulated controller's `Group/Control` binding keys and its settings.

    Settings are the emulator's own per-device options — Dolphin's sideways Wii
    Remote, its battery level — declared as `AddSetting(&value, {details}, …)`.
    `NumericSettingDetails` takes the ini name first and the UI name fourth.
    """
    emu = load_source(root / "Source/Core/InputCommon/ControllerEmu/ControllerEmu.h", report)
    constants = parse_char_constants(emu)
    constants.update(parse_char_constants(load_source(header, report)))
    constants.update(extra_constants or {})
    directions = parse_named_directions(emu, constants)
    text = load_source(source, report)

    def resolve(expression: str) -> str | None:
        candidate = strip_noop_macros(expression).strip()
        return constants.get(candidate) or parse_string_literal(candidate)

    start = re.search(rf"{constructor}\s*\([^)]*\)[^{{]*\{{", text)
    if not start:
        raise Bail(f"{source.name} no longer defines `{constructor}`")
    body = text[start.end(): match_braces(text, start.end() - 1) - 1]

    controls: list[str] = []
    groups: dict[str, str] = {}
    # Brace-match the argument list rather than regexing it: `Cursor(IR_GROUP,
    # _trans("Point"))` nests parentheses, and a non-greedy match stops inside.
    for found in re.finditer(r"(m_\w+)\s*=\s*new\s+ControllerEmu::(\w+)\s*\(", body):
        member, kind = found.group(1), found.group(2)
        close = match_braces(body, found.end() - 1, "(", ")")
        fields = split_top_level(body[found.end(): close - 1])
        name = resolve(fields[0]) if fields else None
        if name is None:
            report.warn(f"{constructor}: group `{member}` has a non-literal name; skipped")
            continue
        groups[member] = name
        for implicit in group_classes.get(kind, []):
            controls.append(f"{name}/{implicit}")

    # `for (const char* b : {A_BUTTON, ...}) m_buttons->AddInput(..., b)` adds one
    # control per entry. Handle loops first and record their extent so the
    # direct-call pass below does not also see the loop variable.
    loops = re.compile(
        r"for\s*\(\s*(?:const\s+)?(?:auto|const\s+char\*)\s*&?\s*\w+\s*:\s*"
        r"(\{[^}]*\}|\w+)\s*\)\s*\{?\s*(m_\w+)->AddInput"
    )
    spans: list[tuple[int, int]] = []
    for found in loops.finditer(body):
        entries, member = found.group(1), found.group(2)
        spans.append((found.start(), body.find(";", found.end()) + 1))
        group = groups.get(member)
        if group is None:
            continue
        if entries.startswith("{"):
            names = [resolve(entry) for entry in split_top_level(entries[1:-1])]
        elif entries == "named_directions":
            names = list(directions)
        else:
            report.warn(f"{constructor}: loop over `{entries}` is not a literal list; skipped")
            continue
        for name in names:
            if name is None:
                report.warn(f"{constructor}: loop for `{group}` has an unresolved name")
                continue
            controls.append(f"{group}/{name}")

    for found in re.finditer(r"(m_\w+)->AddInput\s*\(", body):
        if any(start <= found.start() < end for start, end in spans):
            continue
        close = match_braces(body, found.end() - 1, "(", ")")
        group = groups.get(found.group(1))
        if group is None:
            continue
        for name in add_input_names(body[found.end(): close - 1], resolve):
            controls.append(f"{group}/{name}")

    settings: list[dict] = []
    for found in re.finditer(r"(m_\w+)->AddSetting\s*\(", body):
        group = groups.get(found.group(1))
        if group is None:
            continue
        close = match_braces(body, found.end() - 1, "(", ")")
        fields = split_top_level(body[found.end(): close - 1])
        # AddSetting(&value, {ini_name, ui_suffix, ui_description, ui_name}, default, min, max)
        if len(fields) < 3 or not fields[1].strip().startswith("{"):
            continue
        details = split_top_level(fields[1].strip()[1:-1])
        ini_name = resolve(details[0]) if details else None
        if ini_name is None:
            report.warn(f"{constructor}: setting in `{group}` has a non-literal name")
            continue
        ui_name = resolve(details[3]) if len(details) > 3 else None
        settings.append({
            "key": f"{group}/{ini_name}",
            "title": ui_name or ini_name,
            "default": fields[2].strip(),
            "range": [field.strip() for field in fields[3:5]],
        })

    unique = sorted(set(controls))
    if not unique:
        raise Bail(f"no controls parsed from {constructor}")
    return unique, settings


def build_setting_option(setting: dict, bool_style: tuple[str, str], scope: Scope,
                         report: Report) -> dict | None:
    """One `AddSetting` call as a layout option, typed from its default."""
    try:
        default = evaluate_expression(setting["default"], scope)
    except Unresolved as error:
        report.note(f"input: setting `{setting['key']}` has no constant default ({error})")
        return None
    option: dict = {
        "key": snake_case(setting["key"].split("/")[-1]),
        "title": setting["title"],
        "emit": {"key": setting["key"]},
    }
    if isinstance(default, bool):
        option["type"] = "bool"
        option["emit"]["bool_style"] = BOOL_STYLE
    elif isinstance(default, int):
        option["type"] = "int"
    elif isinstance(default, float):
        option["type"] = "float"
    else:
        report.note(f"input: setting `{setting['key']}` has an unsupported default type")
        return None
    option["default"] = default
    bounds = []
    for expression in setting["range"]:
        try:
            bounds.append(evaluate_expression(expression, scope))
        except Unresolved:
            bounds = []
            break
    if len(bounds) == 2 and option["type"] != "bool":
        option["range"] = {"min": bounds[0], "max": bounds[1]}
    del bool_style
    return option


def dolphin_ports(enums: dict[str, list[EnumDef]], report: Report):
    """Port declarations per machine and input group, and the keys they claim.

    Ports are grouped the way the machinespec groups them. A Wii has four Wii
    Remote ports *and* four GameCube ports, which are different families of
    device on different buses; the old flat numbering could only express that
    by interleaving them arbitrarily, so the four SI ports landed at indices
    4-7 with nothing saying why. Now the same four GameCube ports appear under
    `gamecube` on the Wii and under `controller` on the GameCube, which is
    where the machines themselves put them.

    A port's `emit` writes the emulator's own device-type keys, which are also
    `Config::Info` settings. The definition document must not offer them as
    options as well, so the claims come back with the ports.
    """
    si_pad = enum_value(enums, "SIDevices", "SIDEVICE_GC_CONTROLLER", report)
    si_none = enum_value(enums, "SIDevices", "SIDEVICE_NONE", report)
    wii_emulated = enum_value(enums, "WiimoteSource", "Emulated", report)
    wii_real = enum_value(enums, "WiimoteSource", "Real", report)
    wii_none = enum_value(enums, "WiimoteSource", "None", report)

    gc_ports = []
    for index in range(4):
        key = f"SIDevice{index}"
        gc_ports.append({
            "target": target_id("gcpad", f"GCPad{index + 1}"),
            "layouts": ["GAMECUBE_CONTROLLER"],
            "emit": {
                "unplugged": [{"target": "main.Core", "key": key, "value": si_none}],
                "GAMECUBE_CONTROLLER": [{"target": "main.Core", "key": key, "value": si_pad}],
            },
        })
    wii_ports = []
    for index in range(4):
        wii_ports.append({
            "target": target_id("wiipad", f"Wiimote{index + 1}"),
            "layouts": ["WII_REMOTE_CONTROLLER", "WII_REMOTE_NUNCHUCK_CONTROLLER"],
            # An `emit.passthrough` entry is what declares the port supports
            # passthrough; there is no separate field. The transport it needs
            # and the resources it claims are facts about the device and the
            # host, not about Dolphin's config file, so they are not ours.
            "emit": {
                "unplugged": [{"key": "Source", "value": wii_none}],
                "passthrough": [{"key": "Source", "value": wii_real}],
                "WII_REMOTE_CONTROLLER": [
                    {"key": "Source", "value": wii_emulated},
                    {"key": "Extension", "value": "None"},
                ],
                "WII_REMOTE_NUNCHUCK_CONTROLLER": [
                    {"key": "Source", "value": wii_emulated},
                    {"key": "Extension", "value": "Nunchuk"},
                ],
            },
        })

    claimed = {
        (entry.get("target", port["target"]), entry["key"])
        for port in gc_ports + wii_ports
        for entries in port["emit"].values()
        for entry in entries
    }
    platforms = {
        "NINTENDO_GCN": {"controller": gc_ports},
        "NINTENDO_WII": {"wii": wii_ports, "gamecube": gc_ports},
    }
    return platforms, claimed


def build_input(root: Path, spec_root: Path, systems: dict[str, str],
                enums: dict[str, list[EnumDef]], scope: Scope,
                bool_style: tuple[str, str], revision: str | None,
                report: Report) -> str:
    group_classes = parse_group_classes(root, report)
    core = root / "Source/Core/Core"
    gcpad_controls, gcpad_settings = parse_controller(
        root, core / "HW/GCPadEmu.h", core / "HW/GCPadEmu.cpp", "GCPad::GCPad",
        group_classes, report,
    )
    wiimote_controls, wiimote_settings = parse_controller(
        root, core / "HW/WiimoteEmu/WiimoteEmu.h", core / "HW/WiimoteEmu/WiimoteEmu.cpp",
        "Wiimote::Wiimote", group_classes, report,
    )
    # Cursor's relative-input setting belongs to the nested IR group rather
    # than Wiimote's constructor, so parse_controller cannot discover it.
    # Keep it in the generated layout options because onLaunch selects it
    # from the already-resolved controller mapping.
    wiimote_settings.append({
        "key": "IR/Relative Input",
        "title": "Relative Input",
        "default": "false",
        "range": [],
    })
    # Extension controls are written under `<attachment>/`, which Dolphin builds
    # by concatenation in Attachments::SaveConfig.
    nunchuk_controls, nunchuk_settings = parse_controller(
        root, core / "HW/WiimoteEmu/Extension/Nunchuk.h",
        core / "HW/WiimoteEmu/Extension/Nunchuk.cpp", "Nunchuk::Nunchuk",
        group_classes, report,
    )
    gcpad, wiimote = set(gcpad_controls), set(wiimote_controls)
    nunchuk = {f"Nunchuk/{control}" for control in nunchuk_controls}

    vocabulary = {
        "GAMECUBE_CONTROLLER": gcpad,
        "WII_REMOTE_CONTROLLER": wiimote,
        "WII_REMOTE_NUNCHUCK_CONTROLLER": wiimote | nunchuk,
    }
    layouts = {
        "GAMECUBE_CONTROLLER": (GAMECUBE_BINDINGS, gcpad_settings),
        "WII_REMOTE_CONTROLLER": (WII_REMOTE_BINDINGS, wiimote_settings),
        "WII_REMOTE_NUNCHUCK_CONTROLLER": (
            WII_NUNCHUK_BINDINGS,
            wiimote_settings + [{**setting, "key": f"Nunchuk/{setting['key']}"}
                                for setting in nunchuk_settings],
        ),
    }

    platforms, _ = dolphin_ports(enums, report)

    lines = [
        "# Generated by support/parse_config.py. Do not edit by hand.",
        f"# Dolphin revision: {revision or 'unknown'}",
        "",
    ]
    # One root per file, using the same ids the definition document derives from
    # Config::System, and the paths Dolphin's own loader chain declares.
    for system, sections in (("GCPad", [f"GCPad{n}" for n in range(1, 5)]),
                             ("WiiPad", [f"Wiimote{n}" for n in range(1, 5)])):
        if system not in systems:
            raise Bail(f"System::{system} has no INI path; cannot place input config")
        root_id = slugify(system)
        lines += [target_header(root_id),
                  f'output = {{ format = "ini", path = {toml_string(systems[system])} }}',
                  ""]
        lines += [target_header(root_id, section) for section in sections]
        lines.append("")

    lines += ['[input]', 'driver = "sdl3"', 'device = "SDL/{index}/{name}"', ""]
    lines += build_sdl_mapping(root, report) + [""]

    bound = 0
    exposed = 0
    for layout, (bindings, settings) in layouts.items():
        lines.append(f"[input.layouts.{toml_key(layout)}]")
        known = vocabulary[layout]
        device = consolespec.load_inputspec(spec_root, layout)
        for key, control in sorted(bindings.items()):
            if control not in known:
                report.error(f"input: Dolphin has no control `{control}` for `{key}` "
                             f"in {layout}")
                continue
            if not consolespec.check_binding(device, key, report, "input"):
                continue
            lines.append(f"{toml_key(key)} = {toml_string(control)}")
            bound += 1
        lines.append("")
        # The emulator's own per-device settings, expanded per port at lowering.
        for setting in settings:
            option = build_setting_option(setting, bool_style, scope, report)
            if option is None:
                continue
            lines.append(f"[[input.layouts.{toml_key(layout)}.options]]")
            for name in ("key", "title", "type"):
                lines.append(f"{name} = {toml_value(option[name])}")
            for name in ("range", "default"):
                if name in option:
                    lines.append(f"{name} = {toml_value(option[name])}")
            lines.append(f"emit = {toml_value(option['emit'])}")
            lines.append("")
            exposed += 1

    # Ports are per machine and per input group: a GameCube title has four
    # controller ports, a Wii title has four Wii Remote ports and the same four
    # GameCube ports on a separate bus.
    total = 0
    for platform, groups in platforms.items():
        machine = consolespec.load_machinespec(spec_root, platform)
        for group, ports in groups.items():
            offered = {layout for port in ports for layout in port["layouts"]}
            consolespec.check_group(machine, group, sorted(offered), len(ports),
                                    report, "input")
            for index, port in enumerate(ports):
                lines.append(
                    f"[platforms.{toml_key(platform)}.ports.{toml_key(group)}.{index}]")
                lines.append(f"target = {toml_string(port['target'])}")
                lines.append(f"layouts = {toml_value(port['layouts'])}")
                for choice, entries in port["emit"].items():
                    if choice in layouts:
                        entries = [*entries, {"key": "Device", "value": "{device}"}]
                    lines.append(f"emit.{toml_key(choice)} = {toml_value(entries)}")
                lines.append("")
                total += 1

    # Each layout now declares its bindings in full, so what it leaves unbound is
    # its own vocabulary minus its own table — not minus every layout's.
    unbound = {name: sorted(known - set(layouts[name][0].values()))
               for name, known in vocabulary.items()}
    report.note(f"input: {bound} bindings across {len(layouts)} layouts, "
                f"{exposed} exposed options, {total} ports across "
                f"{len(platforms)} machines")
    for layout, controls in unbound.items():
        report.note(f"input: {layout} leaves {len(controls)} Dolphin controls unbound")
    lines += ["# Dolphin controls with no consolespec element bound to them:"]
    for layout, controls in unbound.items():
        lines += [f"#   {layout}:"] + [f"#     {control}" for control in controls]
    return "\n".join(lines).rstrip() + "\n"


# --------------------------------------------------------------------------
# Entry point
# --------------------------------------------------------------------------


def dolphin_revision(root: Path) -> str | None:
    try:
        result = subprocess.run(
            ["git", "-C", str(root), "rev-parse", "--short", "HEAD"],
            capture_output=True, text=True, check=True, timeout=15,
        )
    except (OSError, subprocess.SubprocessError):
        return None
    return result.stdout.strip() or None



def read_text(path: Path) -> str:
    return path.read_text(encoding="utf-8", errors="replace")


def build_sdl_mapping(root: Path, report: Report) -> list[str]:
    """`[input.mapping]`: the SDL virtual gamepad, in Dolphin's own words.

    Dolphin names an SDL control by its semantic position, and signs a stick half
    by the direction it pushes toward — the same convention consolespec uses, so
    `analog.stick.left.y+` is `Left Y+` with no inversion. Trigger axes take the
    bare name; `Axis::GetName` returns no sign for them.
    """
    source = read_text(root / "Source/Core/InputCommon/ControllerInterface/SDL/SDLGamepad.h")
    buttons = sdlpad.named(source, "s_sdl_button_names")
    axes = sdlpad.named(source, "s_sdl_axis_names")
    entries = sdlpad.compose(buttons, axes, sign_first=False, raw_sdl=False)
    entries.extend(SDL_POINTER_AND_MOTION_MAPPING.items())
    missing = [key for key in sdlpad.BUTTONS.values()
               if key not in {entry for entry, _ in entries}]
    if missing:
        report.note(f"input: {'Dolphin'} names no SDL control for {', '.join(missing)}")
    report.note(f"input: {len(entries)} SDL controls paired by constant and sensor name")
    return sdlpad.emit_mapping((key, f"`{value}`") for key, value in entries)

def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--dolphin", required=True, type=Path,
                        help="path to a Dolphin source checkout")
    parser.add_argument("--out", type=Path, default=Path(__file__).parent.parent / "config",
                        help="directory to write the documents into")
    parser.add_argument("--consolespec", type=Path,
                        help="path to the consolespec directory "
                             "(default: found by walking up from this script)")
    parser.add_argument("--systems", default="Main,GFX",
                        help="comma-separated Config::System names, or `all`")
    parser.add_argument("--only", choices=["definition", "actions", "input"], action="append",
                        help="emit only the named document (repeatable)")
    parser.add_argument("--strict", action="store_true",
                        help="exit non-zero on warnings as well as errors "
                             "(notes are informational and never fail)")
    parser.add_argument("--dry-run", action="store_true", help="do not write files")
    arguments = parser.parse_args(argv)

    root = arguments.dolphin.expanduser().resolve()
    if not (root / "Source/Core").is_dir():
        print(f"error: {root} does not look like a Dolphin checkout", file=sys.stderr)
        return 2
    try:
        spec_root = (arguments.consolespec.expanduser().resolve()
                     if arguments.consolespec else consolespec.find_root())
    except consolespec.SpecError as error:
        print(f"error: {error}", file=sys.stderr)
        return 2

    report = Report()
    revision = dolphin_revision(root)
    wanted = arguments.only or ["definition", "actions", "input"]
    documents: dict[str, str] = {}

    try:
        # Both documents need these; parse them once.
        enums = parse_enums(root, report)
        systems = parse_config_systems(root, report)
        scope = enum_scope(enums, parse_constants(root, report))
        bool_style = parse_bool_style(root, report)
        _, claimed = dolphin_ports(enums, report)
        if "definition" in wanted:
            selected = (set(systems) if arguments.systems.strip().lower() == "all"
                        else {name.strip() for name in arguments.systems.split(",") if name.strip()})
            unknown = selected - set(systems)
            if unknown:
                report.warn(f"unknown config systems requested: {', '.join(sorted(unknown))}")
            owned = selected & set(INPUT_SYSTEMS)
            if owned:
                report.note(f"definition: {', '.join(sorted(owned))} left to input.toml, "
                            "which owns the controller files")
                selected -= owned
            infos = parse_infos(root, report)
            backends = parse_video_backends(root, report)
            titles = parse_system_titles(root, report)
            documents.update(build_definition(
                infos, systems, titles, enums, scope, backends, bool_style,
                claimed, selected, revision, report
            ))
        if "actions" in wanted:
            documents["actions.toml"] = build_actions(root, revision, report)
        if "input" in wanted:
            documents["input.toml"] = build_input(root, spec_root, systems, enums,
                                                  scope, bool_style, revision, report)
    except (Bail, consolespec.SpecError) as error:
        print(f"error: {error}", file=sys.stderr)
        report.emit()
        return 1

    if not arguments.dry_run:
        arguments.out.mkdir(parents=True, exist_ok=True)
        for name, text in documents.items():
            path = arguments.out / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(text, encoding="utf-8", newline="\n")

    report.emit()
    destination = "(dry run)" if arguments.dry_run else str(arguments.out)
    print(f"wrote {', '.join(sorted(documents))} to {destination}")
    print(f"{len(report.notes)} notes, {len(report.warnings)} warnings, "
          f"{len(report.errors)} errors")
    if report.errors:
        return 1
    if arguments.strict and report.warnings:
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
