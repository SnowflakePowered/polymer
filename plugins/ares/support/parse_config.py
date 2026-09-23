#!/usr/bin/env python3
"""Generate the Ares plugin's configuration documents from Ares's source.

Ares declares setting storage and defaults in ``desktop-ui/settings/settings.hpp``
and maps those members to BML paths with the ``bind`` calls in ``settings.cpp``.
This generator joins those two declarations. Dynamic input, hotkey, firmware,
recent-file, and per-emulator entries are deliberately left to their dedicated
configuration documents rather than guessed from the loops in ``process``.

Outputs, into --out (default ../config):

    ares.toml       the settings.bml target tree
    sections/*.toml one option document per top-level BML section
    input.toml      SDL virtual-pad bindings and consolespec ports

Usage:

    python parse_config.py --ares E:/coding/ares
"""

from __future__ import annotations

import argparse
import re
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "support"))
import consolespec  # noqa: E402
from cppconfig import (  # noqa: E402
    Bail,
    Report,
    humanize,
    match_braces,
    parse_string_literal,
    slugify,
    snake_case,
    strip_comments,
    target_header,
    target_id,
    toml_key,
    toml_string,
    toml_value,
)

SETTINGS_FILE = "settings.bml"
BOOL_STYLE = "lower"

CPP_TYPES = {
    "bool": "bool",
    "u8": "int",
    "u32": "int",
    "f64": "float",
    "string": "string",
}

BIND_TYPES = {
    "boolean": "bool",
    "natural": "int",
    "real": "float",
    "string": "string",
}

# Set at load time by the selected video backend, rather than by the member
# initializer. Omitting a default preserves that distinction in the definition.
DYNAMIC_DEFAULTS = {"video.driver"}

# Machines represented by Ares that also have a consolespec id. Ares has a few
# additional regional/computer variants; those share these machines' virtual
# pad configuration and do not need separate plugin platform declarations.
PLATFORMS = (
    "ATARI_2600", "BANDAI_WS", "BANDAI_WSC", "COLECO_CV", "NEC_SGFX",
    "NEC_TG16", "NEC_TGCD", "NINTENDO_FDS", "NINTENDO_GB", "NINTENDO_GBA",
    "NINTENDO_GBC", "NINTENDO_N64", "NINTENDO_N64DD", "NINTENDO_NES",
    "NINTENDO_SNES", "SEGA_32X", "SEGA_32X_CD", "SEGA_CD", "SEGA_GEN",
    "SEGA_GG", "SEGA_SAT", "SEGA_SG1000", "SEGA_SMS", "SNK_NG", "SNK_NGP",
    "SNK_NGPC", "SONY_PSX",
)

# consolespec binding key → the BML key Ares's VirtualPad constructor serializes.
# Layouts use the intersection with this map; unusual direct-mapped devices
# (mice, keypads, light guns) are not falsely presented as virtual gamepads.
BINDING_TO_VIRTUAL = {
    "button.a": "A..South",
    "button.b": "B..East",
    "button.x": "X..West",
    "button.y": "Y..North",
    "button.l": "L-Bumper",
    "button.r": "R-Bumper",
    "button.select": "Select",
    "button.start": "Start",
    "button.clickl": "L-Stick..Click",
    "button.clickr": "R-Stick..Click",
    "directional.n": "Pad.Up",
    "directional.s": "Pad.Down",
    "directional.w": "Pad.Left",
    "directional.e": "Pad.Right",
    "directional.left.n": "Pad.Up",
    "directional.left.s": "Pad.Down",
    "directional.left.w": "Pad.Left",
    "directional.left.e": "Pad.Right",
    "analog.stick.left.x-": "L-Left",
    "analog.stick.left.x+": "L-Right",
    "analog.stick.left.y-": "L-Down",
    "analog.stick.left.y+": "L-Up",
    "analog.stick.right.x-": "R-Left",
    "analog.stick.right.x+": "R-Right",
    "analog.stick.right.y-": "R-Down",
    "analog.stick.right.y+": "R-Up",
    "analog.rotary.x-": "L-Left",
    "analog.rotary.x+": "L-Right",
    "trigger.left": "L-Trigger",
    "trigger.right": "R-Trigger",
}

# Ares's SDL joystick backend stores identifier/group/input[/qualifier]. The
# identifier is supplied by `device`; these values describe SDL's standardized
# virtual gamepad groups (Axis=0, Button=3).
SDL_MAPPING = (
    ("button.a", "3/0"), ("button.b", "3/1"),
    ("button.x", "3/2"), ("button.y", "3/3"),
    ("button.select", "3/4"), ("button.guide", "3/5"),
    ("button.start", "3/6"), ("button.clickl", "3/7"),
    ("button.clickr", "3/8"), ("button.l", "3/9"),
    ("button.r", "3/10"), ("directional.n", "3/11"),
    ("directional.s", "3/12"), ("directional.w", "3/13"),
    ("directional.e", "3/14"), ("button.0", "3/15"),
    ("button.1", "3/16"), ("button.2", "3/17"),
    ("button.3", "3/18"), ("button.4", "3/19"),
    ("button.5", "3/20"),
    ("analog.stick.left.x+", "0/0/Hi"),
    ("analog.stick.left.x-", "0/0/Lo"),
    ("analog.stick.left.y+", "0/1/Lo"),
    ("analog.stick.left.y-", "0/1/Hi"),
    ("analog.stick.right.x+", "0/2/Hi"),
    ("analog.stick.right.x-", "0/2/Lo"),
    ("analog.stick.right.y+", "0/3/Lo"),
    ("analog.stick.right.y-", "0/3/Hi"),
    ("trigger.left", "0/4/Hi"), ("trigger.right", "0/5/Hi"),
)


def revision_of(root: Path) -> str | None:
    try:
        return subprocess.run(
            ["git", "-C", str(root), "rev-parse", "HEAD"],
            check=True,
            capture_output=True,
            text=True,
        ).stdout.strip() or None
    except (OSError, subprocess.CalledProcessError):
        return None


def read_text(path: Path) -> str:
    try:
        return path.read_text(encoding="utf-8")
    except UnicodeDecodeError:
        return path.read_text(encoding="utf-8-sig")


def scalar_default(ctype: str, expression: str | None):
    if expression is None or expression.strip() == "{}":
        return {"bool": False, "u8": 0, "u32": 0, "f64": 0.0, "string": ""}[ctype]
    expression = expression.strip()
    if ctype == "bool" and expression in ("true", "false"):
        return expression == "true"
    if ctype == "string":
        value = parse_string_literal(expression)
        if value is not None:
            return value
    if ctype in ("u8", "u32") and re.fullmatch(r"\d+[uUlL]*", expression):
        return int(re.match(r"\d+", expression).group())
    if ctype == "f64" and re.fullmatch(r"(?:\d+(?:\.\d*)?|\.\d+)(?:[eE][+-]?\d+)?", expression):
        return float(expression)
    raise Bail(f"cannot evaluate {ctype} default `{expression}`")


def parse_members(header: str, report: Report) -> dict[str, dict]:
    """Return fully-qualified Settings member names with type and default."""
    text = strip_comments(header)
    start = re.search(r"\bstruct\s+Settings\s*:\s*Markup::Node\s*\{", text)
    if not start:
        raise Bail("settings.hpp no longer declares Settings : Markup::Node")
    close = match_braces(text, start.end() - 1)
    root = text[start.end():close - 1]
    members: dict[str, dict] = {}

    def walk(body: str, prefix: str = "") -> None:
        position = 0
        while position < len(body):
            position += len(body[position:]) - len(body[position:].lstrip())
            nested = re.match(r"struct\s+\w+\s*\{", body[position:])
            if nested:
                brace = position + nested.end() - 1
                end = match_braces(body, brace)
                trailer = re.match(r"\s*(\w+)\s*;", body[end:])
                if not trailer:
                    raise Bail("anonymous Settings child struct has no member name")
                name = trailer.group(1)
                walk(body[brace + 1:end - 1], f"{prefix}{name}.")
                position = end + trailer.end()
                continue

            semicolon = body.find(";", position)
            if semicolon < 0:
                break
            declaration = body[position:semicolon].strip()
            position = semicolon + 1
            field = re.fullmatch(
                r"(bool|u8|u32|f64|string)\s+(\w+)(?:\s*\[\s*\d+\s*\])?"
                r"(?:\s*=\s*(.+))?",
                declaration,
                re.DOTALL,
            )
            if not field:
                continue
            ctype, name, expression = field.groups()
            member = f"{prefix}{name}"
            members[member] = {
                "ctype": ctype,
                "type": CPP_TYPES[ctype],
                "default": scalar_default(ctype, expression),
            }

    walk(root)
    if not members:
        raise Bail("no scalar Settings members found")
    report.note(f"members: {len(members)} scalar declarations")
    return members


def parse_bindings(source: str, members: dict[str, dict], report: Report) -> list[dict]:
    """Parse fixed-path bind(type, "BML/Path", member) calls."""
    text = strip_comments(source)
    pattern = re.compile(
        r"\bbind\s*\(\s*(string|boolean|natural|real)\s*,\s*"
        r'"((?:\\.|[^"\\])*)"\s*,\s*([A-Za-z_]\w*(?:\.[A-Za-z_]\w*)+)\s*\)'
    )
    settings: list[dict] = []
    seen: set[str] = set()
    for found in pattern.finditer(text):
        bind_type, encoded_path, member = found.groups()
        path = parse_string_literal(f'"{encoded_path}"')
        if path is None:
            raise Bail(f"cannot decode bind path `{encoded_path}`")
        if path in seen:
            report.warn(f"binding: duplicate BML path {path}")
            continue
        seen.add(path)
        declaration = members.get(member)
        if declaration is None:
            report.error(f"binding: `{member}` has no scalar declaration")
            continue
        kind = BIND_TYPES[bind_type]
        if kind != declaration["type"]:
            report.error(
                f"binding: `{path}` is {bind_type}, but `{member}` is {declaration['ctype']}"
            )
            continue
        components = path.split("/")
        if len(components) < 2 or any(not component for component in components):
            report.error(f"binding: invalid BML path `{path}`")
            continue
        settings.append({
            "section": components[0],
            "path": components,
            "member": member,
            "type": kind,
            "default": None if member in DYNAMIC_DEFAULTS else declaration["default"],
        })
    if not settings:
        raise Bail("no fixed-path bind calls found in settings.cpp")
    report.note(f"bindings: {len(settings)} fixed settings")
    return settings


def build_definition(settings: list[dict], revision: str | None, report: Report) -> dict[str, str]:
    grouped: dict[str, list[dict]] = {}
    target_paths: set[tuple[str, ...]] = set()
    for setting in settings:
        grouped.setdefault(setting["section"], []).append(setting)
        components = setting["path"]
        for depth in range(1, len(components)):
            target_paths.add(tuple(components[:depth]))

    revision = revision or "unknown"
    main_lines = [
        "# Generated by support/parse_config.py. Do not edit by hand.",
        f"# Ares revision: {revision}",
        "",
        "[meta]",
        'emulator = "ares"',
        f"source_revision = {toml_string(revision)}",
        "",
        f"[bool_style.{BOOL_STYLE}]",
        'true = "true"',
        'false = "false"',
        "",
        target_header("settings"),
        f'output = {{ format = "bml", path = {toml_string(SETTINGS_FILE)} }}',
        "",
    ]
    for path in sorted(target_paths):
        main_lines.append(target_header("settings", *path))
    main_lines.append("")

    documents: dict[str, str] = {}
    emitted = 0
    for section, entries in sorted(grouped.items()):
        slug = slugify(section)
        lines = [
            "# Generated by support/parse_config.py. Do not edit by hand.",
            f"# Ares revision: {revision}",
            "",
            f"[sections.{toml_key(slug)}]",
            f"title = {toml_string({'Nintendo64': 'Nintendo 64'}.get(section, humanize(section)))}",
            f"target = {toml_string(target_id('settings', section))}",
            "",
        ]
        used: set[str] = set()
        for setting in sorted(entries, key=lambda item: [part.lower() for part in item["path"]]):
            relative = setting["path"][1:]
            key = snake_case("_".join(relative))
            if key in used:
                raise Bail(f"definition: duplicate option key `{slug}.{key}`")
            used.add(key)
            emit = {"key": relative[-1]}
            if len(relative) > 1:
                emit["target"] = target_id("settings", *setting["path"][:-1])
            if setting["type"] == "bool":
                emit["bool_style"] = BOOL_STYLE
            option_type = "path" if section == "Paths" else setting["type"]
            lines.extend([
                f"[[sections.{toml_key(slug)}.options]]",
                f"key = {toml_string(key)}",
                f"title = {toml_string(humanize(relative[-1]))}",
                f"type = {toml_string(option_type)}",
            ])
            if setting["default"] is not None:
                lines.append(f"default = {toml_value(setting['default'])}")
            lines.extend([f"emit = {toml_value(emit)}", ""])
            emitted += 1
        documents[f"sections/{slug}.toml"] = "\n".join(lines).rstrip() + "\n"

    documents["ares.toml"] = "\n".join(main_lines).rstrip() + "\n"
    report.note(f"definition: {emitted} options across {len(grouped)} sections")
    return documents


def build_input(spec_root: Path, revision: str | None, report: Report) -> str:
    revision = revision or "unknown"
    machines = {machine: consolespec.load_machinespec(spec_root, machine)
                for machine in PLATFORMS}
    layout_names = sorted({layout for machine in machines.values()
                           for group in machine.groups.values() for layout in group.inputs})

    layouts: dict[str, list[tuple[str, str]]] = {}
    for name in layout_names:
        spec = consolespec.load_inputspec(spec_root, name)
        entries = []
        for key, binding in BINDING_TO_VIRTUAL.items():
            if key in spec:
                entries.append((key, binding))
        if entries:
            layouts[name] = sorted(entries)
        else:
            report.note(f"input: {name} has no VirtualPad-compatible controls")

    lines = [
        "# Generated by support/parse_config.py. Do not edit by hand.",
        f"# Ares revision: {revision}",
        "",
        target_header("settings"),
        f'output = {{ format = "bml", path = {toml_string(SETTINGS_FILE)} }}',
        "",
    ]
    for index in range(5):
        lines.append(target_header("settings", f"VirtualPad{index + 1}"))
    lines.extend([
        "",
        "[input]",
        'driver = "sdl3"',
        # Ares uses SDL's GUID plus the duplicate ordinal as its stable device
        # identifier (see ruby/input/joypad/sdl.cpp).
        'device = "{guid}/{index}"',
        "",
        "[input.mapping]",
    ])
    width = max(len(key) for key, _ in SDL_MAPPING) + 2
    for key, value in SDL_MAPPING:
        lines.append(f"{toml_string(key)}{' ' * (width - len(key) - 2)} = "
                     f"{toml_string('{device}/' + value)}")

    for name, entries in layouts.items():
        lines.extend(["", f"[input.layouts.{toml_key(name)}]"])
        for key, value in entries:
            lines.append(f"{toml_key(key)} = {toml_string(value)}")

    ports = 0
    for machine_name, machine in machines.items():
        for group_name, group in machine.groups.items():
            offered = [layout for layout in group.inputs if layout in layouts]
            if not offered or group.ports is None:
                continue
            # Ares exposes five shared VirtualPads. No supported machine needs
            # more, but cap malformed/upstream expansions defensively.
            for index in range(min(group.ports, 5)):
                lines.extend([
                    "",
                    f"[platforms.{toml_key(machine_name)}.ports.{toml_key(group_name)}.{index}]",
                    f"target = {toml_string(target_id('settings', f'VirtualPad{index + 1}'))}",
                    f"layouts = {toml_value(offered)}",
                    "emit = {}",
                ])
                ports += 1
    report.note(f"input: {len(layouts)} layouts and {ports} ports")
    return "\n".join(lines).rstrip() + "\n"


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--ares", required=True, type=Path)
    parser.add_argument("--out", type=Path, default=Path(__file__).parent.parent / "config")
    parser.add_argument("--consolespec", type=Path)
    parser.add_argument("--revision", help="Source revision when using a release source archive")
    parser.add_argument("--strict", action="store_true")
    parser.add_argument("--dry-run", action="store_true")
    arguments = parser.parse_args(argv)

    root = arguments.ares.expanduser().resolve()
    settings_root = root / "desktop-ui/settings"
    if not settings_root.is_dir():
        print(f"error: {root} does not look like an Ares checkout", file=sys.stderr)
        return 2

    report = Report()
    try:
        spec_root = (arguments.consolespec.expanduser().resolve()
                     if arguments.consolespec else consolespec.find_root())
        members = parse_members(read_text(settings_root / "settings.hpp"), report)
        settings = parse_bindings(read_text(settings_root / "settings.cpp"), members, report)
        revision = arguments.revision or revision_of(root)
        documents = build_definition(settings, revision, report)
        documents["input.toml"] = build_input(spec_root, revision, report)
    except (Bail, OSError, consolespec.SpecError) as error:
        print(f"error: {error}", file=sys.stderr)
        report.emit()
        return 1

    if not arguments.dry_run:
        arguments.out.mkdir(parents=True, exist_ok=True)
        for name, contents in documents.items():
            path = arguments.out / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(contents, encoding="utf-8", newline="\n")
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
