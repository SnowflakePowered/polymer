"""Reader for consolespec inputspec and machinespec documents.

consolespec replaces Stone as the source of truth for what a machine's ports
accept and what elements an input device has. Two things changed that the
plugin config parsers have to accommodate.

**Elements are compressed.** consolespec declares clusters and leaves their
binding-key expansion implicit — `axes = 2, alignment = "left"` for a stick,
`directions = 4` for a D-pad. `expand` derives the canonical binding paths used
by plugin configuration documents; it does not recreate Stone's old element
identifiers.

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

import tomllib
from dataclasses import dataclass, field
from pathlib import Path

__all__ = [
    "Element", "InputSpec", "InputGroup", "MachineSpec",
    "SpecError", "find_root", "load_inputspec", "load_machinespec", "expand",
    "check_binding", "check_group",
]


class SpecError(Exception):
    """A consolespec document is missing or does not say what was assumed."""


BUTTON_ELEMENTS = {
    "a", "b", "c", "x", "y", "z", "l", "r", "start", "select",
    "guide", "clickl", "clickr",
}

# A cluster of `directions` directional buttons, in the order the schema
# describes them: the cardinals first, then the intercardinals.
HAT_DIRECTIONS = {
    4: ("N", "E", "S", "W"),
    8: ("N", "E", "S", "W", "NE", "SE", "SW", "NW"),
}

# Each axis of an analog or pointer cluster expands to positive and negative
# binding paths, over as many of x, y, z as the cluster declares.
AXIS_NAMES = ("x", "y", "z")
AXIS_SIGNS = ("+", "-")


@dataclass(frozen=True)
class Element:
    """One expanded element of an input device."""

    kind: str          # button, directional, analog, trigger, rumble, pointer, ...
    label: str | None  # the cluster's label, when it carries one
    path: str = ""     # binding key: <kind>[.<class>][.<alignment>][.<component>]
    cluster: str = ""  # the same, without the component: the cluster it belongs to
    analog: bool = False    # pressure-sensitive button (DualShock 2 face buttons)
    digital: bool = False   # analog-positioned element with a digital signal only
    optional: bool = False  # add-on rather than built in (N64 Rumble Pak)


@dataclass
class InputSpec:
    """An inputspec document with its elements expanded."""

    id: str
    type: str
    elements: tuple[Element, ...]
    path: Path

    def __contains__(self, binding: str) -> bool:
        return any(element.path == binding for element in self.elements)


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


def _add(elements: list[Element], element: Element) -> None:
    """Record an expanded element and default its cluster to its own path."""
    if not element.cluster:
        element = Element(element.kind, element.label, element.path, element.path,
                          element.analog, element.digital, element.optional)
    elements.append(element)


def expand(document: dict, source: str = "<inputspec>") -> tuple[Element, ...]:
    """Expand an inputspec's `[input]` table into canonical binding paths."""
    elements: list[Element] = []

    for entry in document.get("button", []):
        raw = str(entry.get("element", ""))
        if raw not in BUTTON_ELEMENTS:
            if not raw.isdigit() or not 0 <= int(raw) <= 31:
                raise SpecError(f"{source}: unknown button element `{raw}`")
        _add(elements, Element("button", entry.get("label"), _path("button", raw),
                               analog=bool(entry.get("analog"))))

    for entry in document.get("directional", []):
        hats = entry.get("directions")
        directions = HAT_DIRECTIONS.get(hats)
        if directions is None:
            raise SpecError(f"{source}: directional cluster with {hats} directions")
        alignment = entry.get("alignment")
        for direction in directions:
            _add(elements, Element("directional", entry.get("label"),
                                   _path("directional", alignment, direction.lower()),
                                   cluster=_path("directional", alignment)))

    for entry in document.get("analog", []):
        # `class` says what the cluster is, `alignment` says where it sits.
        # Both are optional: class defaults to a stick, and alignment appears
        # only where a device has two clusters of one class to tell apart.
        cluster_class = entry.get("class", "stick")
        alignment = entry.get("alignment")
        axes = entry.get("axes")
        if axes not in (1, 2, 3):
            raise SpecError(f"{source}: analog cluster with {axes} axes")
        for axis in AXIS_NAMES[:axes]:
            for sign in AXIS_SIGNS:
                _add(elements, Element("analog", entry.get("label"),
                                       _path("analog", cluster_class, alignment,
                                             f"{axis}{sign}"),
                                       cluster=_path("analog", cluster_class, alignment),
                                       digital=bool(entry.get("digital"))))

    for entry in document.get("trigger", []):
        alignment = entry.get("alignment")
        if alignment not in ("left", "right"):
            raise SpecError(f"{source}: trigger alignment `{alignment}`")
        _add(elements, Element("trigger", entry.get("label"),
                               _path("trigger", alignment),
                               digital=bool(entry.get("digital"))))

    for entry in document.get("rumble", []):
        size = entry.get("size")
        if size not in ("big", "small"):
            raise SpecError(f"{source}: rumble size `{size}`")
        _add(elements, Element("rumble", entry.get("label"),
                               _path("rumble", entry.get("alignment") or size),
                               optional=bool(entry.get("optional"))))

    for entry in document.get("pointer", []):
        dimensions = entry.get("dimensions")
        if dimensions not in (2, 3):
            raise SpecError(f"{source}: pointer with {dimensions} dimensions")
        # The absolute position, then the relative axes each dimension carries.
        align = entry.get("alignment")
        _add(elements, Element("pointer", entry.get("label"),
                               _path("pointer", align)))
        for axis in AXIS_NAMES[:dimensions]:
            for sign in AXIS_SIGNS:
                _add(elements, Element("pointer", entry.get("label"),
                                       _path("pointer", align, f"{axis}{sign}"),
                                       cluster=_path("pointer", align)))

    for kind in ("touchscreen", "microphone", "camera"):
        for entry in document.get(kind, []):
            _add(elements, Element(kind, entry.get("label"),
                                   _path(kind, entry.get("alignment"))))

    if not elements:
        raise SpecError(f"{source}: no elements")
    return tuple(elements)


def check_binding(spec: InputSpec, binding: str, report, context: str) -> bool:
    """Whether `binding` is a canonical path the device actually has."""
    if binding in spec:
        return True
    report.error(f"{context}: `{binding}` is not a binding of {spec.id}")
    return False


def check_group(machine: MachineSpec, group: str, layouts, ports: int,
                report, context: str) -> InputGroup:
    """Check a plugin's port group against what the machine declares.

    A layout the group does not accept is an error: the plugin is offering a
    device that cannot be plugged in there. Exceeding the declared port count
    is also an error because Polymer validates plugin ports against that
    topology. Multitap slots must therefore be represented by the machine
    specification before a plugin can expose them.
    """
    spec = machine.group(group)
    for layout in layouts:
        if layout not in spec.inputs:
            report.error(f"{context}: {machine.id}/{group} does not accept "
                         f"`{layout}`; it accepts {', '.join(spec.inputs) or 'nothing'}")
    if spec.ports is not None and ports > spec.ports:
        report.error(f"{context}: {ports} ports declared for {machine.id}/{group}, "
                     f"which has {spec.ports}")
    return spec


def find_root(start: Path | None = None) -> Path:
    """Locate the consolespec repository by walking up from `start`."""
    here = (start or Path(__file__)).resolve()
    for candidate in (here, *here.parents):
        roots = (candidate, candidate / "consolespec", candidate.parent / "consolespec")
        for root in roots:
            definitions = root / "definitions"
            if (definitions / "inputspec").is_dir() and (definitions / "machinespec").is_dir():
                return root
    raise SpecError(f"no consolespec repository near {here}")


_inputs: dict[tuple[Path, str], InputSpec] = {}
_machines: dict[tuple[Path, str], MachineSpec] = {}


def load_inputspec(root: Path, layout: str) -> InputSpec:
    """Load and expand one inputspec by id."""
    key = (root, layout)
    if key in _inputs:
        return _inputs[key]
    path = root / "definitions" / "inputspec" / f"{layout}.toml"
    if not path.is_file():
        raise SpecError(
            f"no inputspec `{layout}` in {root / 'definitions' / 'inputspec'}"
        )
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
    path = root / "definitions" / "machinespec" / f"{machine}.toml"
    if not path.is_file():
        raise SpecError(
            f"no machinespec `{machine}` in {root / 'definitions' / 'machinespec'}"
        )
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
