"""The SDL virtual gamepad, as our controls are named on it.

The authority is `crates/input/snowglass-input/src/sdl.rs`; this is the same
pairing. It is a pairing and not an ordering on purpose: which of our controls is
SDL's south is the one choice that is ours, and SDL's numbering is a consequence
of it. Emulator sources annotate every entry of their own name tables with the
SDL constant, so a generator joins on the control and never counts — which is
what lets it survive SDL appending to the enum, as it has with `MISC2`..`MISC6`.
"""

from __future__ import annotations

import re

# Our name for each control of the SDL gamepad.
BUTTONS = {
    "SDL_GAMEPAD_BUTTON_SOUTH": "button.a",
    "SDL_GAMEPAD_BUTTON_EAST": "button.b",
    "SDL_GAMEPAD_BUTTON_WEST": "button.x",
    "SDL_GAMEPAD_BUTTON_NORTH": "button.y",
    "SDL_GAMEPAD_BUTTON_BACK": "button.select",
    "SDL_GAMEPAD_BUTTON_GUIDE": "button.guide",
    "SDL_GAMEPAD_BUTTON_START": "button.start",
    "SDL_GAMEPAD_BUTTON_LEFT_STICK": "button.clickl",
    "SDL_GAMEPAD_BUTTON_RIGHT_STICK": "button.clickr",
    "SDL_GAMEPAD_BUTTON_LEFT_SHOULDER": "button.l",
    "SDL_GAMEPAD_BUTTON_RIGHT_SHOULDER": "button.r",
    "SDL_GAMEPAD_BUTTON_DPAD_UP": "directional.n",
    "SDL_GAMEPAD_BUTTON_DPAD_DOWN": "directional.s",
    "SDL_GAMEPAD_BUTTON_DPAD_LEFT": "directional.w",
    "SDL_GAMEPAD_BUTTON_DPAD_RIGHT": "directional.e",
    "SDL_GAMEPAD_BUTTON_MISC1": "button.0",
    "SDL_GAMEPAD_BUTTON_RIGHT_PADDLE1": "button.1",
    "SDL_GAMEPAD_BUTTON_LEFT_PADDLE1": "button.2",
    "SDL_GAMEPAD_BUTTON_RIGHT_PADDLE2": "button.3",
    "SDL_GAMEPAD_BUTTON_LEFT_PADDLE2": "button.4",
    "SDL_GAMEPAD_BUTTON_TOUCHPAD": "button.5",
}

# Our name for each signed half of each `SDL_GAMEPAD_AXIS_*`, as
# (constant, our key, is the half SDL-positive).
#
# The third field is where a convention difference lives. Our vocabulary names a
# half for the screen direction it pushes toward, so `y+` is *up* — which is
# SDL's *negative* Y. An emulator that signs in raw SDL terms (PCSX2,
# DuckStation) therefore writes `-LeftY` for our `y+`, and one that signs by
# direction (Dolphin) writes `Left Y+`. Neither is wrong; this is where they meet.
STICKS = [
    ("SDL_GAMEPAD_AXIS_LEFTX", "analog.stick.left.x+", True),
    ("SDL_GAMEPAD_AXIS_LEFTX", "analog.stick.left.x-", False),
    ("SDL_GAMEPAD_AXIS_LEFTY", "analog.stick.left.y+", False),
    ("SDL_GAMEPAD_AXIS_LEFTY", "analog.stick.left.y-", True),
    ("SDL_GAMEPAD_AXIS_RIGHTX", "analog.stick.right.x+", True),
    ("SDL_GAMEPAD_AXIS_RIGHTX", "analog.stick.right.x-", False),
    ("SDL_GAMEPAD_AXIS_RIGHTY", "analog.stick.right.y+", False),
    ("SDL_GAMEPAD_AXIS_RIGHTY", "analog.stick.right.y-", True),
]

# A trigger is one unsigned range, so it has no signed halves.
TRIGGERS = [
    ("SDL_GAMEPAD_AXIS_LEFT_TRIGGER", "trigger.left"),
    ("SDL_GAMEPAD_AXIS_RIGHT_TRIGGER", "trigger.right"),
]


def named(source: str, array: str, constants: str = r"SDL_GAMEPAD_(?:BUTTON|AXIS)_\w+") -> dict[str, str]:
    """`{SDL constant: the emulator's name}` for one of its own tables.

    Every emulator annotates each entry with the constant it is, which is what
    makes this a join rather than a count. An entry without one is skipped: it is
    a control SDL added after that emulator last looked.
    """
    start = source.index(array)
    start = source.index("{", start)
    depth = 0
    for index in range(start, len(source)):
        if source[index] == "{":
            depth += 1
        elif source[index] == "}":
            depth -= 1
            if depth == 0:
                end = index
                break
    names = {}
    for line in source[start : end + 1].splitlines():
        constant = re.search(rf"//\s*({constants})", line)
        if not constant:
            continue
        # `TRANSLATE_NOOP("SDLInputSource", "A")` carries the context first.
        value = re.search(r'TRANSLATE_NOOP\(\s*"[^"]*"\s*,\s*"([^"]*)"', line)
        if not value:
            value = re.search(r'"((?:[^"\\]|\\.)*)"', line)
        if value:
            names[constant.group(1)] = value.group(1)
    return names


def emit_mapping(entries: list[tuple[str, str]]) -> list[str]:
    """`[input.mapping]` lines, aligned."""
    lines = ["[input.mapping]"]
    width = max(len(key) for key, _ in entries) + 2
    for key, value in entries:
        lines.append(f'"{key}"{" " * (width - len(key) - 2)} = "{value}"')
    return lines


def compose(buttons: dict[str, str], axes: dict[str, str],
            sign_first: bool, raw_sdl: bool, trigger_sign: str = "") -> list[tuple[str, str]]:
    """Pair our controls with an emulator's names for them.

    `sign_first` puts the sign before the name (PCSX2's `+LeftX`) rather than
    after (Dolphin's `Left X+`). `raw_sdl` says which convention the sign
    follows — see `STICKS`.
    """
    entries = [(key, buttons[constant])
               for constant, key in BUTTONS.items() if constant in buttons]
    for constant, key, sdl_positive in STICKS:
        if constant not in axes:
            continue
        mark = "+" if (sdl_positive if raw_sdl else key.endswith("+")) else "-"
        name = axes[constant]
        entries.append((key, f"{mark}{name}" if sign_first else f"{name}{mark}"))
    for constant, key in TRIGGERS:
        if constant in axes:
            entries.append((key, f"{trigger_sign}{axes[constant]}"))
    return entries
