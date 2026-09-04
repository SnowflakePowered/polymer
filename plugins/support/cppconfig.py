"""Shared C++-to-TOML machinery for the emulator config generators.

Every plugin's `support/parse_config.py` reads an emulator's source tree and
writes the same three documents. What differs between them is where the settings
live and how they are declared; what does not differ is the C++ lexing, the
preprocessor, constant-expression evaluation, enum extraction, and TOML output.
That half lives here.

Plugins configure the emulator-specific bits by assigning to the module globals
before loading anything:

    cppconfig.DEFINES = {"_WIN32": True}
    cppconfig.UNDEFINES = {"__APPLE__"}
    cppconfig.KEY_OVERRIDES = {"VSync": "vsync"}
"""

from __future__ import annotations

import ast
import re
import sys
from dataclasses import dataclass, field
from pathlib import Path

# Dolphin is compiled per platform and polymer targets Windows first, so the
# Windows branch of every #ifdef is the one that describes the config we write.
DEFINES = {"_WIN32": True, "WIN32": True, "HAS_LIBMGBA": True}

UNDEFINES = {
    "USE_RETRO_ACHIEVEMENTS",
    "__APPLE__",
    "__linux__",
    "ANDROID",
    "__ANDROID__",
    "HAVE_XRANDR",
    "USE_UPNP",
}

# `_trans` is a no-op translation marker; `fmt` wrappers appear in defaults.
NOOP_MACROS = ("_trans", "wxTRANSLATE")

# Emitted option keys come from Dolphin's INI key names. A handful read badly
# under the generic camel-case split; the rest are left as the algorithm finds
# them so that new upstream keys need no maintenance here.
KEY_OVERRIDES = {
    "VSync": "vsync",
    "MEM1Size": "mem1_size",
    "MEM2Size": "mem2_size",
}

ACRONYMS = {
    "efb", "xfb", "fps", "vps", "ir", "gpu", "cpu", "dsp", "hle", "lle", "jit",
    "sd", "usb", "ui", "png", "ssaa", "msaa", "fsaa", "vi", "xf", "tmem", "gba",
    "hdmi", "ntsc", "pal", "url", "id", "ini", "ram", "iso", "wad", "hdd", "mmu",
    "fma", "avi", "dpl2", "udp", "tcp", "ipl", "bba", "exi", "si", "gc", "wii",
    "hdr", "sdr", "efb2ram", "dsp", "aram", "gci", "nand", "vbi", "fifo",
}

INT_TYPES = {
    "int": ("dec", None), "s8": ("dec", None), "s16": ("dec", None),
    "s32": ("dec", None), "s64": ("dec", None), "long": ("dec", None),
    "u8": ("hex", 2), "u16": ("hex", 4), "u32": ("hex", 8), "u64": ("hex", 16),
    "size_t": ("dec", None), "unsigned int": ("dec", None),
}

FLOAT_TYPES = {"float", "double"}

STRING_TYPES = {"std::string", "string"}

class Bail(Exception):
    """A parse failure that means the extractor no longer matches Dolphin."""

@dataclass
class Report:
    notes: list[str] = field(default_factory=list)
    warnings: list[str] = field(default_factory=list)
    errors: list[str] = field(default_factory=list)

    def note(self, message: str) -> None:
        self.notes.append(message)

    def warn(self, message: str) -> None:
        self.warnings.append(message)

    def error(self, message: str) -> None:
        self.errors.append(message)

    def emit(self, stream=sys.stderr) -> None:
        for kind, entries in (("note", self.notes), ("warning", self.warnings),
                              ("error", self.errors)):
            for entry in entries:
                print(f"{kind}: {entry}", file=stream)

def strip_comments(text: str) -> str:
    """Blank out comments, preserving offsets so line numbers stay usable."""
    out = []
    i, n = 0, len(text)
    while i < n:
        c = text[i]
        if c in "\"'":
            end = skip_literal(text, i)
            out.append(text[i:end])
            i = end
        elif text.startswith("//", i):
            end = text.find("\n", i)
            end = n if end < 0 else end
            out.append(" " * (end - i))
            i = end
        elif text.startswith("/*", i):
            end = text.find("*/", i + 2)
            end = n if end < 0 else end + 2
            out.append("".join(ch if ch == "\n" else " " for ch in text[i:end]))
            i = end
        else:
            out.append(c)
            i += 1
    return "".join(out)

def skip_literal(text: str, start: int) -> int:
    """Return the index just past the string or char literal at `start`."""
    quote = text[start]
    i = start + 1
    while i < len(text):
        if text[i] == "\\":
            i += 2
            continue
        if text[i] == quote:
            return i + 1
        i += 1
    return len(text)

def preprocess(text: str, report: Report, where: str) -> str:
    """Resolve #if/#ifdef against DEFINES/UNDEFINES, blanking dead branches.

    Unknown conditions keep their first branch, which is the upstream author's
    own default ordering; the choice is reported rather than assumed silently.
    """
    lines = text.split("\n")
    out: list[str] = []
    # Each frame: (taken_now, already_taken_a_branch, condition_was_known)
    stack: list[tuple[bool, bool, bool]] = []

    def live() -> bool:
        return all(frame[0] for frame in stack)

    for number, line in enumerate(lines, 1):
        stripped = line.strip()
        directive = re.match(r"#\s*(ifdef|ifndef|if|elif|else|endif)\b(.*)", stripped)
        if not directive:
            out.append(line if live() else "")
            continue
        kind, rest = directive.group(1), directive.group(2).strip()
        if kind in ("ifdef", "ifndef", "if"):
            if kind == "ifdef":
                value = evaluate_condition(f"defined({rest})")
            elif kind == "ifndef":
                value = evaluate_condition(f"!defined({rest})")
            else:
                value = evaluate_condition(rest)
            known = value is not None
            if not known:
                report.note(f"{where}:{number}: unevaluated `#{kind} {rest}`, keeping first branch")
                value = True
            stack.append((bool(value), bool(value), known))
        elif kind == "elif":
            if not stack:
                raise Bail(f"{where}:{number}: #elif without #if")
            _, taken, known = stack[-1]
            value = evaluate_condition(rest)
            if value is None:
                known = False
                value = False
            stack[-1] = (bool(value) and not taken, taken or bool(value), known)
        elif kind == "else":
            if not stack:
                raise Bail(f"{where}:{number}: #else without #if")
            _, taken, known = stack[-1]
            stack[-1] = (not taken, True, known)
        else:
            if not stack:
                raise Bail(f"{where}:{number}: #endif without #if")
            stack.pop()
        out.append("")
    if stack:
        raise Bail(f"{where}: unbalanced preprocessor conditionals")
    return "\n".join(out)

def evaluate_condition(expression: str) -> bool | None:
    """Evaluate a preprocessor condition, or None when it is not decidable."""
    text = expression.strip()
    if not text:
        return None
    text = re.sub(r"defined\s*\(\s*(\w+)\s*\)", r"defined_\1", text)
    text = re.sub(r"defined\s+(\w+)", r"defined_\1", text)

    def resolve(name: str) -> bool | None:
        symbol = name[len("defined_"):] if name.startswith("defined_") else name
        if symbol in DEFINES:
            return True
        if symbol in UNDEFINES:
            return False
        return None

    tokens = re.findall(r"[A-Za-z_]\w*|&&|\|\||[!()]|\d+", text)
    if not tokens:
        return None
    python: list[str] = []
    for token in tokens:
        if token == "&&":
            python.append(" and ")
        elif token == "||":
            python.append(" or ")
        elif token == "!":
            python.append(" not ")
        elif token in "()":
            python.append(token)
        elif token.isdigit():
            python.append("True" if int(token) else "False")
        else:
            value = resolve(token)
            if value is None:
                return None
            python.append("True" if value else "False")
    try:
        return bool(eval("".join(python), {"__builtins__": {}}, {}))  # noqa: S307
    except Exception:
        return None

def load_source(path: Path, report: Report) -> str:
    if not path.is_file():
        raise Bail(f"missing Dolphin source file: {path}")
    text = path.read_text(encoding="utf-8", errors="replace")
    text = strip_comments(text)
    return preprocess(text, report, path.name)

def match_braces(text: str, start: int, opener: str = "{", closer: str = "}") -> int:
    """Index just past the delimiter matching the one at `start`."""
    depth = 0
    i = start
    while i < len(text):
        c = text[i]
        if c in "\"'":
            i = skip_literal(text, i)
            continue
        if c == opener:
            depth += 1
        elif c == closer:
            depth -= 1
            if depth == 0:
                return i + 1
        i += 1
    raise Bail(f"unbalanced `{opener}` at offset {start}")

def split_top_level(text: str, separator: str = ",") -> list[str]:
    """Split on `separator` outside of brackets and literals."""
    parts, depth, current = [], 0, []
    i = 0
    while i < len(text):
        c = text[i]
        if c in "\"'":
            end = skip_literal(text, i)
            current.append(text[i:end])
            i = end
            continue
        if c in "([{":
            depth += 1
        elif c in ")]}":
            depth -= 1
        if c == separator and depth == 0:
            parts.append("".join(current))
            current = []
        else:
            current.append(c)
        i += 1
    parts.append("".join(current))
    return [part.strip() for part in parts if part.strip()]

def strip_noop_macros(expression: str) -> str:
    text = expression.strip()
    for macro in NOOP_MACROS:
        pattern = re.compile(rf"\b{macro}\s*\(")
        while True:
            found = pattern.search(text)
            if not found:
                break
            end = match_braces(text, found.end() - 1, "(", ")")
            text = text[: found.start()] + text[found.end(): end - 1] + text[end:]
    return text.strip()

class Unresolved(Exception):
    pass

def parse_string_literal(expression: str) -> str | None:
    """Concatenate adjacent string literals, or None if this is not a string."""
    text = expression.strip()
    pieces, i = [], 0
    while i < len(text):
        if text[i].isspace():
            i += 1
            continue
        if text[i] == "R" and text.startswith('R"', i):
            marker = text.index("(", i)
            tag = text[i + 2: marker]
            end = text.index(f"){tag}\"", marker)
            pieces.append(text[marker + 1: end])
            i = end + len(tag) + 2
            continue
        if text[i] != '"':
            return None
        end = skip_literal(text, i)
        pieces.append(decode_escapes(text[i + 1: end - 1]))
        i = end
    return "".join(pieces) if pieces else None

def decode_escapes(text: str) -> str:
    return (text.replace("\\n", "\n").replace("\\t", "\t")
                .replace('\\"', '"').replace("\\\\", "\\"))

NUMBER = re.compile(
    r"(?<![\w.])(?:0[xX][0-9a-fA-F']+|0[bB][01']+|"
    r"(?:\d[\d']*)?\.\d[\d']*(?:[eE][+-]?\d+)?|\d[\d']*\.?(?:[eE][+-]?\d+)?)"
    r"[uUlLfFzZ]*"
)

IDENTIFIER = re.compile(r"(?<![\w.])[A-Za-z_]\w*(?:::[A-Za-z_]\w*)*")

def normalize_number(token: str) -> str:
    """Strip C++ digit separators and literal suffixes for Python parsing."""
    text = token.replace("'", "")
    text = re.sub(r"(?<=[0-9a-fA-F.])[uUlLzZ]+$", "", text)
    if not text.lower().startswith(("0x", "0b")):
        text = re.sub(r"(?<=[0-9.])[fF]$", "", text)
    if text.endswith("."):
        text += "0"
    if text.startswith("."):
        text = "0" + text
    return text

class Scope:
    """Named C++ constants, resolved by qualified name or by any suffix of one.

    Dolphin writes `Memory::MEM1_SIZE_RETAIL` and `ExpansionInterface::
    EXIDeviceType::None`, while the tables here are keyed by the innermost
    names. Matching on suffixes bridges that without modelling namespaces. A
    bare name that two different declarations disagree on is dropped rather
    than guessed at, so an ambiguous default is reported instead of silently
    taking whichever file was read first.
    """

    AMBIGUOUS = object()

    def __init__(self, values: dict[str, object] | None = None) -> None:
        self.values: dict[str, object] = dict(values or {})

    def define(self, name: str, value: object) -> None:
        existing = self.values.get(name, None)
        if existing is None:
            self.values[name] = value
        elif existing is not Scope.AMBIGUOUS and existing != value:
            self.values[name] = Scope.AMBIGUOUS

    def lookup(self, name: str) -> object | None:
        parts = name.split("::")
        for start in range(len(parts)):
            candidate = "::".join(parts[start:])
            if candidate in self.values:
                value = self.values[candidate]
                if value is Scope.AMBIGUOUS:
                    raise Unresolved(f"`{candidate}` is declared more than once with "
                                     "different values")
                return value
        return None

def evaluate_expression(expression: str, scope: Scope) -> object:
    """Evaluate a C++ constant expression, raising Unresolved when it is code."""
    text = strip_noop_macros(expression)
    if not text:
        raise Unresolved("empty expression")

    literal = parse_string_literal(text)
    if literal is not None:
        return literal
    if text in ("true", "false"):
        return text == "true"
    while text.startswith("(") and match_braces(text, 0, "(", ")") == len(text):
        text = text[1:-1].strip()

    whole = scope.lookup(text)
    if whole is not None:
        return whole

    normalized = NUMBER.sub(lambda m: normalize_number(m.group(0)), text)

    # Substitute resolvable named constants before parsing, so that qualified
    # names (which Python cannot represent) never reach the AST.
    def substitute(found: re.Match[str]) -> str:
        name = found.group(0)
        if name in ("true", "false"):
            return "True" if name == "true" else "False"
        value = scope.lookup(name)
        if isinstance(value, (int, float, bool)):
            return repr(value)
        return name

    normalized = IDENTIFIER.sub(substitute, normalized)
    if "::" in normalized:
        unknown = re.search(r"[A-Za-z_][\w:]*::[A-Za-z_]\w*", normalized)
        raise Unresolved(f"unknown constant `{unknown.group(0) if unknown else text}`")
    try:
        tree = ast.parse(normalized, mode="eval")
    except SyntaxError as error:
        raise Unresolved(f"unparsable: {expression}") from error
    return evaluate_ast(tree.body, scope, expression)

def evaluate_ast(node: ast.AST, scope: Scope, original: str) -> object:
    if isinstance(node, ast.Constant):
        if isinstance(node.value, (int, float, str, bool)):
            return node.value
        raise Unresolved(f"unsupported literal in {original}")
    if isinstance(node, ast.Name):
        value = scope.lookup(node.id)
        if value is not None:
            return value
        raise Unresolved(f"unknown identifier `{node.id}` in {original}")
    if isinstance(node, ast.UnaryOp):
        operand = evaluate_ast(node.operand, scope, original)
        if isinstance(node.op, ast.USub):
            return -operand
        if isinstance(node.op, ast.UAdd):
            return +operand
        if isinstance(node.op, ast.Invert):
            return ~operand
        raise Unresolved(f"unsupported unary operator in {original}")
    if isinstance(node, ast.BinOp):
        left = evaluate_ast(node.left, scope, original)
        right = evaluate_ast(node.right, scope, original)
        return apply_binary(node.op, left, right, original)
    raise Unresolved(f"non-constant expression: {original}")

def apply_binary(op: ast.AST, left, right, original: str):
    if isinstance(op, ast.Add):
        return left + right
    if isinstance(op, ast.Sub):
        return left - right
    if isinstance(op, ast.Mult):
        return left * right
    if isinstance(op, ast.Div):
        if isinstance(left, int) and isinstance(right, int):
            if right == 0:
                raise Unresolved(f"division by zero in {original}")
            # C++ integer division truncates toward zero; Python floors.
            quotient = abs(left) // abs(right)
            return quotient if (left >= 0) == (right >= 0) else -quotient
        return left / right
    if isinstance(op, ast.Mod):
        return left % right
    if isinstance(op, ast.LShift):
        return left << right
    if isinstance(op, ast.RShift):
        return left >> right
    if isinstance(op, ast.BitOr):
        return left | right
    if isinstance(op, ast.BitAnd):
        return left & right
    if isinstance(op, ast.BitXor):
        return left ^ right
    raise Unresolved(f"unsupported operator in {original}")

@dataclass
class EnumDef:
    name: str
    underlying: str
    members: list[tuple[str, int]]

def parse_enums(root: Path, report: Report) -> dict[str, list[EnumDef]]:
    """Collect every `enum`/`enum class` definition under Source/Core.

    Simple names collide across Dolphin (`Mode`, `State`, `EntryType`), so each
    name keeps every distinct definition; resolution refuses to guess later.
    """
    enums: dict[str, list[EnumDef]] = {}
    pattern = re.compile(r"\benum\s+(?:class\s+|struct\s+)?(\w+)\s*(?::\s*([\w:\s]+?)\s*)?\{")
    for path in sorted(root.glob("Source/Core/**/*.h")) + sorted(root.glob("Source/Core/**/*.cpp")):
        try:
            text = strip_comments(path.read_text(encoding="utf-8", errors="replace"))
        except OSError as error:
            report.warn(f"unreadable {path}: {error}")
            continue
        for found in pattern.finditer(text):
            name, underlying = found.group(1), (found.group(2) or "int").strip()
            try:
                end = match_braces(text, found.end() - 1)
            except Bail:
                continue
            body = text[found.end(): end - 1]
            members: list[tuple[str, int]] = []
            scope = Scope()
            following = 0
            broken = False
            for entry in split_top_level(body):
                entry = re.sub(r"\[\[.*?\]\]", "", entry).strip()
                if not entry:
                    continue
                member = re.match(r"^(\w+)\s*(?:=\s*(.+))?$", entry, re.S)
                if not member:
                    broken = True
                    break
                member_name = member.group(1)
                if member.group(2) is not None:
                    try:
                        value = evaluate_expression(member.group(2), scope)
                    except Unresolved:
                        broken = True
                        break
                    if not isinstance(value, int):
                        broken = True
                        break
                    following = int(value)
                members.append((member_name, following))
                scope.define(member_name, following)
                scope.define(f"{name}::{member_name}", following)
                following += 1
            if broken or not members:
                continue
            definition = EnumDef(name, underlying, members)
            existing = enums.setdefault(name, [])
            if not any(other.members == members and other.underlying == underlying
                       for other in existing):
                existing.append(definition)
    return enums

CONSTANT_DECLARATION = re.compile(
    r"(?:static\s+)?constexpr\s+(?:const\s+)?[\w:<>,\s*&]+?\b(\w+)\s*=\s*([^;{}]+);"
)

CONSTANT_DEFINE = re.compile(r"^\s*#\s*define\s+(\w+)\s+([^\\\n]+)$", re.M)

def collect_constants(text: str) -> list[tuple[str, str]]:
    return [(found.group(1), found.group(2).strip())
            for found in list(CONSTANT_DECLARATION.finditer(text))
            + list(CONSTANT_DEFINE.finditer(text))]

def resolve_constants(declarations: list[tuple[str, str]], scope: Scope) -> Scope:
    """Evaluate declarations to a fixed point, since they reference each other."""
    pending = list(declarations)
    while pending:
        remaining = []
        for name, expression in pending:
            try:
                value = evaluate_expression(expression, scope)
            except (Unresolved, Bail):
                remaining.append((name, expression))
                continue
            if isinstance(value, (int, float, str, bool)):
                scope.define(name, value)
        if len(remaining) == len(pending):
            break
        pending = remaining
    return scope

def slugify(text: str) -> str:
    slug = re.sub(r"[^0-9a-zA-Z]+", "-", text).strip("-").lower()
    return slug or "root"

def snake_case(text: str) -> str:
    if text in KEY_OVERRIDES:
        return KEY_OVERRIDES[text]
    spaced = re.sub(r"(.)([A-Z][a-z]+)", r"\1_\2", text)
    spaced = re.sub(r"([a-z0-9])([A-Z])", r"\1_\2", spaced)
    spaced = re.sub(r"[^0-9a-zA-Z]+", "_", spaced)
    return re.sub(r"_+", "_", spaced).strip("_").lower() or "option"

def humanize(text: str) -> str:
    # An all-caps name is already an initialism or an upstream constant spelling
    # (SMPTE_NTSCM); splitting and re-capitalizing it only makes it wrong.
    if text.isupper():
        return text
    words = snake_case(text).split("_")
    return " ".join(word.upper() if word in ACRONYMS else word.capitalize() for word in words)

def integer_repr(underlying: str) -> tuple[str, int | None]:
    return INT_TYPES.get(underlying.split("::")[-1], ("dec", None))

def format_integer(value: int, radix: str, width: int | None) -> str:
    if radix == "hex":
        return f"0x{value & ((1 << (4 * (width or 8))) - 1):0{width or 8}x}"
    return str(value)

def base_type(ctype: str) -> str:
    return ctype.replace("const", "").strip()

def resolve_enum(ctype: str, enums: dict[str, list[EnumDef]],
                 report: Report, origin: str) -> EnumDef | None:
    simple = base_type(ctype).split("::")[-1]
    candidates = enums.get(simple, [])
    if not candidates:
        return None
    if len(candidates) > 1:
        report.warn(f"{origin}: `{ctype}` matches {len(candidates)} different enums; "
                    "qualify it in the script to resolve")
        return None
    return candidates[0]

def enum_scope(enums: dict[str, list[EnumDef]], base: Scope) -> Scope:
    scope = Scope(base.values)
    for candidates in enums.values():
        for enum in candidates:
            for name, value in enum.members:
                scope.define(f"{enum.name}::{name}", value)
                scope.define(name, value)
    return scope

def enum_value(enums: dict[str, list[EnumDef]], enum_name: str, member: str,
               report: Report) -> str:
    """Render one enum member the way Dolphin writes it, from the parsed enum."""
    candidates = enums.get(enum_name, [])
    if len(candidates) != 1:
        raise Bail(f"`{enum_name}` matches {len(candidates)} enum definitions")
    enum = candidates[0]
    for name, value in enum.members:
        if name == member:
            radix, width = integer_repr(enum.underlying)
            return format_integer(value, radix, width)
    raise Bail(f"`{enum_name}` has no member `{member}`")

def toml_string(value: str) -> str:
    escaped = (value.replace("\\", "\\\\").replace('"', '\\"')
                    .replace("\n", "\\n").replace("\t", "\\t").replace("\r", "\\r"))
    return f'"{escaped}"'

def toml_value(value) -> str:
    if isinstance(value, bool):
        return "true" if value else "false"
    if isinstance(value, str):
        return toml_string(value)
    if isinstance(value, int):
        return str(value)
    if isinstance(value, float):
        text = repr(value)
        return text if ("." in text or "e" in text or "E" in text) else text + ".0"
    if isinstance(value, dict):
        inner = ", ".join(f"{toml_key(k)} = {toml_value(v)}" for k, v in value.items())
        return "{ " + inner + " }" if inner else "{}"
    if isinstance(value, (list, tuple)):
        return "[" + ", ".join(toml_value(entry) for entry in value) + "]"
    if value is None:
        raise Bail("cannot serialize None to TOML")
    raise Bail(f"cannot serialize {type(value).__name__} to TOML")

def toml_key(key: str) -> str:
    return key if re.fullmatch(r"[A-Za-z0-9_-]+", key) else toml_string(key)

def target_header(*parts: str) -> str:
    """`[targets.gfx.Hardware]` — nesting is the parent relationship."""
    for part in parts:
        if "." in part:
            raise Bail(f"target component `{part}` contains a dot, which would be "
                       "read as nesting; give it an explicit id and `node`")
    return "[targets." + ".".join(toml_key(part) for part in parts) + "]"

def target_id(*parts: str) -> str:
    return ".".join(parts)
