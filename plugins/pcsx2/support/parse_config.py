#!/usr/bin/env python3
"""Generate the PCSX2 plugin's configuration documents from PCSX2's source.

PCSX2 is the least declarative of the three. Its pads are a table like
DuckStation's — each binding carrying the `GenericInputBinding` it corresponds
to — and its hotkey defaults are plain `SetStringValue` calls. Its *settings*,
though, are macro applications over struct members:

    SettingsWrapSection("EmuCore/GS");
    SettingsWrapBitBool(FrameLimitEnable);

which yields the section and the key (the stringified member name) but **not the
default**, which lives in whatever the struct's constructor happened to set.
So the definition document this produces has keys and no defaults, and says so.
See `--defaults` for supplying them from a generated PCSX2.ini.

Outputs, into --out (default ../config):

    pcsx2.toml     targets                        (Pcsx2Config.cpp)
    sections/*.toml one rooted document per section (Pcsx2Config.cpp)
    actions.toml   action map                     (SIO/Pad/Pad.cpp, Hotkeys.cpp)
    input.toml     ports + per-layout bindings    (SIO/Pad/Pad*.cpp)

Usage:

    python parse_config.py --pcsx2 E:/coding/pcsx2
"""

from __future__ import annotations

import argparse
import re
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "support"))
import consolespec
import sdlpad
import cppconfig
from cppconfig import *  # noqa: F403 — shared C++/TOML machinery

cppconfig.DEFINES = {"_WIN32": True, "WIN32": True}
cppconfig.UNDEFINES = {"__ANDROID__", "__APPLE__", "__linux__", "PCSX2_DEVBUILD"}
cppconfig.NOOP_MACROS = ("TRANSLATE_NOOP", "TRANSLATE_SV", "FSUI_NSTR")

SETTINGS_FILE = "inis/PCSX2.ini"
BOOL_STYLE = "lower"
FLOAT_STYLE = "{:.2}"
FLOAT_SETTINGS = {
    ("EmuCore/GS", "upscale_multiplier"),
    ("Framerate", "NominalScalar"),
    ("Framerate", "SlomoScalar"),
    ("Framerate", "TurboScalar"),
}
FILESYSTEM_SETTINGS = {
    ("DEV9/Hdd", "HddFile"),
    ("EmuCore", "BlockDumpSaveDirectory"),
    ("EmuCore/GS", "HWDumpDirectory"),
    ("EmuCore/GS", "OSDFontPath"),
    ("EmuCore/GS", "SWDumpDirectory"),
}

# PCSX2 shares stenzek's InputManager with DuckStation, so the same generic-pad
# vocabulary appears in its binding tables and the same element mapping serves.
BINDING_TO_GENERIC = {
    "button.a": "Cross",
    "button.b": "Circle",
    "button.x": "Square",
    "button.y": "Triangle",
    "button.select": "Select",
    "button.start": "Start",
    "button.guide": "System",
    "button.l": "L1",
    "button.r": "R1",
    "trigger.left": "L2",
    "trigger.right": "R2",
    "button.clickl": "L3",
    "button.clickr": "R3",
    "directional.n": "DPadUp",
    "directional.s": "DPadDown",
    "directional.w": "DPadLeft",
    "directional.e": "DPadRight",
    "analog.stick.left.x-": "LeftStickLeft",
    "analog.stick.left.x+": "LeftStickRight",
    "analog.stick.left.y-": "LeftStickDown",
    "analog.stick.left.y+": "LeftStickUp",
    "analog.stick.right.x-": "RightStickLeft",
    "analog.stick.right.x+": "RightStickRight",
    "analog.stick.right.y-": "RightStickDown",
    "analog.stick.right.y+": "RightStickUp",
}

# The machine these ports belong to and its machinespec input group. PCSX2 has
# eight internal pad slots, but Polymer exposes the four controller ports
# declared by the PlayStation 2 machine specification.
MACHINE = "SONY_PS2"
PORT_GROUP = "controller"
PORTS = 4

# PCSX2's standard pad type is `DualShock2`, which is the DualShock 2 and not
# the DualShock the PS2's group also accepts: the two are separate inputspec
# layouts because the DualShock 2's face buttons are pressure sensitive. Its
# other pad types (Guitar, Jogcon, Negcon, Popn) have no inputspec layout, so
# they are not offered.
LAYOUTS = {"DUALSHOCK_2_CONTROLLER": "DualShock2"}

ACTION_VOCABULARY = {
    "load_state": {"hotkey": "LoadStateFromSlot"},
    "save_state": {"hotkey": "SaveStateToSlot"},
    "screenshot": {"hotkey": "Screenshot"},
    "pause": {"hotkey": "TogglePause", "fallback": ("space", "Keyboard/Space")},
    "fast_forward": {"hotkey": "HoldTurbo", "fallback": ("tab", "Keyboard/Tab")},
    "toggle_fullscreen": {"hotkey": "ToggleFullscreen"},
    # F4-F6, F8, F9 are taken by PCSX2's own defaults; the collision check below
    # is what keeps this table honest when they move.
    "frame_advance": {"hotkey": "FrameAdvance", "fallback": ("f10", "Keyboard/F10")},
    "reset": {"hotkey": "ResetVM", "fallback": ("f11", "Keyboard/F11")},
    "exit": {"hotkey": "ShutdownVM", "fallback": ("f12", "Keyboard/F12")},
}

# `SettingsWrap*` macro → the value type it implies, where the macro says.
WRAP_TYPES = {
    "SettingsWrapBitBool": "bool",
    "SettingsWrapBitBoolEx": "bool",
    "SettingsWrapBitfield": "int",
    "SettingsWrapBitfieldEx": "int",
    "SettingsWrapIntEnumEx": "int",
    "SettingsWrapEntry": None,
    "SettingsWrapEntryEx": None,
    "SettingsWrapEnumEx": None,
    "SettingsWrapParsedEnum": None,
}
# Macros whose second argument is the ini key rather than the member name.
EXPLICIT_KEY = {
    "SettingsWrapEntryEx", "SettingsWrapBitfieldEx", "SettingsWrapBitBoolEx",
    "SettingsWrapEnumEx", "SettingsWrapIntEnumEx", "SettingsWrapParsedEnum",
}


def parse_settings(root: Path, report: Report) -> list[dict]:
    """(section, key, type) for every SettingsWrap* application, in file order."""
    text = load_source(root / "pcsx2/Pcsx2Config.cpp", report)
    settings: dict[tuple[str, str], dict] = {}
    section = None
    pattern = re.compile(r"\b(SettingsWrapSection|" + "|".join(WRAP_TYPES) + r")\s*\(")
    for found in pattern.finditer(text):
        macro = found.group(1)
        close = match_braces(text, found.end() - 1, "(", ")")
        arguments = split_top_level(text[found.end(): close - 1])
        if macro == "SettingsWrapSection":
            section = parse_string_literal(arguments[0]) if arguments else None
            continue
        if section is None or not arguments:
            continue
        if macro in EXPLICIT_KEY and len(arguments) > 1:
            key = parse_string_literal(arguments[1])
        else:
            # `SettingsWrapEntry(var)` stringifies the member expression itself,
            # so `EE.bios` becomes the key "EE.bios".
            key = arguments[0].strip()
            if not re.fullmatch(r"[A-Za-z_][\w.]*", key):
                key = None
        if not key:
            report.note(f"settings: unparsable key for {macro} in [{section}]")
            continue
        settings.setdefault((section, key),
                            {"section": section, "key": key, "type": WRAP_TYPES[macro]})
    if not settings:
        raise Bail("no SettingsWrap macros found; check --pcsx2")
    return list(settings.values())


def parse_defaults(path: Path, report: Report) -> dict[tuple[str, str], str]:
    """Defaults lifted from a PCSX2.ini the emulator generated for itself."""
    defaults: dict[tuple[str, str], str] = {}
    section = None
    for line in path.read_text(encoding="utf-8", errors="replace").splitlines():
        line = line.strip()
        if line.startswith("[") and line.endswith("]"):
            section = line[1:-1]
        elif section and "=" in line and not line.startswith(("#", ";")):
            key, value = line.split("=", 1)
            defaults[(section, key.strip())] = value.strip()
    report.note(f"settings: {len(defaults)} defaults read from {path.name}")
    return defaults


def parse_hotkeys(root: Path, report: Report) -> tuple[set[str], dict[str, str]]:
    """The hotkey vocabulary and the defaults PCSX2 ships for it."""
    # Entries come in two forms in the same list — `DEFINE_HOTKEY("X", …)` and a
    # bare `{"X", …}` — and GS keeps its own list. What every entry shares is a
    # name followed by its translated category, so key on that instead.
    entry = re.compile(r"\"([A-Za-z]\w*)\"\s*,\s*TRANSLATE_NOOP\s*\(\s*\"Hotkeys\"")
    names: set[str] = set()
    for path in sorted(root.glob("pcsx2/**/*.cpp")):
        try:
            text = path.read_text(encoding="utf-8", errors="replace")
        except OSError:
            continue
        if "HOTKEY" not in text:
            continue
        names.update(found.group(1) for found in entry.finditer(strip_comments(text)))
    if not names:
        raise Bail("no hotkey declarations found")

    defaults: dict[str, str] = {}
    text = load_source(root / "pcsx2/SIO/Pad/Pad.cpp", report)
    start = re.search(r"SetDefaultHotkeyConfig\s*\([^)]*\)\s*\{", text)
    if not start:
        raise Bail("Pad.cpp no longer defines SetDefaultHotkeyConfig")
    body = text[start.end(): match_braces(text, start.end() - 1) - 1]
    for call in re.finditer(r"SetStringValue\s*\(", body):
        close = match_braces(body, call.end() - 1, "(", ")")
        arguments = split_top_level(body[call.end(): close - 1])
        if len(arguments) != 3:
            continue
        if parse_string_literal(arguments[0]) != "Hotkeys":
            continue
        key = parse_string_literal(arguments[1])
        value = parse_string_literal(arguments[2])
        if key and value:
            defaults[key] = value
    report.note(f"actions: {len(names)} hotkeys declared, {len(defaults)} with defaults")
    return names, defaults


def parse_binding_tables(root: Path, report: Report) -> dict[str, dict[str, str]]:
    """Pad type name → {GenericInputBinding: ini binding name}."""
    variables: dict[str, dict[str, str]] = {}
    tables: dict[str, dict[str, str]] = {}
    for path in sorted((root / "pcsx2/SIO/Pad").glob("Pad*.cpp")):
        text = load_source(path, report)
        for found in re.finditer(r"InputBindingInfo\s+([\w:]+)\s*\[\]\s*=\s*\{", text):
            variable = found.group(1).split("::")[-1]
            body = text[found.end(): match_braces(text, found.end() - 1) - 1]
            entries: dict[str, str] = {}
            for entry in split_top_level(body):
                entry = entry.strip()
                if not entry.startswith("{"):
                    continue
                fields = split_top_level(entry[1: match_braces(entry, 0) - 1])
                if len(fields) < 2:
                    continue
                name = parse_string_literal(strip_noop_macros(fields[0]))
                generic = re.match(r"GenericInputBinding::(\w+)", fields[-1].strip())
                if name and generic:
                    entries.setdefault(generic.group(1), name)
            if entries:
                variables[f"{path.name}:{variable}"] = entries
        for found in re.finditer(r"ControllerInfo\s+[\w:]*ControllerInfo\s*=\s*\{", text):
            fields = split_top_level(
                text[found.end(): match_braces(text, found.end() - 1) - 1]
            )
            if len(fields) < 5:
                continue
            name = parse_string_literal(strip_noop_macros(fields[1]))
            key = f"{path.name}:{fields[4].strip().split('::')[-1]}"
            if name and key in variables:
                tables[name] = variables[key]
    if not tables:
        raise Bail("no pad binding tables parsed")
    report.note("input: binding tables for " + ", ".join(sorted(tables)))
    return tables


def build_definition(settings: list[dict], defaults: dict[tuple[str, str], str],
                     revision: str | None, report: Report) -> dict[str, str]:
    grouped: dict[str, list[dict]] = {}
    for setting in settings:
        grouped.setdefault(setting["section"], []).append(setting)

    main_lines = [
        "# Generated by support/parse_config.py. Do not edit by hand.",
        f"# PCSX2 revision: {revision or 'unknown'}",
        "#",
        "# PCSX2 declares settings as macro applications over struct members, so the",
        "# section and key are derivable but the default is not: it lives in the",
        "# struct's constructor. Options without a `default` below were not given one",
        "# by --defaults; supply a generated PCSX2.ini to fill them in.",
        "",
        "[meta]",
        'emulator = "pcsx2"',
        f'source_revision = "{revision or "unknown"}"',
        "",
        f"[bool_style.{BOOL_STYLE}]",
        'true = "true"',
        'false = "false"',
        "",
        target_header("ini"),
        f'output = {{ format = "ini", path = {toml_string(SETTINGS_FILE)} }}',
        "",
    ]
    main_lines += [target_header("ini", section) for section in sorted(grouped)]
    main_lines.append("")

    documents: dict[str, str] = {}
    emitted = typed = defaulted = 0
    for section in sorted(grouped):
        slug = slugify(section)
        filename = f"sections/{slug}.toml"
        if filename in documents:
            raise Bail(f"definition: duplicate section output name {filename!r}")
        lines = [
            "# Generated by support/parse_config.py. Do not edit by hand.",
            f"# PCSX2 revision: {revision or 'unknown'}",
            "",
            f"[sections.{toml_key(slug)}]",
            f"title = {toml_string(section)}",
            f"target = {toml_string(target_id('ini', section))}",
            "",
        ]
        used: set[str] = set()
        for setting in sorted(grouped[section], key=lambda s: s["key"].lower()):
            key = snake_case(setting["key"].replace(".", "_"))
            if key in used:
                continue
            used.add(key)
            raw = defaults.get((section, setting["key"]))
            kind = setting["type"]
            if kind is None:
                kind = infer_type(raw)
            else:
                typed += 1
            if (section, setting["key"]) in FILESYSTEM_SETTINGS:
                kind = "path"
            if (section, setting["key"]) in FLOAT_SETTINGS:
                kind = "float"
            emit = {"key": setting["key"]}
            if kind == "bool":
                emit["bool_style"] = BOOL_STYLE
            if kind == "float":
                emit["float_style"] = FLOAT_STYLE
            lines.append(f"[[sections.{toml_key(slug)}.options]]")
            lines.append(f"key = {toml_string(key)}")
            lines.append(f"title = {toml_string(humanize(setting['key'].replace('.', ' ')))}")
            lines.append(f"type = {toml_string(kind or 'string')}")
            value = coerce_default(raw, kind)
            if value is not None:
                lines.append(f"default = {toml_value(value)}")
                defaulted += 1
            lines.append(f"emit = {toml_value(emit)}")
            lines.append("")
            emitted += 1
        documents[filename] = "\n".join(lines).rstrip() + "\n"
    report.note(f"definition: {emitted} options across {len(grouped)} sections; "
                f"{typed} typed by their macro, {defaulted} with defaults")
    if defaulted < emitted:
        report.warn(f"definition: {emitted - defaulted} options have no default "
                    "(PCSX2 keeps them in struct constructors; pass --defaults)")
    documents["pcsx2.toml"] = "\n".join(main_lines).rstrip() + "\n"
    return documents


def infer_type(raw: str | None) -> str | None:
    if raw is None:
        return None
    if raw.lower() in ("true", "false"):
        return "bool"
    if re.fullmatch(r"-?\d+", raw):
        return "int"
    if re.fullmatch(r"-?\d*\.\d+", raw):
        return "float"
    return "string"


def coerce_default(raw: str | None, kind: str | None):
    if raw is None:
        return None
    try:
        if kind == "bool":
            return raw.lower() in ("true", "1")
        if kind == "int":
            return int(raw)
        if kind == "float":
            return float(raw)
    except ValueError:
        return None
    return raw


def canonical_chord(expression: str) -> str:
    """`Keyboard/Control & Keyboard/F8` → `control+f8`."""
    parts = []
    for token in expression.split("&"):
        token = token.strip()
        parts.append(token.split("/", 1)[-1].strip().lower() if "/" in token else token.lower())
    return "+".join(part for part in parts if part)


def build_actions(names: set[str], defaults: dict[str, str], revision: str | None,
                  report: Report) -> str:
    taken = {canonical_chord(value) for value in defaults.values()}
    lines = [
        "# Generated by support/parse_config.py. Do not edit by hand.",
        f"# PCSX2 revision: {revision or 'unknown'}",
        "",
        target_header("ini"),
        f'output = {{ format = "ini", path = {toml_string(SETTINGS_FILE)} }}',
        "",
        target_header("ini", "Hotkeys"),
        "",
        "[actions]",
        f'target = {toml_string(target_id("ini", "Hotkeys"))}',
        "",
        "[actions.map]",
    ]
    resolved = 0
    for action, spec in sorted(ACTION_VOCABULARY.items()):
        hotkey = spec["hotkey"]
        if hotkey not in names:
            report.error(f"action `{action}`: `{hotkey}` is not a PCSX2 hotkey")
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
                             "with a PCSX2 default")
                continue
            taken.add(chord)
            report.note(f"action `{action}`: `{hotkey}` is unbound upstream; using {expression}")
        else:
            chord = canonical_chord(expression)
        lines.append(f"{action} = "
                     f"{toml_value({'expression': expression, 'name': hotkey})}")
        resolved += 1
    report.note(f"actions: {resolved} of {len(ACTION_VOCABULARY)} vocabulary entries resolved, "
                f"{len(names) - resolved} PCSX2 hotkeys unused")
    return "\n".join(lines).rstrip() + "\n"


def build_input(tables: dict[str, dict[str, str]], root: Path, spec_root: Path,
                revision: str | None, report: Report) -> str:
    lines = [
        "# Generated by support/parse_config.py. Do not edit by hand.",
        f"# PCSX2 revision: {revision or 'unknown'}",
        "",
        target_header("ini"),
        f'output = {{ format = "ini", path = {toml_string(SETTINGS_FILE)} }}',
        "",
    ]
    # PCSX2 numbers its physical and multitap slots as one Pad sequence.
    lines += [target_header("ini", f"Pad{port}") for port in range(1, PORTS + 1)]
    lines += ["", "[input]", 'driver = "sdl3"', 'device = "SDL-{index}"', ""]
    lines += build_sdl_mapping(root, report) + [""]

    bound = 0
    for layout, controller in LAYOUTS.items():
        table = tables.get(controller)
        if table is None:
            report.error(f"input: no binding table for pad type `{controller}`")
            continue
        lines.append(f"[input.layouts.{toml_key(layout)}]")
        device = consolespec.load_inputspec(spec_root, layout)
        for key, generic in sorted(BINDING_TO_GENERIC.items()):
            binding = table.get(generic)
            if binding is None:
                continue
            # The generic vocabulary is wider than any one device; an element
            # this pad does not have is a gap between the two, not a typo here.
            if key not in device:
                report.note(f"input: {layout} has no `{key}`; PCSX2 binds "
                            f"it as `{binding}` on {controller}")
                continue
            lines.append(f"{toml_key(key)} = {toml_string(binding)}")
            bound += 1
        lines.append("")

    machine = consolespec.load_machinespec(spec_root, MACHINE)
    consolespec.check_group(machine, PORT_GROUP, list(LAYOUTS), PORTS, report,
                            "input")
    for port in range(1, PORTS + 1):
        lines.append(f"[platforms.{toml_key(MACHINE)}.ports."
                     f"{toml_key(PORT_GROUP)}.{port - 1}]")
        lines.append(f"target = {toml_string(target_id('ini', f'Pad{port}'))}")
        lines.append(f"layouts = {toml_value(list(LAYOUTS))}")
        lines.append('emit.unplugged = [{ key = "Type", value = "None" }]')
        for layout, controller in LAYOUTS.items():
            lines.append(f"emit.{toml_key(layout)} = "
                         f"{toml_value([{'key': 'Type', 'value': controller}])}")
        lines.append("")
    report.note(f"input: {bound} bindings across {len(LAYOUTS)} layouts, "
                f"{PORTS} {MACHINE}/{PORT_GROUP} pad slots")
    return "\n".join(lines).rstrip() + "\n"


def revision_of(root: Path) -> str | None:
    try:
        result = subprocess.run(["git", "-C", str(root), "rev-parse", "--short", "HEAD"],
                                capture_output=True, text=True, check=True, timeout=15)
    except (OSError, subprocess.SubprocessError):
        return None
    return result.stdout.strip() or None



def read_text(path: Path) -> str:
    return path.read_text(encoding="utf-8", errors="replace")


def build_sdl_mapping(root: Path, report: Report) -> list[str]:
    """`[input.mapping]`: the SDL virtual gamepad, in PCSX2's own words.

    PCSX2 keeps two vocabularies — one for the UI, which varies with the pad type
    SDL reports, and one for the settings file, which does not. Only the settings
    one may be written, so only that one is read here. Signs are a prefix on the
    value and follow raw SDL, so our `y+` (up) is PCSX2's `-LeftY`.
    """
    source = read_text(root / "pcsx2/Input/SDLInputSource.cpp")
    buttons = sdlpad.named(source, "s_sdl_button_setting_names")
    axes = sdlpad.named(source, "s_sdl_axis_setting_names")
    entries = sdlpad.compose(buttons, axes, sign_first=True, raw_sdl=True, trigger_sign="+")
    missing = [key for key in sdlpad.BUTTONS.values()
               if key not in {entry for entry, _ in entries}]
    if missing:
        report.note(f"input: {'Pcsx2'} names no SDL control for {', '.join(missing)}")
    report.note(f"input: {len(entries)} SDL controls paired by constant")
    return sdlpad.emit_mapping((key, f"{{device}}/{value}") for key, value in entries)

def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--pcsx2", required=True, type=Path)
    parser.add_argument("--defaults", type=Path,
                        help="a PCSX2.ini the emulator generated, for default values")
    parser.add_argument("--out", type=Path, default=Path(__file__).parent.parent / "config")
    parser.add_argument("--consolespec", type=Path)
    parser.add_argument("--strict", action="store_true")
    parser.add_argument("--dry-run", action="store_true")
    arguments = parser.parse_args(argv)

    root = arguments.pcsx2.expanduser().resolve()
    if not (root / "pcsx2").is_dir():
        print(f"error: {root} does not look like a PCSX2 checkout", file=sys.stderr)
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
        defaults: dict[tuple[str, str], str] = {}
        if arguments.defaults:
            if not arguments.defaults.is_file():
                raise Bail(f"--defaults file not found: {arguments.defaults}")
            defaults = parse_defaults(arguments.defaults, report)
        settings = parse_settings(root, report)
        names, hotkey_defaults = parse_hotkeys(root, report)
        tables = parse_binding_tables(root, report)
        documents = {
            "actions.toml": build_actions(names, hotkey_defaults, revision, report),
            "input.toml": build_input(tables, root, spec_root, revision, report),
        }
        documents.update(build_definition(settings, defaults, revision, report))
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
    print(f"wrote {', '.join(sorted(documents))} to "
          f"{'(dry run)' if arguments.dry_run else arguments.out}")
    print(f"{len(report.notes)} notes, {len(report.warnings)} warnings, "
          f"{len(report.errors)} errors")
    if report.errors:
        return 1
    return 1 if arguments.strict and report.warnings else 0


if __name__ == "__main__":
    sys.exit(main())
