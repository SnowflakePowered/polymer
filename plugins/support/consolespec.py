"""Reader for consolespec inputspec and machinespec documents.

consolespec replaces Stone as the source of truth for what a machine's ports
accept and what elements an input device has. Two things changed that the
plugin config parsers have to accommodate.

**Elements are compressed.** Stone spelled every element out: a stick was four
named entries (`AxisLeftAnalogPositiveX` and friends), a D-pad was four
`Directional*` entries. consolespec declares the *cluster* and leaves the
expansion implicit — `axes = 2, alignment = "left"` for the stick, `directions = 4`
for the D-pad. The element vocabulary is unchanged; it is now derived rather
than read. `expand` below is that derivation, and it is the only place the
naming rules live.

**Ports are grouped.** A machinespec no longer offers a flat port count; it
names groups (`[input.wii]`, `[input.gamecube]`) that each carry their own port
count and their own set of acceptable devices. A Wii has four Wii Remote ports
*and* four GameCube ports, which the old flat numbering could only express by
picking an arbitrary interleaving. Ports in a plugin's input document are now
keyed `[platforms.<machine>.ports.<group>.<n>]` to match.

Nothing here reads Stone. A plugin that still names a Stone-era layout id
(`GCN_CONTROLLER` for what consolespec calls `GAMECUBE_CONTROLLER`) fails to
load, which is the intended way to find them.
"""

from __future__ import annotations

import re
import tomllib
from dataclasses import dataclass, field
from pathlib import Path

__all__ = [
    "Element", "InputSpec", "InputGroup", "MachineSpec", "Projection",
    "SpecError", "find_root", "load_inputspec", "load_machinespec", "expand",
    "check_binding", "check_group", "load_projection",
]


class SpecError(Exception):
    """A consolespec document is missing or does not say what was assumed."""


# Buttons name their element semantically; the element id capitalizes it.
# Anything not listed is a numbered button ("0".."31" -> Button0..Button31).
BUTTON_ELEMENTS = {
    "a": "A", "b": "B", "c": "C", "x": "X", "y": "Y", "z": "Z",
    "l": "L", "r": "R",
    "start": "Start", "select": "Select", "guide": "Guide",
    "clickl": "ClickL", "clickr": "ClickR",
}

# A cluster of `directions` directional buttons, in the order the schema
# describes them: the cardinals first, then the intercardinals.
HAT_DIRECTIONS = {
    4: ("N", "E", "S", "W"),
    8: ("N", "E", "S", "W", "NE", "SE", "SW", "NW"),
}

# An analog cluster's (class, alignment) picks the element prefix. Sticks are
# named for the side of the device; motion sensors for the quantity they
# measure, and only while a device has just one of them.
ANALOG_PREFIXES = {
    # A device with one stick need not say which side it is on. A rotary dial
    # has no side at all and reads as the primary analog axis.
    ("stick", None): "AxisLeftAnalog",
    ("stick", "left"): "AxisLeftAnalog",
    ("stick", "right"): "AxisRightAnalog",
    ("rotary", None): "AxisLeftAnalog",
    ("rotary", "left"): "AxisLeftAnalog",
    ("rotary", "right"): "AxisRightAnalog",
    ("gyroscope", None): "GyroscopeAxis",
    ("accelerometer", None): "AccelerometerAxis",
}


def _pascal(text: str) -> str:
    """`"3D Slider"` -> `"3DSlider"`, for ids derived from a label."""
    return "".join(word[:1].upper() + word[1:]
                   for word in re.split(r"[^A-Za-z0-9]+", text) if word)

# Each axis of an analog or pointer cluster expands to a positive and a
# negative element, over as many of X, Y, Z as the cluster declares.
AXIS_NAMES = ("X", "Y", "Z")
AXIS_SIGNS = ("Positive", "Negative")


@dataclass(frozen=True)
class Element:
    """One expanded element of an input device."""

    id: str
    kind: str          # button, directional, analog, trigger, rumble, pointer, ...
    label: str | None  # the cluster's label, when it carries one
    path: str = ""     # binding key: <kind>[.<class>][.<alignment>][.<component>]
    cluster: str = ""  # the same, without the component; what the projection places
    analog: bool = False    # pressure-sensitive button (DualShock 2 face buttons)
    digital: bool = False   # analog-positioned element with a digital signal only
    optional: bool = False  # add-on rather than built in (N64 Rumble Pak)


@dataclass
class InputSpec:
    """An inputspec document with its elements expanded."""

    id: str
    type: str
    elements: dict[str, Element]
    path: Path

    def __contains__(self, element: str) -> bool:
        return element in self.elements

    def ids(self) -> set[str]:
        return set(self.elements)

    def path(self, element: str) -> str:
        """The binding key for an element, e.g. `analog.stick.left.x+`."""
        return self.elements[element].path


@dataclass
class InputGroup:
    """One `[input.<name>]` group of a machinespec: a family of ports."""

    name: str
    inputs: tuple[str, ...] = ()
    ports: int | None = None
    accessories: bool = False


@dataclass
class MachineSpec:
    """A machinespec document, reduced to the parts the input documents need."""

    id: str
    name: str
    groups: dict[str, InputGroup] = field(default_factory=dict)
    path: Path | None = None

    def group(self, name: str) -> InputGroup:
        try:
            return self.groups[name]
        except KeyError:
            raise SpecError(
                f"{self.id} has no input group `{name}`; it has "
                f"{', '.join(sorted(self.groups)) or 'none'}") from None


def _path(kind: str, *parts: str | None) -> str:
    """`button.a`, `directional.right.n`, `analog.stick.left.x+`.

    The binding key. It is context-free — built from a cluster's own fields —
    so adding a cluster to a device never renames an existing key.
    """
    return ".".join([kind, *(part for part in parts if part)])


def _finish(element: Element) -> Element:
    """A cluster with one component is its own cluster path."""
    if element.cluster:
        return element
    return Element(element.id, element.kind, element.label, element.path,
                   element.path, element.analog, element.digital, element.optional)


def _unique(elements: dict[str, Element], element: Element) -> None:
    """Record an element, disambiguating a repeated id with an ordinal.

    Most devices have at most one cluster per discriminator, but not all: the
    N64 pad declares two directionals, the DualSense two small rumble motors.
    The second and later get a `2`, `3`, ... suffix in declaration order, so a
    reordered document changes which is which but a stable one is stable.
    """
    element = _finish(element)
    if element.id not in elements:
        elements[element.id] = element
        return
    ordinal = 2
    while f"{element.id}{ordinal}" in elements:
        ordinal += 1
    suffixed = f"{element.id}{ordinal}"
    elements[suffixed] = Element(suffixed, element.kind, element.label, element.path,
                                 element.cluster, element.analog, element.digital,
                                 element.optional)


def expand(document: dict, source: str = "<inputspec>") -> dict[str, Element]:
    """Expand an inputspec's `[input]` table into its element vocabulary."""
    elements: dict[str, Element] = {}

    for entry in document.get("button", []):
        raw = str(entry.get("element", ""))
        name = BUTTON_ELEMENTS.get(raw)
        if name is None:
            if not raw.isdigit() or not 0 <= int(raw) <= 31:
                raise SpecError(f"{source}: unknown button element `{raw}`")
            name = raw
        _unique(elements, Element(f"Button{name}", "button", entry.get("label"),
                                  _path("button", raw),
                                  analog=bool(entry.get("analog"))))

    for entry in document.get("directional", []):
        hats = entry.get("directions")
        directions = HAT_DIRECTIONS.get(hats)
        if directions is None:
            raise SpecError(f"{source}: directional cluster with {hats} directions")
        alignment = entry.get("alignment")
        for direction in directions:
            _unique(elements, Element(f"Directional{direction}", "directional",
                                      entry.get("label"),
                                      _path("directional", alignment,
                                            direction.lower()),
                                      cluster=_path("directional", alignment)))

    for entry in document.get("analog", []):
        # `class` says what the cluster is, `alignment` says where it sits.
        # Both are optional: class defaults to a stick, and alignment appears
        # only where a device has two clusters of one class to tell apart.
        cluster_class = entry.get("class", "stick")
        alignment = entry.get("alignment")
        prefix = ANALOG_PREFIXES.get((cluster_class, alignment))
        if prefix is None:
            # A cluster sharing its class with another on the same device, or a
            # slider, has no id in this vocabulary. Qualifying by position or
            # label keeps it unique until binding keys move to cluster paths.
            qualifier = alignment or entry.get("label")
            if not qualifier:
                raise SpecError(
                    f"{source}: `{cluster_class}` cluster with no alignment or label")
            prefix = _pascal(qualifier) + _pascal(cluster_class) + "Axis"
        axes = entry.get("axes")
        if axes not in (1, 2, 3):
            raise SpecError(f"{source}: analog cluster with {axes} axes")
        for axis in AXIS_NAMES[:axes]:
            for sign in AXIS_SIGNS:
                component = f"{axis.lower()}{'+' if sign == 'Positive' else '-'}"
                _unique(elements, Element(f"{prefix}{sign}{axis}", "analog",
                                          entry.get("label"),
                                          _path("analog", cluster_class, alignment,
                                                component),
                                          cluster=_path("analog", cluster_class,
                                                        alignment),
                                          digital=bool(entry.get("digital"))))

    for entry in document.get("trigger", []):
        alignment = entry.get("alignment")
        if alignment not in ("left", "right"):
            raise SpecError(f"{source}: trigger alignment `{alignment}`")
        _unique(elements, Element(f"Trigger{alignment.capitalize()}", "trigger",
                                  entry.get("label"),
                                  _path("trigger", alignment),
                                  digital=bool(entry.get("digital"))))

    for entry in document.get("rumble", []):
        size = entry.get("size")
        if size not in ("big", "small"):
            raise SpecError(f"{source}: rumble size `{size}`")
        _unique(elements, Element(f"Rumble{size.capitalize()}", "rumble",
                                  entry.get("label"),
                                  _path("rumble", entry.get("alignment") or size),
                                  optional=bool(entry.get("optional"))))

    for entry in document.get("pointer", []):
        dimensions = entry.get("dimensions")
        if dimensions not in (2, 3):
            raise SpecError(f"{source}: pointer with {dimensions} dimensions")
        # The absolute position, then the relative axes each dimension carries.
        align = entry.get("alignment")
        _unique(elements, Element(f"Pointer{dimensions}D", "pointer",
                                  entry.get("label"),
                                  _path("pointer", align)))
        for axis in AXIS_NAMES[:dimensions]:
            for sign in AXIS_SIGNS:
                component = f"{axis.lower()}{'+' if sign == 'Positive' else '-'}"
                _unique(elements, Element(f"PointerAxis{sign}{axis}", "pointer",
                                          entry.get("label"),
                                          _path("pointer", align, component),
                                          cluster=_path("pointer", align)))

    for kind, name in (("touchscreen", "Touchscreen"),
                       ("microphone", "Microphone"), ("camera", "Camera")):
        for entry in document.get(kind, []):
            _unique(elements, Element(name, kind, entry.get("label"),
                                      _path(kind, entry.get("alignment"))))

    if not elements:
        raise SpecError(f"{source}: no elements")
    return elements


def check_binding(spec: InputSpec, element: str, report, context: str) -> bool:
    """Whether `element` is one the device actually has, reporting if not.

    Stone let a plugin name any element it liked; nothing checked that the
    layout had it. Now that the vocabulary is derived from the device, a
    binding for an element the device does not have is a plugin bug — Cemu's
    Wii Remote table was a copy of its Pro Controller table, which is how this
    check found it — so it is an error, not a warning.
    """
    if element in spec:
        return True
    report.error(f"{context}: `{element}` is not an element of {spec.id}")
    return False


def check_group(machine: MachineSpec, group: str, layouts, ports: int,
                report, context: str) -> InputGroup:
    """Check a plugin's port group against what the machine declares.

    A layout the group does not accept is an error: the plugin is offering a
    device that cannot be plugged in there. More ports than the machine
    declares is only a warning, because the machine's count is its base port
    count and a multitap legitimately exceeds it.
    """
    spec = machine.group(group)
    for layout in layouts:
        if layout not in spec.inputs:
            report.error(f"{context}: {machine.id}/{group} does not accept "
                         f"`{layout}`; it accepts {', '.join(spec.inputs) or 'nothing'}")
    if spec.ports is not None and ports > spec.ports:
        report.warn(f"{context}: {ports} ports declared for {machine.id}/{group}, "
                    f"which has {spec.ports}; correct only if the extras are a "
                    f"multitap the machine's spec does not count")
    return spec


def find_root(start: Path | None = None) -> Path:
    """Locate the `consolespec` directory by walking up from `start`."""
    here = (start or Path(__file__)).resolve()
    for candidate in (here, *here.parents):
        root = candidate / "consolespec"
        if (root / "inputspec").is_dir() and (root / "machinespec").is_dir():
            return root
    raise SpecError(f"no consolespec directory above {here}")


_inputs: dict[tuple[Path, str], InputSpec] = {}
_machines: dict[tuple[Path, str], MachineSpec] = {}


def load_inputspec(root: Path, layout: str) -> InputSpec:
    """Load and expand one inputspec by id."""
    key = (root, layout)
    if key in _inputs:
        return _inputs[key]
    path = root / "inputspec" / f"{layout}.toml"
    if not path.is_file():
        raise SpecError(f"no inputspec `{layout}` in {root / 'inputspec'}")
    document = tomllib.loads(path.read_text(encoding="utf-8")).get("input")
    if document is None:
        raise SpecError(f"{path}: no [input] table")
    if document.get("id") != layout:
        raise SpecError(f"{path}: declares id `{document.get('id')}`")
    spec = InputSpec(layout, document.get("type", "controller"),
                     expand(document, str(path)), path)
    _inputs[key] = spec
    return spec


def load_machinespec(root: Path, machine: str) -> MachineSpec:
    """Load one machinespec by id, keeping its input groups."""
    key = (root, machine)
    if key in _machines:
        return _machines[key]
    path = root / "machinespec" / f"{machine}.toml"
    if not path.is_file():
        raise SpecError(f"no machinespec `{machine}` in {root / 'machinespec'}")
    document = tomllib.loads(path.read_text(encoding="utf-8"))
    identity = document.get("machine", {})
    if identity.get("id") != machine:
        raise SpecError(f"{path}: declares id `{identity.get('id')}`")
    section = document.get("input", {})
    groups: dict[str, InputGroup] = {}
    # `groups` lists the group names in the machine's own order; each names a
    # sibling table. A machine with no input section has no groups at all.
    for name in section.get("groups", []):
        table = section.get(name)
        if table is None:
            raise SpecError(f"{path}: group `{name}` has no [input.{name}] table")
        groups[name] = InputGroup(name, tuple(table.get("inputs", ())),
                                  table.get("ports"), bool(table.get("accessories")))
    spec = MachineSpec(machine, identity.get("name", machine), groups, path)
    _machines[key] = spec
    return spec


# --------------------------------------------------------------------------
# SDL projection
# --------------------------------------------------------------------------


class Projection:
    """Reader for `crates/input/snowglass-input/projection/SDL.proj.toml`.

    Mirrors the resolution order in `snowglass_input::projection` so a
    generator and the runtime agree on whether a binding can ever fire. A
    component entry wins over its cluster, which is what keeps a device's
    second directional off the hat its first one holds.
    """

    def __init__(self, defaults: dict[str, str], layouts: dict[str, dict[str, str]]):
        self.defaults = defaults
        self.layouts = layouts

    @staticmethod
    def _flatten(prefix: str, value, into: dict[str, str]) -> None:
        if isinstance(value, str):
            into[prefix.rstrip(".")] = value
        elif isinstance(value, dict):
            for key, nested in value.items():
                path = prefix.rstrip(".") if key == "" else f"{prefix}{key}"
                Projection._flatten(f"{path}.", nested, into)

    @classmethod
    def load(cls, path: Path) -> "Projection":
        document = tomllib.loads(path.read_text(encoding="utf-8"))
        defaults: dict[str, str] = {}
        for kind, value in document.get("defaults", {}).items():
            cls._flatten(f"{kind}.", value, defaults)
        return cls(defaults, document.get("layouts", {}))

    def slot(self, layout: str, element: Element) -> str | None:
        """The SDL slot an element reaches, or None if it reaches nothing."""
        table = self.layouts.get(layout, {})
        if element.path in table:
            return table[element.path]
        if element.cluster in table:
            return table[element.cluster]
        probe = element.cluster
        while probe:
            if probe in self.defaults:
                return self.defaults[probe]
            if "." not in probe:
                return None
            probe = probe.rsplit(".", 1)[0]
        return None


def load_projection(root: Path | None = None) -> Projection:
    """Load the projection that ships with `snowglass-input`."""
    here = (root or Path(__file__)).resolve()
    for candidate in (here, *here.parents):
        table = (candidate / "crates/input/snowglass-input/projection/SDL.proj.toml")
        if table.is_file():
            return Projection.load(table)
    raise SpecError(f"no SDL.proj.toml above {here}")
