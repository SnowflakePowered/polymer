"""Generate the standard gamepad projection from consolespec.

Run from the repository root after changing an inputspec:

    python plugins/support/gen_projection.py

The output is committed and reviewed: this proposes an allocation, it does not
own one. Two bugs — the Wii Remote + Nunchuk's colliding accelerometers and the
3DS depth slider claiming a whole stick — were caught by review, not by the
rule, so a human reads the diff.


The allocation rule follows the conventional gamepad layout: real hardware fills
*unused standard slots* before it reaches for misc1 or a paddle. A Wii Remote's
1 and 2 become X and Y because a Wii Remote has no X/Y face buttons; Saturn's C
and Z become shoulders because a Saturn pad's shoulder slots are free. Only when
every standard slot is taken does anything land on a spare.

Both sides are binding keys: a device control on the left, the standard pad
control that carries it on the right. A control the pad has under the same name
keeps it, so only the differences are written.
"""
import tomllib
from pathlib import Path

import consolespec

ROOT = consolespec.find_root() / "definitions" / "inputspec"

# The pad's own buttons. An element with one of these names claims it.
PAD_BUTTONS = {"a", "b", "x", "y", "select", "guide", "start",
               "clickl", "clickr", "l", "r"}
# Free standard slots in the order hardware fills them, then the true spares:
# misc1, the four paddles, and the touchpad.
FILL = ["button.x", "button.y", "button.l", "button.r",
        "trigger.left", "trigger.right",
        "button.0", "button.1", "button.2", "button.3", "button.4"]
TOUCHPAD = "button.5"
# A second directional becomes the right stick's digital half-axes, which is
# what Nintendo's own NSO N64 controller reports for the C-buttons.
STICK2 = {"n": "analog.stick.right.y-", "s": "analog.stick.right.y+",
          "w": "analog.stick.right.x-", "e": "analog.stick.right.x+"}
# A rotary dial or a slider is one degree of freedom, so it takes one axis, not
# a whole stick. The 3DS has both Circle Pads occupied, so its depth slider
# lands on a trigger axis.
AXIS_FILL = ["analog.stick.left.x", "analog.stick.right.x",
             "trigger.left", "trigger.right"]

# Where a layout's face buttons physically sit, for devices whose elements are
# named by label rather than by position. The pad's buttons are positional:
# `button.a` is the bottom one.
POSITION = {
    **{name: {"button.a": "button.b", "button.b": "button.a",
              "button.x": "button.y", "button.y": "button.x"}
       for name in ["SNES_CONTROLLER", "SWITCH_PRO_CONTROLLER",
                    "SWITCH_2_PRO_CONTROLLER", "SWITCH_JOYCONS_CONTROLLER",
                    "SWITCH_2_JOYCONS_CONTROLLER", "WIIU_PRO_CONTROLLER",
                    "WIIU_DRC_CONTROLLER"]},
    **{name: {"button.a": "button.b", "button.b": "button.a"}
       for name in ["NES_CONTROLLER", "GB_LAYOUT", "GBA_LAYOUT",
                    "NDS_LAYOUT", "DSI_LAYOUT", "3DS_LAYOUT", "VB_CONTROLLER"]},
    "GAMECUBE_CONTROLLER": {"button.b": "button.x", "button.x": "button.b"},
    "N64_CONTROLLER": {"button.b": "button.x"},
}

JOYCON_LEFT = {
    "directional.n": "button.x", "directional.e": "button.y",
    "directional.s": "button.b", "directional.w": "button.a",
    "analog.stick.left.x+": "analog.stick.left.y-",
    "analog.stick.left.x-": "analog.stick.left.y+",
    "analog.stick.left.y-": "analog.stick.left.x-",
    "analog.stick.left.y+": "analog.stick.left.x+",
    "button.0": "button.l", "button.1": "button.r",
    "button.l": "none", "trigger.left": "none",
}


JOYCON_RIGHT = {
    "button.a": "button.a", "button.b": "button.x",
    "button.x": "button.b", "button.y": "button.y",
    "analog.stick.right.x+": "analog.stick.left.y+",
    "analog.stick.right.x-": "analog.stick.left.y-",
    "analog.stick.right.y-": "analog.stick.left.x+",
    "analog.stick.right.y+": "analog.stick.left.x-",
    "button.0": "button.l", "button.1": "button.r",
    "button.r": "none", "trigger.right": "none",
}


# The pad control that drives each guest control when the controller is held
# sideways, rail or IR end up. Dolphin's "Sideways Wii Remote" rotates the D-pad
# the same way; the hook rotates motion to match.
HORIZONTAL = {
    "WII_REMOTE_CONTROLLER": {
        "directional.n": "directional.w", "directional.e": "directional.n",
        "directional.s": "directional.e", "directional.w": "directional.s",
        "button.0": "button.a", "button.1": "button.b",
        "button.a": "button.x", "button.b": "button.y",
    },
    "SWITCH_LEFT_JOYCON_CONTROLLER": JOYCON_LEFT,
    "SWITCH_2_LEFT_JOYCON_CONTROLLER": JOYCON_LEFT,
    "SWITCH_RIGHT_JOYCON_CONTROLLER": JOYCON_RIGHT,
    "SWITCH_2_RIGHT_JOYCON_CONTROLLER": JOYCON_RIGHT,
}

POINTER_TO_LEFT_STICK = {
    "pointer.x+": "analog.stick.left.x+", "pointer.x-": "analog.stick.left.x-",
    "pointer.y+": "analog.stick.left.y-", "pointer.y-": "analog.stick.left.y+",
}

# Where the physical device disagrees with the mechanical rule, per
# gamecontrollerdb, or where a control is deliberately left off the pad.
OVERRIDE = {
    # 32/33 GameCube entries put Z on the right shoulder and use no left
    # shoulder at all; "Digital L"/"Digital R" are the click at the end of each
    # analog trigger's travel, the same physical control as the axis.
    "GAMECUBE_CONTROLLER": {
        "button.z": "button.r",
        "button.l": "trigger.left",
        "button.r": "trigger.right",
    },
    # The NSO N64 Controller reports Z as righttrigger and L/R as the
    # shoulders: Z is the underside trigger, not a shoulder button.
    "N64_CONTROLLER": {"button.z": "trigger.right"},
    "DUALSENSE_CONTROLLER": {"button.0": TOUCHPAD},
    "DUALSHOCK_4_CONTROLLER": {"button.0": TOUCHPAD},
    "SWITCH_2_JOYCONS_CONTROLLER": {
        key: "none" for key in ["pointer.right", "pointer.right.x+",
                                "pointer.right.x-", "pointer.right.y+",
                                "pointer.right.y-"]
    },
    "WII_REMOTE_CONTROLLER": {"button.guide": "none", **POINTER_TO_LEFT_STICK},
    "WII_REMOTE_NUNCHUCK_CONTROLLER": {
        "analog.stick.left.x+": "analog.stick.right.x+",
        "analog.stick.left.x-": "analog.stick.right.x-",
        "analog.stick.left.y+": "analog.stick.right.y+",
        "analog.stick.left.y-": "analog.stick.right.y-",
        "button.0": "button.l",
        "button.1": "button.r",
        "button.c": "button.x",
        "button.guide": "none",
        "button.z": "button.y",
        **POINTER_TO_LEFT_STICK,
    },
}


def keypad(document):
    """The buttons forming a telephone keypad, if the device has one.

    A 5200, ColecoVision, Intellivision or Jaguar controller is a gamepad with
    a keypad bolted on. The keypad cannot fit SDL's gamepad and does not belong
    there — but the joystick, Fire, Start, Pause and Reset do, so the transport
    is a property of each key, not of the layout.
    """
    labels = {b["label"]: b["element"] for b in document.get("button", [])}
    digits = [str(n) for n in range(10)]
    if all(d in labels for d in digits) and "*" in labels and "#" in labels:
        return {f"button.{labels[k]}" for k in digits + ["*", "#"]}
    return set()


def taken(claimed, slot):
    return any(slot == c or slot.startswith(c + ".") or c.startswith(slot + ".")
               for c in claimed)


def place_axis(rows, claimed, unplaced, cls, label):
    free = next((s for s in AXIS_FILL if not taken(claimed, s)), None)
    if free is None:
        unplaced.append((f"analog.{cls}", label))
        return
    claimed.add(free)
    for sign in "+-":
        target = f"{free}{sign}" if free.startswith("analog.") else free
        rows[f"analog.{cls}.x{sign}"] = target


def plan(name, document):
    """(rows, spares_used, overflow) for one layout."""
    forced = {**POSITION.get(name, {}), **OVERRIDE.get(name, {})}
    pad = keypad(document)
    claimed = set(forced.values())
    rows, unplaced = {}, []

    for button in document.get("button", []):
        key = f"button.{button['element']}"
        if key in forced:
            continue
        if key in pad:
            rows[key] = "keyboard"
        elif button["element"] in PAD_BUTTONS and key not in claimed:
            claimed.add(key)
        else:
            unplaced.append((key, button["label"]))

    directionals = document.get("directional", [])
    if directionals:
        claimed.add("directional")
    for analog in document.get("analog", []):
        cls = analog.get("class", "stick")
        align = analog.get("alignment")
        if cls in ("rotary", "slider"):
            place_axis(rows, claimed, unplaced, cls, analog.get("label", ""))
        elif cls == "stick":
            claimed.add(f"analog.stick.{align or 'left'}")
        else:
            # A device can carry two of one sensor — the Wii Remote's own
            # accelerometer and the Nunchuk's. SDL has SENSOR_ACCEL_L/_R for
            # exactly that, so alignment selects between them.
            claimed.add(f"{cls}-{align}" if align else cls)
    for trigger in document.get("trigger", []):
        claimed.add(f"trigger.{trigger['alignment']}")

    # A second directional takes the right stick when nothing else wants it.
    for cluster in directionals[1:]:
        align = cluster.get("alignment")
        prefix = f"directional.{align}" if align else "directional"
        label = cluster.get("label", "")
        directions = "nesw"[: cluster["directions"]]
        if not taken(claimed, "analog.stick.right"):
            claimed.add("analog.stick.right")
            rows.update({f"{prefix}.{d}": STICK2[d] for d in directions})
        else:
            unplaced += [(f"{prefix}.{d}", f"{label} {d.upper()}".strip())
                         for d in directions]

    # A touchpad click belongs on SDL's touchpad slot, not on a spare.
    if document.get("touchscreen"):
        for entry in list(unplaced):
            if "touchpad" in entry[1].lower():
                rows[entry[0]] = TOUCHPAD
                claimed.add(TOUCHPAD)
                unplaced.remove(entry)

    free = [s for s in FILL if not taken(claimed, s)]
    overflow = max(0, len(unplaced) - len(free))
    rows.update({key: slot for (key, _), slot in zip(unplaced, free)})
    rows.update(forced)
    rows = {key: slot for key, slot in rows.items() if key != slot}
    spares = sum(1 for slot in rows.values()
                 if slot.startswith("button.") and slot[-1].isdigit())
    return rows, spares, overflow


entries, over = [], []
for path in sorted(ROOT.glob("*.toml")):
    document = tomllib.loads(path.read_text(encoding="utf-8"))["input"]
    rows, spares, overflow = plan(path.stem, document)
    if overflow:
        over.append((path.stem, overflow))
    elif rows:
        entries.append((path.stem, rows, spares))

lines = []
for name, rows, _ in entries:
    lines.append(f"[{name}]")
    width = max(len(key) for key in rows) + 2
    for key in sorted(rows):
        quoted = f'"{key}"'
        lines.append(f'{quoted:<{width}} = "{rows[key]}"')
    lines.append("")

if over:
    lines += ["# Still more buttons than any transport here can carry.", ""]
    for name, overflow in over:
        lines += [f"# {name}: {overflow} unplaced", ""]

out = Path("crates/polymer-config/projection/SDL.proj.toml")
out.parent.mkdir(parents=True, exist_ok=True)
out.write_text("\n".join(lines).rstrip() + "\n", encoding="utf-8", newline="\n")

horizontal = []
for name, rows in sorted(HORIZONTAL.items()):
    horizontal.append(f"[{name}]")
    width = max(len(key) for key in rows) + 2
    for key in sorted(rows):
        quoted = f'"{key}"'
        horizontal.append(f'{quoted:<{width}} = "{rows[key]}"')
    horizontal.append("")
out.with_name("SDL.horizontal.toml").write_text(
    "\n".join(horizontal).rstrip() + "\n", encoding="utf-8", newline="\n")
need_spare = [name for name, _, spares in entries if spares]
print(f"{len(entries)} layout entries, {len(over)} keyboard-transport, "
      f"{len(lines)} lines")
print(f"needing a spare slot: {need_spare or 'none'}")
