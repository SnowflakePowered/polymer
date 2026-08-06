"""Generate crates/input/snowglass-input/projection/SDL.proj.toml from consolespec.

Run from the repository root after changing an inputspec:

    python plugins/support/gen_projection.py

The output is committed and reviewed: this proposes an allocation, it does not
own one. Two bugs — the Wii Remote + Nunchuk's colliding accelerometers and the
3DS depth slider claiming a whole stick — were caught by review, not by the
rule, so a human reads the diff.


The allocation rule comes from SDL's own gamecontrollerdb: real hardware fills
*unused standard slots* before it reaches for misc1 or a paddle. A Wii Remote's
1 and 2 become X and Y because a Wii Remote has no X/Y face buttons; Saturn's C
and Z become shoulders because a Saturn pad's shoulder slots are free. Only when
every standard slot is taken does anything land on a spare.
"""
import tomllib
from pathlib import Path

ROOT = Path("consolespec/inputspec")

# What each consolespec element claims by default.
BUTTON_SLOT = {
    "a": "south", "b": "east", "x": "west", "y": "north",
    "select": "back", "guide": "guide", "start": "start",
    "clickl": "left-stick-click", "clickr": "right-stick-click",
    "l": "left-shoulder", "r": "right-shoulder",
}
# Free standard slots in the order hardware fills them, then the true spares.
FILL = ["west", "north", "left-shoulder", "right-shoulder",
        "left-trigger", "right-trigger",
        "misc1", "right-paddle1", "left-paddle1", "right-paddle2",
        "left-paddle2"]
# A second directional becomes the right stick's digital half-axes, which is
# what Nintendo's own NSO N64 controller reports for the C-buttons.
STICK2 = {"n": "right-stick.y-", "s": "right-stick.y+",
          "w": "right-stick.x-", "e": "right-stick.x+"}
# A rotary dial or a slider is one degree of freedom, so it takes one axis, not
# a whole stick. The 3DS has both Circle Pads occupied, so its depth slider
# lands on a trigger axis.
AXIS_FILL = ["left-stick.x", "right-stick.x", "left-trigger", "right-trigger"]

# Where the physical device disagrees with the mechanical rule, per
# gamecontrollerdb. Both are shoulder-region shapes no rule can infer.
OVERRIDE = {
    # 32/33 GameCube entries put Z on the right shoulder and use no left
    # shoulder at all; "Digital L"/"Digital R" are the click at the end of each
    # analog trigger's travel, the same physical control as the axis.
    "GAMECUBE_CONTROLLER": {
        "button.z": "right-shoulder",
        "button.l": "left-trigger",
        "button.r": "right-trigger",
    },
    # The NSO N64 Controller reports Z as righttrigger and L/R as the
    # shoulders: Z is the underside trigger, not a shoulder button.
    "N64_CONTROLLER": {"button.z": "right-trigger"},
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


def plan(name, document):
    """(rows, spares_used, overflow) for one layout."""
    forced = OVERRIDE.get(name, {})
    pad = keypad(document)
    claimed = set(forced.values())
    rows, unplaced = [], []

    for button in document.get("button", []):
        key = f"button.{button['element']}"
        if key in forced:
            rows.append((key, forced[key], button["label"]))
            continue
        if key in pad:
            rows.append((key, "keyboard", button["label"]))
            continue
        slot = BUTTON_SLOT.get(button["element"])
        if slot and slot not in claimed:
            claimed.add(slot)
            continue                      # the default already covers it
        unplaced.append((key, button["label"]))

    directionals = document.get("directional", [])
    if directionals:
        claimed.add("hat")
    for analog in document.get("analog", []):
        cls = analog.get("class", "stick")
        align = analog.get("alignment")
        if cls in ("rotary", "slider"):
            free = next((s for s in AXIS_FILL if s not in claimed
                         and s.split(".")[0] not in claimed), None)
            if free is None:
                unplaced.append((f"analog.{cls}", analog.get("label", "")))
                continue
            claimed.add(free)
            if free != AXIS_FILL[0]:
                rows.append((f"analog.{cls}", free, analog.get("label", "")))
        elif cls == "stick":
            claimed.add(f"{align or 'left'}-stick")
        else:
            # A device can carry two of one sensor — the Wii Remote's own
            # accelerometer and the Nunchuk's. SDL has SENSOR_ACCEL_L/_R for
            # exactly that, so alignment selects between them.
            claimed.add(f"{cls}-{align}" if align else cls)
    for trigger in document.get("trigger", []):
        claimed.add(f"{trigger['alignment']}-trigger")

    # A second directional takes the right stick when nothing else wants it.
    for cluster in directionals[1:]:
        align = cluster.get("alignment")
        prefix = f"directional.{align}" if align else "directional"
        label = cluster.get("label", "")
        directions = "nesw"[: cluster["directions"]]
        if "right-stick" not in claimed:
            claimed.add("right-stick")
            rows += [(f"{prefix}.{d}", STICK2[d], f"{label} {d.upper()}".strip())
                     for d in directions]
        else:
            unplaced += [(f"{prefix}.{d}", f"{label} {d.upper()}".strip())
                         for d in directions]

    # A touchpad click belongs on SDL's touchpad slot, not on a spare.
    if document.get("touchscreen"):
        for entry in list(unplaced):
            if "touchpad" in entry[1].lower():
                rows.append((entry[0], "touchpad", entry[1]))
                claimed.add("touchpad")
                unplaced.remove(entry)

    free = [s for s in FILL if s not in claimed]
    overflow = max(0, len(unplaced) - len(free))
    rows += [(key, slot, label)
             for (key, label), slot in zip(unplaced, free)]
    spares = sum(1 for _, slot, _ in rows
                 if slot.startswith(("misc", "left-paddle", "right-paddle")))
    # Keep a keypad in its printed order rather than TOML's.
    rows.sort(key=lambda row: (row[1] != "keyboard", ))
    return rows, spares, overflow


entries, over = [], []
for path in sorted(ROOT.glob("*.toml")):
    document = tomllib.loads(path.read_text(encoding="utf-8"))["input"]
    rows, spares, overflow = plan(path.stem, document)
    if overflow:
        over.append((path.stem, overflow))
    elif rows:
        entries.append((path.stem, rows, spares))

lines = [
    "# Projects consolespec clusters onto SDL's virtual gamepad.",
    "#",
    "# Mapping, not description: consolespec says a cluster has two degrees of",
    "# freedom, and this file decides which SDL axes carry them. Shared, so",
    '# "which physical button is C-Up" is the same in every emulator.',
    "#",
    "# Allocation follows SDL's gamecontrollerdb: unused standard slots first,",
    "# spares only when none are left. A layout appears below only when the",
    "# defaults do not already cover it.",
    "",
    "[defaults.button]",
]
lines += [f'{element} = "{slot}"' for element, slot in BUTTON_SLOT.items()]
lines += [
    "",
    "# A sole cluster of these kinds takes the standard slot. A second one is an",
    "# error unless the layout below says where it goes.",
    "[defaults.directional]",
    '"" = "hat"',
    "",
    "[defaults.analog]",
    "# Keys are quoted so a dot reads as part of the cluster path rather than",
    "# as TOML nesting: `gyroscope` and `gyroscope.left` are siblings, and a",
    "# bare dotted key cannot extend a string.",
    '"stick.left" = "left-stick"',
    '"stick.right" = "right-stick"',
    '"rotary" = "left-stick.x"',
    '"slider" = "left-stick.x"',
    '"gyroscope" = "gyroscope"',
    '"gyroscope.left" = "gyroscope-left"',
    '"gyroscope.right" = "gyroscope-right"',
    '"accelerometer" = "accelerometer"',
    '"accelerometer.left" = "accelerometer-left"',
    '"accelerometer.right" = "accelerometer-right"',
    "",
    "[defaults.trigger]",
    'left = "left-trigger"',
    'right = "right-trigger"',
    "",
    "[defaults.pointer]",
    '"" = "pointer"',
    "",
]
for name, rows, _ in entries:
    lines.append(f"[layouts.{name}]")
    width = max(len(key) for key, _, _ in rows) + 2
    for key, slot, label in rows:
        quoted = f'"{key}"'
        comment = f"   # {label}" if label else ""
        lines.append(f'{quoted:<{width}} = "{slot}"{comment}')
    lines.append("")

if over:
    lines += ["# Still more buttons than any transport here can carry.", ""]
    for name, overflow in over:
        lines += [f"# {name}: {overflow} unplaced", ""]

out = Path("crates/input/snowglass-input/projection/SDL.proj.toml")
out.parent.mkdir(parents=True, exist_ok=True)
out.write_text("\n".join(lines).rstrip() + "\n", encoding="utf-8", newline="\n")
need_spare = [name for name, _, spares in entries if spares]
print(f"{len(entries)} layout entries, {len(over)} keyboard-transport, "
      f"{len(lines)} lines")
print(f"needing a spare slot: {need_spare or 'none'}")
