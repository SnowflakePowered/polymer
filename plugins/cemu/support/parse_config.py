#!/usr/bin/env python3
"""Generate the Cemu plugin's configuration documents from Cemu's source.

Cemu is the odd one of the four. Its settings live in an XML document, not an
INI, so the definition document's targets nest to arbitrary depth. Its hotkeys
are stored as *two space-separated integers in one element* — a wxWidgets
keycode and a controller button index — which is a fifth binding encoding beyond
the four the design's survey lists. And its controller bindings are keyed by
number rather than name: `<mapping>7</mapping>` is a `ButtonId` ordinal, so the
binding table is an integer mapping on both sides. Each emulated controller
class declares its *own* `ButtonId` enum with its own ordering, so there is one
table per layout and the enums are parsed from one header each.

Two structs share `settings.xml`. `CemuConfig::Save` writes the `content` node
and *returns* it, and `XMLConfig::Save` hands that return value to every child
config, so `wxCemuConfig::Save` appends its own settings — and the hotkeys —
under `content` as well. Both are parsed, and the wx half is rooted there.

Outputs, into --out (default ../config):

    cemu.toml      targets + section imports      (config/CemuConfig.{h,cpp},
                                                   gui/wxgui/wxCemuConfig.{h,cpp})
    sections/*.toml one rooted document per section
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
import consolespec
import sdlpad
import cppconfig
from cppconfig import *  # noqa: F403 — shared C++/TOML machinery

cppconfig.DEFINES = {"_WIN32": True, "WIN32": True}
cppconfig.UNDEFINES = {
    "__linux__", "__APPLE__", "CEMU_DEBUG_ASSERT", "ANDROID", "__ANDROID__",
}

SETTINGS_FILE = "settings.xml"
BOOL_STYLE = "lower"

# Member types that hold a collection rather than a setting. Cemu writes these
# as repeated `Entry` elements, which an option cannot describe, so they are
# skipped at the declaration rather than left to fail later as an unresolved
# member.
CONTAINER_TYPES = {
    "std::vector", "std::map", "std::unordered_map", "std::set",
    "std::unordered_set", "std::array", "std::mutex", "std::shared_mutex",
    "std::atomic", "std::function", "std::unique_ptr", "std::shared_ptr",
}

# Types Cemu writes one element per field of. Walking the config struct finds
# `Vector2i window_position{-1, -1}` but not what a `Vector2i` holds, because
# it is a template in util/math/vector2.h rather than a member struct, so the
# fields and their order are named here. Order matters: the brace initializer
# is positional.
AGGREGATES = {"Vector2i": {"x": "int", "y": "int"}}

# Nodes the definition document does not describe. The hotkey elements are
# `sHotkeyCfg` — a keycode and a pad button packed into one element — which is
# not a scalar an option can carry; actions.toml owns them instead.
EXCLUDED_PATHS = {("content", "Hotkeys")}

# The machine these ports belong to, and how its machinespec groups them. A Wii
# U has one GamePad port and eight controller ports; Cemu has eight profile
# slots total, of which at most two can hold a GamePad
# (`InputManager::kMaxVPADControllers`) and at most seven anything else
# (`kMaxWPADControllers`). Intersecting the two gives one GamePad port and seven
# controller ports, which is the partition below — profile 0 is the GamePad, 1
# through 7 the rest. Cemu itself assigns channels by scanning the slots in
# order, so a vacant slot 0 does not shift the others.
MACHINE = "NINTENDO_WIIU"
GAMEPAD_PORTS = 1
CONTROLLER_PORTS = 7

# consolespec element → the `ButtonId` enumerator Cemu's profile stores as a
# number. Cemu's own names are the vocabulary; the parser turns them into the
# integers the file actually holds, so a reordered enum changes the emitted
# numbers. Every class declares its own enum: `kButtonId_Home` is 27 on a
# GamePad, 11 on a Pro Controller and 17 on a Wiimote, so the tables cannot be
# shared even where the element names coincide.
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

# ProController::ButtonId. Mostly the same element names as the GamePad but
# different ordinals: the Pro enum puts Home before the D-pad where the GamePad
# puts it last. The stick clicks are absent because the inputspec does not
# declare them (see UNDECLARED).
PRO_BINDINGS = {element: enumerator for element, enumerator in VPAD_BINDINGS.items()
                if element not in ("ButtonClickL", "ButtonClickR")}

# WiimoteController::ButtonId, which shares almost nothing with the other two.
# A Wii Remote has no second stick, no shoulder buttons and no stick clicks; its
# numbered buttons are the ones printed "1" and "2", which consolespec spells as
# the numbered elements 0 and 1.
WIIMOTE_BINDINGS = {
    "ButtonA": "kButtonId_A",
    "ButtonB": "kButtonId_B",
    "Button0": "kButtonId_1",
    "Button1": "kButtonId_2",
    "ButtonStart": "kButtonId_Plus",
    "ButtonSelect": "kButtonId_Minus",
    "ButtonGuide": "kButtonId_Home",
    "DirectionalN": "kButtonId_Up",
    "DirectionalS": "kButtonId_Down",
    "DirectionalW": "kButtonId_Left",
    "DirectionalE": "kButtonId_Right",
}

# Cemu models the extension as part of the Wiimote's own enum rather than a
# separate device, but consolespec models a remote with a nunchuk as its own
# layout, and a layout declares its bindings in full. What a device calls a
# control can change when it gains another — attaching a nunchuk gives the Wii
# Remote a second accelerometer, which forces the first to say which side it is
# on — so no table may be inherited into a layout it was not written for.
WII_NUNCHUK_BINDINGS = {
    **WIIMOTE_BINDINGS,
    "ButtonZ": "kButtonId_Nunchuck_Z",
    "ButtonC": "kButtonId_Nunchuck_C",
    "AxisLeftAnalogPositiveY": "kButtonId_Nunchuck_Up",
    "AxisLeftAnalogNegativeY": "kButtonId_Nunchuck_Down",
    "AxisLeftAnalogNegativeX": "kButtonId_Nunchuck_Left",
    "AxisLeftAnalogPositiveX": "kButtonId_Nunchuck_Right",
}

# `WPADDeviceType`, which `WiimoteController::save` writes into `<device_type>`
# alongside `<type>`. One Cemu type string covers both remote layouts; this is
# what distinguishes them.
WPAD_CORE = "0"        # kWAPDevCore — bare remote
WPAD_FREESTYLE = "1"   # kWAPDevFreestyle — remote with nunchuk

# Elements Cemu binds that the inputspec does not declare, so there is nothing to
# bind them to. Reported on every run; move an entry into the table above once
# the inputspec gains the button. The Wii U Pro Controller's sticks click on real
# hardware and Cemu maps them, but WIIU_PRO_CONTROLLER declares no `clickl` /
# `clickr` button, unlike WIIU_DRC_CONTROLLER.
UNDECLARED = {
    ("WIIU_PRO_CONTROLLER", "ButtonClickL"): "kButtonId_StickL",
    ("WIIU_PRO_CONTROLLER", "ButtonClickR"): "kButtonId_StickR",
}

# inputspec layout → the Cemu emulated controller that realizes it: the header
# its `ButtonId` enum lives in, the string the profile writes into
# `<emulated_controller><type>`, the binding table, and any further keys the
# type alone does not pin down.
LAYOUTS = {
    "WIIU_DRC_CONTROLLER": {
        "header": "VPADController.h",
        "type": "Wii U GamePad",
        "bindings": VPAD_BINDINGS,
        "emit": {},
    },
    "WIIU_PRO_CONTROLLER": {
        "header": "ProController.h",
        "type": "Wii U Pro Controller",
        "bindings": PRO_BINDINGS,
        "emit": {},
    },
    "WII_REMOTE_CONTROLLER": {
        "header": "WiimoteController.h",
        "type": "Wiimote",
        "bindings": WIIMOTE_BINDINGS,
        "emit": {"device_type": WPAD_CORE},
    },
    "WII_REMOTE_NUNCHUCK_CONTROLLER": {
        "header": "WiimoteController.h",
        "type": "Wiimote",
        "bindings": WII_NUNCHUK_BINDINGS,
        "emit": {"device_type": WPAD_FREESTYLE},
    },
}

# Which layouts each port group offers. Cemu's Wii U Classic Controller has no
# inputspec layout, so it is not offered.
GROUP_LAYOUTS = {
    "gamepad": ["WIIU_DRC_CONTROLLER"],
    "controller": ["WIIU_PRO_CONTROLLER", "WII_REMOTE_CONTROLLER",
                   "WII_REMOTE_NUNCHUCK_CONTROLLER"],
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


def collect_enumerators(text: str, scope: Scope, namespace: str | None) -> None:
    """Define every enumerator in `text`, bare and qualified.

    Defaults are written both ways — `kStereo` unqualified, but
    `ScreenPosition::kDisabled` and `DefaultColumnSize::name` through their
    scope — and `Scope` resolves a qualified name by any suffix of it, so
    defining the bare name alongside the enum tag and the enclosing namespace
    covers every spelling that appears.
    """
    pattern = re.compile(r"\benum\s+(?:class\s+|struct\s+)?(\w+)?\s*(?::\s*[\w:\s]+?\s*)?\{")
    for found in pattern.finditer(text):
        tag = found.group(1)
        try:
            end = match_braces(text, found.end() - 1)
        except Bail:
            continue
        following = 0
        for entry in split_top_level(text[found.end(): end - 1]):
            member = re.match(r"^(\w+)\s*(?:=\s*(.+))?$", entry.strip(), re.S)
            if not member:
                continue
            if member.group(2):
                try:
                    following = int(evaluate_expression(member.group(2), scope))
                except (Unresolved, TypeError, ValueError):
                    continue
            for qualifier in (None, tag, namespace):
                scope.define(f"{qualifier}::{member.group(1)}" if qualifier
                             else member.group(1), following)
            following += 1


def scope_of(text: str) -> Scope:
    """The named constants a header declares: enumerators and `constexpr`s."""
    scope = Scope()
    for found in re.finditer(r"\bnamespace\s+(\w+)\s*\{", text):
        try:
            end = match_braces(text, found.end() - 1)
        except Bail:
            continue
        collect_enumerators(text[found.end(): end - 1], scope, found.group(1))
    collect_enumerators(text, scope, None)
    return resolve_constants(collect_constants(text), scope)


def strip_casts(expression: str) -> str:
    """Peel the casts and wrappers Cemu writes around a value or a member.

    `static_cast<sint32>(x)`, `(sint64)x` and `boost::nowide::narrow(x).c_str()`
    all name one thing; none of them survive being handed to the expression
    evaluator or matched as a member path.
    """
    text = expression.strip()
    for _ in range(8):
        before = text
        text = re.sub(r"^\(\s*[\w:]+\s*\)\s*", "", text).strip()
        wrapper = re.match(r"^(?:static_cast\s*<[^>]*>|boost::nowide::narrow|"
                           r"std::move|std::forward\s*<[^>]*>)\s*\(", text)
        if wrapper:
            try:
                end = match_braces(text, wrapper.end() - 1, "(", ")")
            except Bail:
                return text
            text = (text[wrapper.end(): end - 1] + text[end:]).strip()
        if text.startswith("("):
            try:
                end = match_braces(text, 0, "(", ")")
            except Bail:
                return text
            if end == len(text):
                text = text[1:-1].strip()
        text = re.sub(r"\.(?:GetValue|c_str)\(\)$", "", text).strip()
        if text == before:
            break
    return text


def split_members(body: str) -> list[str]:
    """One entry per declaration in a struct body.

    A declaration ends at a `;` outside brackets, but a member function's body
    ends at `}`, so a run whose top-level `{` followed a `)` is a definition and
    is discarded when that brace closes. Without that, a function body would be
    glued onto whatever declaration came after it.
    """
    declarations: list[str] = []
    current: list[str] = []
    depth, function = 0, False
    i = 0
    while i < len(body):
        character = body[i]
        if character in "\"'":
            end = skip_literal(body, i)
            current.append(body[i:end])
            i = end
            continue
        current.append(character)
        if character in "([{":
            if depth == 0 and character == "{":
                function = bool(re.search(r"\)\s*(?:const\s*)?$",
                                          "".join(current[:-1])))
            depth += 1
        elif character in ")]}":
            depth -= 1
            if depth == 0 and character == "}" and function:
                current, function = [], False
        elif character == ";" and depth == 0:
            declarations.append("".join(current[:-1]))
            current = []
        i += 1
    return [entry.strip() for entry in declarations if entry.strip()]


DECLARATION = re.compile(
    r"^((?:(?:const|constexpr|static|inline|mutable|unsigned|signed)\s+)*"
    r"[\w:]+(?:\s*<.*>)?(?:\s*[*&])?)\s+(.+)$", re.S)


def parse_members(body: str, scope: Scope, report: Report, origin: str,
                  prefix: str = "") -> dict[str, dict]:
    """Every scalar member a struct body declares, keyed by dotted path.

    Cemu does not store settings uniformly: some are `ConfigValue<T>`, some are
    plain typed members, and some live in an anonymous struct that names the XML
    node they land under (`overlay.text_scale` -> `Graphic/Overlay/TextScale`).
    All three have to resolve, because the writer refers to them by whichever
    form the member happens to take.
    """
    # Access specifiers and the `#define`/`#undef` pairs Cemu wraps around a
    # platform-dependent default both sit between declarations without a `;` of
    # their own, so they would otherwise be read as part of the next one.
    body = re.sub(r"\b(?:public|private|protected)\s*:", "", body)
    body = re.sub(r"^[ \t]*#[ \t]*\w+[^\n]*$", "", body, flags=re.M)
    values: dict[str, dict] = {}
    for declaration in split_members(body):
        nested = re.match(r"^(?:struct|union)\b\s*(\w+)?\s*(?=\{)", declaration)
        if nested:
            try:
                end = match_braces(declaration, nested.end())
            except Bail:
                continue
            name = re.match(r"^\s*(\w+)", declaration[end:])
            if not name:
                continue  # a tag-only declaration: a type, not a member
            values.update(parse_members(declaration[nested.end() + 1: end - 1],
                                        scope, report, origin,
                                        f"{prefix}{name.group(1)}."))
            continue

        config = re.match(r"^ConfigValue(Bounds)?\s*(?=<)", declaration)
        if config:
            try:
                type_end = match_braces(declaration, config.end(), "<", ">")
            except Bail:
                continue
            ctype = declaration[config.end() + 1: type_end - 1].strip()
            rest = declaration[type_end:].strip()
            name = re.match(r"^(\w+)\s*(?=\{|$)", rest)
            if not name:
                continue
            arguments: list[str] = []
            if rest[name.end():].startswith("{"):
                try:
                    end = match_braces(rest, name.end())
                except Bail:
                    continue
                arguments = split_top_level(rest[name.end() + 1: end - 1])
            # `ConfigValueBounds` takes (min, init, max); everything else, and
            # its enum-iterator overload, takes the initial value alone. An
            # absent or empty initializer is value-initialization, which is a
            # real default and not a missing one.
            if config.group(1) and len(arguments) == 3:
                expression = arguments[1]
            elif len(arguments) == 1:
                expression = arguments[0]
            elif arguments:
                report.warn(f"{origin}: `{prefix}{name.group(1)}` has "
                            f"{len(arguments)} initializers; default not read")
                expression = None
            else:
                expression = None
            values[f"{prefix}{name.group(1)}"] = {
                "ctype": ctype,
                "default": (evaluate_default(expression, scope) if expression
                            else value_initialized(ctype)),
            }
            continue

        parsed = DECLARATION.match(declaration)
        if not parsed:
            continue
        qualifiers, declarators = parsed.group(1).strip(), parsed.group(2)
        if re.match(r"^(?:static|constexpr)\b", qualifiers):
            continue
        ctype = re.sub(r"^(?:(?:const|inline|mutable)\s+)*", "", qualifiers).strip()
        if ctype.split("<")[0].strip() in CONTAINER_TYPES or ctype.endswith(("*", "&")):
            continue
        for declarator in split_top_level(declarators):
            member = re.match(r"^(\w+)\s*(?:=\s*(.+)|\{(.*)\})?\s*$",
                              declarator.strip(), re.S)
            if not member:
                continue  # a function declaration, or a declarator we cannot read
            name, expression = member.group(1), member.group(2)
            fields = AGGREGATES.get(ctype.split("::")[-1])
            if fields:
                # `Vector2i pad_size{-1, -1}` — one element per field, in order.
                initializers = split_top_level(member.group(3) or "")
                for index, (field, field_type) in enumerate(fields.items()):
                    values[f"{prefix}{name}.{field}"] = {
                        "ctype": field_type,
                        "default": (evaluate_default(initializers[index], scope)
                                    if index < len(initializers)
                                    else value_initialized(field_type)),
                    }
                continue
            if expression is None and member.group(3):
                expression = member.group(3)
            values[f"{prefix}{name}"] = {
                "ctype": ctype,
                "default": (evaluate_default(expression, scope) if expression
                            else value_initialized(ctype)),
            }
    return values


def evaluate_default(expression: str, scope: Scope) -> object:
    """A declared initializer as a value, or None when it is not a constant."""
    text = strip_casts(expression)
    # `L"default"` and friends: the encoding prefix is not part of the literal.
    text = re.sub(r'(?<![\w])(?:L|u8|u|U)(?=")', "", text)
    if not text:
        return None
    try:
        value = evaluate_expression(text, scope)
    except (Unresolved, Bail):
        return None
    return value if isinstance(value, (bool, int, float, str)) else None


def value_initialized(ctype: str) -> object:
    """What `TType()` gives a member Cemu declares without an initializer.

    `ConfigValueAtomic()` and `ConfigValueNoneAtomic()` both value-initialize,
    so `ConfigValue<bool> pad_open;` really does default to false rather than to
    nothing, and the document should say so.
    """
    return {"bool": False, "int": 0, "float": 0.0, "string": ""}[cemu_type(ctype)]


def parse_config_values(root: Path, report: Report) -> dict[str, dict]:
    """Members of both structs that share settings.xml, keyed by dotted path.

    The two never collide: `CemuConfig`'s members are only ever read by
    `CemuConfig::Save` and `wxCemuConfig`'s only by its own, so a single table
    keyed by member path serves both writers.
    """
    values: dict[str, dict] = {}
    for source, struct in (("src/config/CemuConfig.h", "CemuConfig"),
                           ("src/gui/wxgui/wxCemuConfig.h", "wxCemuConfig")):
        text = load_source(root / source, report)
        found = re.search(rf"\bstruct\s+{struct}\s*\{{", text)
        if not found:
            raise Bail(f"{source} no longer declares `struct {struct}`")
        body = text[found.end(): match_braces(text, found.end() - 1) - 1]
        parsed = parse_members(body, scope_of(text), report, Path(source).name)
        if not parsed:
            raise Bail(f"{source}: `struct {struct}` parsed without members")
        values.update(parsed)
    return values


def parse_save(text: str, qualified: str, seed: tuple[str, ...],
               report: Report) -> list[dict]:
    """The XML path and member behind every `config.set("name", member)` call.

    Cemu writes its document with a parser handle per element, so the enclosing
    path is tracked through the `auto x = y.set("Node")` assignments rather than
    being visible at the call site. `seed` is where the method's own parser
    argument already sits: `CemuConfig::Save` receives the document root and
    opens `content` itself, while `wxCemuConfig::Save` is handed the `content`
    node that the former returned.
    """
    start = re.search(
        rf"\b{re.escape(qualified)}\s*\(\s*XMLConfigParser\s*&\s*(\w+)\s*\)\s*\{{",
        text)
    if not start:
        raise Bail(f"{qualified} is no longer defined as expected")
    body = text[start.end(): match_braces(text, start.end() - 1) - 1]

    handles: dict[str, tuple[str, ...]] = {start.group(1): seed}
    entries: list[dict] = []
    attributes = 0
    for found in re.finditer(
        r"(?:auto\s+(\w+)\s*=\s*)?(\w+)\s*\.set(_attribute)?\s*(?:<[^>]*>)?\s*\(", body
    ):
        target, handle, attribute = found.group(1), found.group(2), found.group(3)
        close = match_braces(body, found.end() - 1, "(", ")")
        arguments = split_top_level(body[found.end(): close - 1])
        name = parse_string_literal(arguments[0]) if arguments else None
        if name is None:
            continue
        parent = handles.get(handle, ())
        if attribute:
            # An XML attribute, not an element. Targets address elements, so
            # these cannot be expressed; count them so they do not read as an
            # oversight in the emitted document.
            attributes += 1
            continue
        if target:
            handles[target] = parent + (name,)
            continue
        if len(arguments) < 2:
            continue
        member = strip_casts(arguments[1])
        if not re.fullmatch(r"[\w.]+", member):
            continue
        entries.append({"path": parent, "name": name, "member": member})
    if not entries:
        raise Bail(f"{qualified} yielded no settings")
    if attributes:
        report.note(f"definition: {attributes} settings under {qualified} are XML "
                    "attributes rather than elements and cannot be targeted")
    return entries


def parse_xml_layout(root: Path, report: Report) -> list[dict]:
    """Every setting both writers put into settings.xml, in document order."""
    return (parse_save(load_source(root / "src/config/CemuConfig.cpp", report),
                       "CemuConfig::Save", (), report)
            + parse_save(load_source(root / "src/gui/wxgui/wxCemuConfig.cpp", report),
                         "wxCemuConfig::Save", ("content",), report))


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


def parse_button_ids(root: Path, header: str, report: Report) -> dict[str, int]:
    """One class's `ButtonId`, whose ordinals are what a profile stores."""
    text = load_source(root / "src/input/emulated" / header, report)
    found = re.search(r"\benum\s+ButtonId\s*\{", text)
    if not found:
        raise Bail(f"{header} no longer declares `enum ButtonId`")
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
        raise Bail(f"{header}: `enum ButtonId` parsed without kButtonId_A")
    return ids


def build_definition(values: dict[str, dict], entries: list[dict],
                     revision: str | None, report: Report) -> dict[str, str]:
    grouped: dict[tuple[str, ...], list[dict]] = {}
    unresolved: list[str] = []
    for entry in entries:
        if entry["path"] in EXCLUDED_PATHS:
            continue
        declared = values.get(entry["member"])
        if declared is None:
            unresolved.append(f"{'/'.join(entry['path'] + (entry['name'],))} "
                              f"({entry['member']})")
            continue
        grouped.setdefault(entry["path"], []).append({**entry, **declared})
    for entry in unresolved:
        # Every one of these is a repeated container element whose member is a
        # loop variable, so there is nothing to declare. Listed rather than
        # counted, because a *new* one would mean a setting was dropped.
        report.note(f"definition: no declaration for {entry}")

    main_lines = [
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
            if header not in main_lines:
                main_lines.append(header)
    main_lines.append("")

    documents: dict[str, str] = {}
    emitted = 0
    for path in sorted(grouped):
        slug = slugify("-".join(path) or "general")
        filename = f"sections/{slug}.toml"
        if filename in documents:
            raise Bail(f"definition: section path {' / '.join(path)!r} produces "
                       f"duplicate output name {filename!r}")
        main_lines += [f"[sections.{toml_key(slug)}]",
                       f"import = {toml_string(filename)}", ""]
        section_lines = [
            "# Generated by support/parse_config.py. Do not edit by hand.",
            f"# Cemu revision: {revision or 'unknown'}",
            "",
            f"[sections.{toml_key(slug)}]",
            f"title = {toml_string(' / '.join(path) or 'General')}",
            f"target = {toml_string(target_id('settings', *path))}",
            "",
        ]
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
            section_lines.append(f"[[sections.{toml_key(slug)}.options]]")
            section_lines.append(f"key = {toml_string(key)}")
            section_lines.append(f"title = {toml_string(humanize(entry['name']))}")
            section_lines.append(f"type = {toml_string(kind)}")
            if isinstance(entry["default"], (bool, int, float, str)):
                section_lines.append(f"default = {toml_value(entry['default'])}")
            section_lines.append(f"emit = {toml_value(emit)}")
            section_lines.append("")
            emitted += 1
        documents[filename] = "\n".join(section_lines).rstrip() + "\n"
    report.note(f"definition: {emitted} options across {len(grouped)} sections")
    documents["cemu.toml"] = "\n".join(main_lines).rstrip() + "\n"
    return documents


def cemu_type(ctype: str) -> str:
    simple = ctype.split("::")[-1].strip()
    if simple == "bool":
        return "bool"
    if simple in ("std::string", "string", "std::wstring", "wstring", "fs::path"):
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
        "#",
        "# The hotkeys are written by `wxCemuConfig::Save`, which runs as a child of",
        "# `CemuConfig::Save` and is handed the `content` node the latter returns, so",
        "# they sit under `content` and not at the document root.",
        "",
        target_header("settings"),
        'format = "xml"',
        f"path = {toml_string(SETTINGS_FILE)}",
        "",
        target_header("settings", "content"),
        target_header("settings", "content", "Hotkeys"),
        "",
        "[actions]",
        f'target = {toml_string(target_id("settings", "content", "Hotkeys"))}',
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


def build_input(ids: dict[str, dict[str, int]], root: Path, spec_root: Path,
                revision: str | None, report: Report) -> str:
    slots = GAMEPAD_PORTS + CONTROLLER_PORTS
    lines = [
        "# Generated by support/parse_config.py. Do not edit by hand.",
        f"# Cemu revision: {revision or 'unknown'}",
        "#",
        "# Cemu keeps one profile file per slot, and its binding keys are ButtonId",
        "# ordinals rather than names. Each emulated controller class declares its",
        "# own ButtonId enum, so the same element is a different number per layout.",
        "",
    ]
    for port in range(slots):
        lines += [target_header(f"profile-{port}"),
                  'format = "xml"',
                  f'path = {toml_string(f"controllerProfiles/controller{port}.xml")}',
                  "",
                  target_header(f"profile-{port}", "emulated_controller"),
                  target_header(f"profile-{port}", "emulated_controller", "controller"),
                  target_header(f"profile-{port}", "emulated_controller", "controller",
                                "mappings"),
                  ""]

    lines += ["[input]", 'driver = "sdl3"', 'device = "SDL/{index}"', ""]
    lines += build_sdl_mapping(root, report) + [""]

    bound = 0
    for layout, definition in LAYOUTS.items():
        lines.append(f"[input.layouts.{toml_key(layout)}]")
        device = consolespec.load_inputspec(spec_root, layout)
        enumerators = ids[definition["header"]]
        for element, enumerator in sorted(definition["bindings"].items()):
            ordinal = enumerators.get(enumerator)
            if ordinal is None:
                report.error(f"input: `{enumerator}` is not in "
                             f"{definition['header']}'s ButtonId")
                continue
            if not consolespec.check_binding(device, element, report, "input"):
                continue
            entry = device.elements[element]
            lines.append(f"{toml_key(entry.path)} = {toml_string(str(ordinal))}")
            bound += 1
        lines.append("")

    # Ports are grouped as the machinespec groups them: the single GamePad port,
    # then the controller ports. Slot 0 holds the GamePad.
    machine = consolespec.load_machinespec(spec_root, MACHINE)
    slot = 0
    for group, count in (("gamepad", GAMEPAD_PORTS),
                         ("controller", CONTROLLER_PORTS)):
        offered = GROUP_LAYOUTS[group]
        consolespec.check_group(machine, group, offered, count, report, "input")
        for index in range(count):
            profile = f"profile-{slot}"
            lines.append(f"[platforms.{toml_key(MACHINE)}.ports."
                         f"{toml_key(group)}.{index}]")
            lines.append("target = " + toml_string(target_id(
                profile, "emulated_controller", "controller", "mappings")))
            lines.append(f"layouts = {toml_value(offered)}")
            for layout in offered:
                definition = LAYOUTS[layout]
                entries = [{"target": target_id(profile, "emulated_controller"),
                            "key": "type", "value": definition["type"]}]
                entries += [{"target": target_id(profile, "emulated_controller"),
                             "key": key, "value": value}
                            for key, value in definition["emit"].items()]
                lines.append(f"emit.{toml_key(layout)} = {toml_value(entries)}")
            lines.append("")
            slot += 1

    report.note(f"input: {bound} bindings across {len(LAYOUTS)} layouts, "
                f"{GAMEPAD_PORTS} {MACHINE}/gamepad and {CONTROLLER_PORTS} "
                f"{MACHINE}/controller ports over {slots} profile slots")
    report.note("input: bindings are ButtonId ordinals; a reordered enum upstream "
                "silently changes every emitted number, so regenerate on version bumps")
    report.note("input: Cemu's Wii U Classic Controller has no inputspec layout "
                "and is not offered")
    for (layout, element), enumerator in sorted(UNDECLARED.items()):
        report.warn(f"input: {layout} declares no `{element}`, so Cemu's "
                    f"`{enumerator}` is left unbound")
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
    """`[input.mapping]`: the SDL virtual gamepad, in Cemu's own words.

    Cemu is the case that makes this a table rather than a rule: it names
    nothing. A profile stores an integer into `ControllerBase`'s own index space,
    and `SDLController::raw_state` writes SDL button *i* into slot *i*, so for
    buttons the value is SDL's own numbering. Axes are not in that run — they sit
    in a separate region of the enum, one constant per signed half — so both are
    resolved from `Controller.h` rather than assumed.
    """
    source = read_text(root / "src/input/api/Controller.h")
    order = enum_order(source, "Buttons2")
    # `axis` is the left stick, `rotation` the right, `trigger` the triggers;
    # P and N are SDL's own signs, not screen directions.
    axis_constant = {
        "SDL_GAMEPAD_AXIS_LEFTX": "kAxisX",
        "SDL_GAMEPAD_AXIS_LEFTY": "kAxisY",
        "SDL_GAMEPAD_AXIS_RIGHTX": "kRotationX",
        "SDL_GAMEPAD_AXIS_RIGHTY": "kRotationY",
        "SDL_GAMEPAD_AXIS_LEFT_TRIGGER": "kTriggerX",
        "SDL_GAMEPAD_AXIS_RIGHT_TRIGGER": "kTriggerY",
    }
    absent = [name for name in ("kButton0", *axis_constant.values())
              if f"{name}P" not in order and name not in order]
    if absent:
        report.error(f"input: Cemu's Buttons2 enum has no {', '.join(absent)}")
        return []

    entries = []
    for constant, key in sdlpad.BUTTONS.items():
        # SDL's value is the slot, and `kButton0` is where that run starts.
        entries.append((key, str(order["kButton0"] + sdl_button_value(constant))))
    for constant, key, sdl_positive in sdlpad.STICKS:
        name = axis_constant[constant] + ("P" if sdl_positive else "N")
        entries.append((key, str(order[name])))
    for constant, key in sdlpad.TRIGGERS:
        entries.append((key, str(order[axis_constant[constant] + "P"])))
    report.note(f"input: {len(entries)} SDL controls resolved from Controller.h")
    return sdlpad.emit_mapping(entries)


def sdl_button_value(constant: str) -> int:
    """`SDL_GAMEPAD_BUTTON_*`'s own value, which Cemu stores verbatim."""
    return list(sdlpad.BUTTONS).index(constant)



def enum_order(source: str, name: str) -> dict[str, int]:
    """`{enumerator: value}` for a plain C++ enum with no explicit values."""
    start = source.index(f"enum {name}")
    body = source[source.index("{", start) + 1:source.index("};", start)]
    order, value = {}, 0
    for line in body.splitlines():
        line = line.split("//")[0].strip().rstrip(",").strip()
        if not line:
            continue
        if "=" in line:
            # An alias like `kButtonNoneAxisMAX = kButtonRight` re-uses a value.
            alias, target = (part.strip() for part in line.split("=", 1))
            order[alias] = order.get(target, value)
            continue
        order[line] = value
        value += 1
    return order

def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--cemu", required=True, type=Path)
    parser.add_argument("--out", type=Path, default=Path(__file__).parent.parent / "config")
    parser.add_argument("--consolespec", type=Path)
    parser.add_argument("--strict", action="store_true")
    parser.add_argument("--dry-run", action="store_true")
    arguments = parser.parse_args(argv)

    root = arguments.cemu.expanduser().resolve()
    if not (root / "src/config").is_dir():
        print(f"error: {root} does not look like a Cemu checkout", file=sys.stderr)
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
        values = parse_config_values(root, report)
        entries = parse_xml_layout(root, report)
        hotkeys = parse_hotkeys(root, report)
        ids = {header: parse_button_ids(root, header, report)
               for header in {entry["header"] for entry in LAYOUTS.values()}}
        documents = {
            "actions.toml": build_actions(hotkeys, revision, report),
            "input.toml": build_input(ids, root, spec_root, revision, report),
        }
        documents.update(build_definition(values, entries, revision, report))
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
