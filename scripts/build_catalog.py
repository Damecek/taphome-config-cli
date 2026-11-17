#!/usr/bin/env python3
"""Build a protobuf type catalog from ILSpy-decompiled TapHome C# sources.

Usage:
    build_catalog.py SRC_DIR OUT_JSON [--variant NAME]
    build_catalog.py --diff OLD_JSON NEW_JSON OUT_MD
    build_catalog.py --self-test

SRC_DIR holds one directory per assembly (SRC_DIR/<Assembly>/<Namespace>/<Class>.cs).
Both attribute flavours are understood: protobuf-net (`using ProtoBuf;`) and the
source-generator based GProtobuf.Core (`using GProtobuf;`), which uses the same
attribute names. The generated serializers (official: the precompiled
`*.Serializer` assembly, next: the `*.Serialization` folders) are parsed separately
and cross-checked against the tags declared by attributes.

Standard library only. Source files are treated as data and never executed.
"""
import argparse
import ast
import json
import re
import sys
from collections import Counter, defaultdict
from pathlib import Path

PROTO_ATTRS = ("ProtoContract", "ProtoMember", "ProtoInclude", "ProtoVarint", "ProtoBuffer")
MODIFIERS = {
    "public", "private", "protected", "internal", "static", "readonly", "virtual", "override",
    "abstract", "new", "sealed", "unsafe", "volatile", "required", "const", "partial", "extern",
    "async", "file",
}
REPEATED_TYPES = {
    "List", "IList", "IEnumerable", "ICollection", "IReadOnlyList", "IReadOnlyCollection",
    "HashSet", "ISet", "Collection", "ReadOnlyCollection", "ObservableCollection", "Queue",
    "Stack", "LinkedList", "ImmutableArray", "ImmutableList",
}
MAP_TYPES = {
    "Dictionary", "IDictionary", "IReadOnlyDictionary", "SortedDictionary",
    "ConcurrentDictionary", "ImmutableDictionary",
}
INT_LIMITS = {  # type -> (min, max)
    "byte": (0, 255), "sbyte": (-128, 127), "short": (-32768, 32767), "ushort": (0, 65535),
    "int": (-2**31, 2**31 - 1), "uint": (0, 2**32 - 1), "long": (-2**63, 2**63 - 1), "ulong": (0, 2**64 - 1),
}
ENUM_UNDERLYING = {"byte", "sbyte", "short", "ushort", "int", "uint", "long", "ulong"}
BUILTIN_TYPES = {
    "bool", "byte", "sbyte", "short", "ushort", "int", "uint", "long", "ulong", "float",
    "double", "decimal", "string", "char", "object", "Guid", "DateTime", "DateTimeOffset",
    "TimeSpan", "Uri", "Version",
}

# ---------------------------------------------------------------- lexical helpers

_NOISE = re.compile(
    r'//[^\n]*|/\*.*?\*/'
    r'|(?:\$@|@\$|@)"(?:[^"]|"")*"'
    r'|\$?"(?:\\.|[^"\\\n])*"'
    r"|'(?:\\.[^']*|[^'\\])'",
    re.S,
)


def blank_noise(text):
    """Replace comments, strings and char literals by spaces. Offsets stay identical."""
    return _NOISE.sub(lambda m: re.sub(r"[^\n]", " ", m.group()), text)


def split_top(text, sep=",", angles=True):
    """Split on `sep` outside (), [], {}, quotes and (if `angles`) <>."""
    parts, depth, cur, quote = [], 0, [], None
    for i, ch in enumerate(text):
        if quote:
            cur.append(ch)
            if ch == quote and text[i - 1] != "\\":
                quote = None
            continue
        if ch in "\"'":
            quote = ch
        elif ch in "([{" or (angles and ch == "<"):
            depth += 1
        elif ch in ")]}" or (angles and ch == ">" and text[i - 1] != "="):
            depth -= 1
        elif ch == sep and depth == 0:
            parts.append("".join(cur).strip())
            cur = []
            continue
        cur.append(ch)
    tail = "".join(cur).strip()
    if tail or parts:
        parts.append(tail)
    return parts


def match_close(clean, open_idx):
    """Index just after the brace that closes the one at open_idx."""
    depth = 0
    for m in re.finditer(r"[{}]", clean[open_idx:]):
        depth += 1 if m.group() == "{" else -1
        if depth == 0:
            return open_idx + m.end()
    return len(clean)


def parse_literal(raw):
    raw = raw.strip()
    if raw in ("true", "false"):
        return raw == "true"
    try:
        return ast.literal_eval(re.sub(r"(?<=[\d.])[fFdDmM]$", "", raw))
    except (ValueError, SyntaxError):
        return raw


def tidy(text):
    return re.sub(r"\s+", " ", text).strip()


# ---------------------------------------------------------------- attributes

def extract_attrs(orig, clean):
    """Read leading `[A(..), B]` lists. Returns (attributes, index where the declaration starts)."""
    attrs, i = [], 0
    while True:
        m = re.compile(r"\s*\[").match(clean, i)
        if not m:
            return attrs, i
        start, depth, j = m.end(), 1, m.end()
        while j < len(clean) and depth:
            depth += {"[": 1, "]": -1}.get(clean[j], 0)
            j += 1
        for item in split_top(orig[start:j - 1], angles=False):
            attr = parse_attr(item)
            if attr:
                attrs.append(attr)
        i = j


def parse_attr(item):
    m = re.match(r"(?:\w+\s*:\s*)?([\w.]+)\s*(?:\((.*)\))?\s*$", item.strip(), re.S)
    if not m:
        return None
    name = m.group(1).split(".")[-1]
    name = name[:-9] if name.endswith("Attribute") and len(name) > 9 else name
    positional, named = [], {}
    for arg in split_top(m.group(2) or "", angles=False):
        nm = re.match(r"(\w+)\s*=(?!=)\s*(.*)$", arg, re.S)
        if nm:
            named[nm.group(1)] = parse_literal(nm.group(2))
        elif arg:
            positional.append(arg)
    return {"name": name, "positional": positional, "named": named}


def find_attr(attrs, name):
    return next((a for a in attrs if a["name"] == name), None)


def int_arg(attr):
    return int(attr["positional"][0]) if attr and attr["positional"] else None


def other_attrs(attrs):
    """Attributes worth keeping for reference (Json*, ProtoMember, ProtoInclude are handled elsewhere)."""
    skip = {"ProtoMember", "ProtoInclude", "ProtoContract", "DefaultValue"}
    return [
        {"name": a["name"], "args": a["positional"] + [f"{k}={v}" for k, v in a["named"].items()]}
        for a in attrs
        if a["name"] not in skip and not a["name"].startswith("Json")
    ]


# ---------------------------------------------------------------- type model

class TypeInfo:
    def __init__(self, name, kind, namespace, assembly, rel, outer, attrs):
        self.name, self.kind, self.namespace, self.assembly = name, kind, namespace, assembly
        self.file, self.outer, self.attrs = rel, outer, attrs
        self.bases, self.abstract, self.generic = [], False, []
        self.members, self.buffer_members, self.values = [], [], []
        self.consts, self.overrides = {}, {}
        self.usings = []
        self.underlying = "int"

    @property
    def full_name(self):
        parts = [self.namespace] if self.namespace else []
        if self.outer:
            parts.append(self.outer)
        return ".".join(parts + [self.name])


_TYPE_DECL = re.compile(
    r"^(?P<mods>(?:\w+\s+)*?)(?P<kw>class|struct|interface|enum|record\s+struct|record\s+class|record)\s+"
    r"(?P<name>\w+)\s*(?P<gen><.*?>)?\s*(?:\([^{]*\))?\s*(?::\s*(?P<bases>.*?))?\s*(?:\bwhere\b.*)?$",
    re.S,
)


def parse_type_header(decl):
    m = _TYPE_DECL.match(decl.strip())
    if not m or not set(m.group("mods").split()) <= MODIFIERS:
        return None
    bases = [tidy(b) for b in split_top(m.group("bases") or "") if b]
    kind = "class" if m.group("kw").startswith("record") else m.group("kw")
    gen = [g.strip() for g in split_top((m.group("gen") or "")[1:-1])] if m.group("gen") else []
    return {
        "name": m.group("name"), "kind": kind, "bases": bases, "generic": gen,
        "mods": set(m.group("mods").split()),
    }


def parse_member_decl(decl):
    """`public override int Foo => 3` -> dict(name, type, expr, init, mods) or None."""
    decl = decl.strip()
    expr = init = None
    if "=>" in decl:
        decl, expr = decl.split("=>", 1)
    else:
        m = re.search(r"(?<![=!<>])=(?!=)", decl)
        if m:
            decl, init = decl[:m.start()], decl[m.end():]
    if "(" in decl:  # method, ctor, indexer or operator
        return None
    toks = decl.split()
    mods = set()
    while toks and toks[0] in MODIFIERS:
        mods.add(toks.pop(0))
    text = " ".join(toks)
    m = re.match(r"^(?P<type>.+?)\s+(?P<name>[\w@.]+)$", text, re.S)
    if not m:
        return None
    return {
        "type": tidy(m.group("type")), "name": m.group("name").split(".")[-1].lstrip("@"),
        "expr": tidy(expr) if expr else None, "init": tidy(init) if init else None, "mods": mods,
    }


def parse_enum_body(body, enum_attrs=None):
    """Return [(name, value)] with implicit and simple constant-expression values resolved."""
    values, known, nxt = [], {}, 0
    for item in split_top(body, angles=False):
        item = re.sub(r"\[[^\]]*\]", "", item).strip()
        if not item:
            continue
        name, _, expr = (p.strip() for p in item.partition("="))
        if expr:
            nxt = eval_const(expr, known)
        values.append({"name": name, "value": nxt})
        known[name] = nxt
        nxt = None if nxt is None else nxt + 1
    return values


_SAFE_NODES = (ast.Expression, ast.BinOp, ast.UnaryOp, ast.Constant, ast.operator, ast.unaryop)


def eval_const(expr, known):
    """Evaluate an integer constant expression; names resolve through `known`. None if impossible."""
    expr = re.sub(r"\(\s*(?:byte|sbyte|short|ushort|int|uint|long|ulong)\s*\)", "", expr)
    expr = re.sub(r"\b(0x[0-9A-Fa-f]+|\d+)[uUlL]+\b", r"\1", expr)
    expr = re.sub(r"\b(byte|sbyte|short|ushort|int|uint|long|ulong)\.(MaxValue|MinValue)\b",
                  lambda m: str(INT_LIMITS[m.group(1)][m.group(2) == "MaxValue"]), expr)
    expr = re.sub(r"(?<![\w.])[A-Za-z_][\w.]*", lambda m: str(known.get(m.group().split(".")[-1], "?")), expr)
    try:
        tree = ast.parse(expr.strip(), mode="eval")
        if not all(isinstance(n, _SAFE_NODES) for n in ast.walk(tree)):
            return None
        value = eval(compile(tree, "<const>", "eval"), {"__builtins__": {}})  # noqa: S307 (whitelisted AST)
        return value if isinstance(value, int) else None
    except (SyntaxError, TypeError, ValueError):
        return None


# ---------------------------------------------------------------- file parser

class FileParser:
    def __init__(self, orig, rel, assembly):
        self.orig, self.clean = orig, blank_noise(orig)
        self.rel, self.assembly = rel, assembly
        self.types, self.usings, self.warnings = [], [], []
        self.attached = Counter()  # Proto attribute name -> number attached to something we kept
        self.scopes = []  # ("ns", name) | ("type", TypeInfo) | ("other", None)
        self.namespace = ""

    def run(self):
        clean, pos = self.clean, 0
        while True:
            m = re.compile(r"[{};]").search(clean, pos)
            if not m:
                break
            header_span = (pos, m.start())
            pos = m.end()
            ch = m.group()
            if ch == "}":
                if self.scopes and self.scopes.pop()[0] == "ns":
                    self.namespace = ".".join(s[1] for s in self.scopes if s[0] == "ns")
            elif ch == ";":
                self.statement(header_span)
            else:
                pos = self.block(header_span, m.start())
        self.warn_unattached()
        return self

    def warn_unattached(self):
        raw = Counter(re.findall(r"\b(%s)\b(?=\s*[(\],])" % "|".join(PROTO_ATTRS), self.clean))
        for name, n in raw.items():
            if self.attached[name] != n:
                self.warnings.append(f"{self.rel}: {name} seen {n}x, attached {self.attached[name]}x")

    # -- helpers
    def current_type(self):
        return self.scopes[-1][1] if self.scopes and self.scopes[-1][0] == "type" else None

    def outer_name(self):
        names = [s[1].name for s in self.scopes if s[0] == "type"]
        return ".".join(names) or None

    def header(self, span):
        return self.orig[span[0]:span[1]], self.clean[span[0]:span[1]]

    def count(self, attrs):
        for a in attrs:
            if a["name"] in PROTO_ATTRS:
                self.attached[a["name"]] += 1

    # -- `;` terminated headers: usings, file-scoped namespace, fields, abstract props
    def statement(self, span):
        orig, clean = self.header(span)
        text = clean.strip()
        if text.startswith("using ") and "=" not in text and "(" not in text:
            self.usings.append(text[6:].strip())
        elif text.startswith("namespace "):
            self.namespace = text.split(None, 1)[1].strip()
        elif self.current_type():
            attrs, start = extract_attrs(orig, clean)
            self.member(self.current_type(), attrs, orig[start:], clean[start:], None)

    # -- `{` headers: namespaces, types, members with bodies. Returns the next scan position.
    def block(self, span, brace_idx):
        orig, clean = self.header(span)
        attrs, start = extract_attrs(orig, clean)
        decl, cdecl = orig[start:], clean[start:]
        stripped = cdecl.strip()
        if stripped.startswith("namespace "):
            name = stripped.split(None, 1)[1].strip()
            self.scopes.append(("ns", name))
            self.namespace = ".".join(s[1] for s in self.scopes if s[0] == "ns")
            return brace_idx + 1
        info = parse_type_header(cdecl)
        if info:
            return self.open_type(info, attrs, brace_idx)
        end = match_close(self.clean, brace_idx)
        cur = self.current_type()
        if cur:
            body = self.clean[brace_idx:end]
            self.member(cur, attrs, decl, cdecl, body)
        return end

    def open_type(self, info, attrs, brace_idx):
        self.count(attrs)
        t = TypeInfo(info["name"], info["kind"], self.namespace, self.assembly, self.rel,
                     self.outer_name(), attrs)
        t.abstract, t.generic = "abstract" in info["mods"], info["generic"]
        t.usings = self.usings
        t.sealed_static = {m for m in info["mods"] if m in ("sealed", "static", "readonly")}
        if t.kind == "enum":
            if info["bases"]:
                t.underlying = info["bases"][0]
            end = match_close(self.clean, brace_idx)
            t.values = parse_enum_body(self.orig[brace_idx + 1:end - 1])
            self.types.append(t)
            return end
        t.bases = info["bases"]
        self.types.append(t)
        self.scopes.append(("type", t))
        return brace_idx + 1

    def member(self, t, attrs, decl, cdecl, body):
        pm = find_attr(attrs, "ProtoMember")
        buf = find_attr(attrs, "ProtoBuffer")
        parsed = parse_member_decl(cdecl if body is None else cdecl)
        if buf:
            m = re.search(r"([\w]+)\s*\(", cdecl)
            t.buffer_members.append({
                "tag": int_arg(buf), "method": m.group(1) if m else None,
                "operation": (buf["positional"][1].split(".")[-1] if len(buf["positional"]) > 1 else None),
            })
            self.count([buf])
            return
        if not parsed:
            if pm:
                self.warnings.append(f"{self.rel}: unparsed ProtoMember declaration: {tidy(decl)[:80]}")
            return
        name = parsed["name"]
        if "const" in parsed["mods"] and parsed["init"] is not None:
            t.consts[name] = parsed["init"]
        if name in ("MessageType", "MinProtocolVersion") and "override" in parsed["mods"]:
            expr = parsed["expr"]
            if expr is None and body:
                r = re.search(r"return\s+([^;]+);", body)
                expr = tidy(r.group(1)) if r else None
            t.overrides[name] = expr
        if not pm or find_attr(attrs, "ProtoIgnore"):
            return
        self.count([pm])
        dv = find_attr(attrs, "DefaultValue")
        t.members.append({
            "tag": int_arg(pm), "name": name, "type": parsed["type"],
            "declared_as": "property" if body is not None else "field",
            "options": pm["named"],
            "default": parse_literal(dv["positional"][0]) if dv and dv["positional"] else None,
            "has_default": dv is not None,
            "attributes": other_attrs(attrs),
        })


# ---------------------------------------------------------------- type shape analysis

def split_generic(t):
    m = re.match(r"^(?:[\w.]+\.)?(\w+)\s*<(.*)>$", t.strip(), re.S)
    return (m.group(1), [tidy(a) for a in split_top(m.group(2))]) if m else (None, [])


def analyze_type(t):
    """Shape of a member type: nullable / repeated / map plus the leaf type names."""
    t = tidy(t)
    shape = {"nullable": False, "repeated": False, "element_type": None, "map": None}
    outer, args = split_generic(t)
    if outer == "Nullable" and len(args) == 1:
        shape["nullable"], t = True, args[0]
        outer, args = split_generic(t)
    elif t.endswith("?"):
        shape["nullable"], t = True, t[:-1].strip()
        outer, args = split_generic(t)
    if outer in MAP_TYPES and len(args) == 2:
        shape["map"] = {"key": args[0], "value": args[1]}
        leaves = args
    elif outer in REPEATED_TYPES and len(args) == 1:
        shape["repeated"], shape["element_type"], leaves = True, args[0], args
    elif t.endswith("[]") and t != "byte[]":
        shape["repeated"], shape["element_type"] = True, t[:-2]
        leaves = [t[:-2].rstrip("[]")]
    else:
        leaves = [t]
    shape["leaves"] = [name for leaf in leaves for name in leaf_names(leaf)]
    return shape


def leaf_names(t):
    """Non-generic type names inside `t`, looking through generic arguments, arrays and `?`."""
    t = re.sub(r"\?$", "", re.sub(r"(\[\])+$", "", tidy(t)))
    outer, args = split_generic(t)
    return [n for a in args for n in leaf_names(a)] if outer else [t]


# ---------------------------------------------------------------- catalog builder

class Catalog:
    def __init__(self, src, variant):
        self.src, self.variant = Path(src), variant
        self.types, self.warnings = [], []
        self.coverage = {"files_with_proto_attrs": 0, "attrs_seen": Counter(), "unparsed_files": []}
        self.skipped_dirs = []

    # -- step 1: parse every non-generated source file
    def parse_sources(self):
        for path in sorted(self.src.rglob("*.cs")):
            rel = path.relative_to(self.src)
            assembly = rel.parts[0]
            if assembly.endswith(".Serializer") or any(p.endswith(".Serialization") for p in rel.parts):
                continue
            fp = FileParser(path.read_text(encoding="utf-8-sig", errors="replace"), str(rel), assembly).run()
            seen = Counter(re.findall(r"\b(%s)\b(?=\s*[(\],])" % "|".join(PROTO_ATTRS), fp.clean))
            if seen:
                self.coverage["files_with_proto_attrs"] += 1
                self.coverage["attrs_seen"].update(seen)
            if fp.warnings:
                self.coverage["unparsed_files"].extend(fp.warnings)
            for t in fp.types:
                t.lib = ("GProtobuf" if any(u.startswith("GProtobuf") for u in fp.usings)
                         else "protobuf-net" if any(u.startswith("ProtoBuf") for u in fp.usings) else None)
            self.types.extend(fp.types)

    # -- step 2: name resolution
    def index(self):
        self.by_name = defaultdict(list)
        for t in self.types:
            self.by_name[t.name].append(t)
        self.by_full = {}
        for t in self.types:
            self.by_full.setdefault(t.full_name, t)

    def resolve(self, name, ctx):
        """Find a parsed type by (possibly dotted) name as seen from type `ctx`."""
        name = re.sub(r"<.*>$", "", name.strip()).replace("global::", "")
        simple = name.split(".")[-1]
        cands = self.by_name.get(simple, [])
        if len(cands) > 1 and "." in name:
            narrowed = [c for c in cands if c.full_name.endswith(name)]
            cands = narrowed or cands
        if len(cands) > 1:  # a type nested in ctx or one of its bases wins
            chain, cur = set(), ctx
            while cur and len(chain) < 50:
                chain.add(cur.name)
                cur = cur.base_type
            cands = [c for c in cands if c.outer and c.outer.split(".")[0] in chain] or cands
        if len(cands) > 1 and "." not in name:
            cands = [c for c in cands if not c.outer] or cands
        if len(cands) > 1:
            visible = [c for c in cands if c.namespace == ctx.namespace or c.namespace in ctx.usings]
            cands = visible or cands
        if len(cands) > 1:
            cands = [c for c in cands if c.assembly == ctx.assembly] or cands
        return cands[0] if cands else None

    def link_bases(self):
        for t in self.types:
            t.base_type = None
            t.base, t.interfaces = None, []
            for i, b in enumerate(t.bases):
                if t.kind in ("struct", "interface"):
                    t.interfaces.append(b)
                    continue
                target = self.resolve(b, t)
                if target and target.kind == "class":
                    t.base, t.base_type = target.name, target
                elif target is None and not re.match(r"^I[A-Z]", b) and i == 0:
                    t.base = b
                else:
                    t.interfaces.append(b)

    def is_message(self, t):
        seen = 0
        while t and seen < 50:
            if t.name == "MessageBase":
                return True
            t, seen = t.base_type, seen + 1
        return False

    # -- step 3: decide what is serializable
    def select(self):
        contract = {id(t) for t in self.types
                    if t.kind != "enum" and (find_attr(t.attrs, "ProtoContract") or t.members
                                             or t.buffer_members or find_attr(t.attrs, "ProtoVarint")
                                             or find_attr(t.attrs, "ProtoInclude"))}
        enums = {id(t) for t in self.types if t.kind == "enum" and find_attr(t.attrs, "ProtoContract")}
        self.ambiguous_enums = []
        self.unresolved = Counter()
        for t in self.types:
            if id(t) not in contract:
                continue
            for m in t.members:
                shape = analyze_type(m["type"])
                m["shape"] = shape
                for leaf in shape["leaves"]:
                    target = self.resolve(leaf, t)
                    if target is None:
                        if leaf not in BUILTIN_TYPES and leaf not in ENUM_UNDERLYING:
                            self.unresolved[leaf] += 1
                    elif target.kind == "enum":
                        enums.add(id(target))
                        if len(self.by_name[target.name]) > 1:
                            self.ambiguous_enums.append(f"{t.full_name}.{m['name']} -> {target.full_name}")
        # the MessageType enum is part of the protocol even though no member references it
        for t in self.types:
            if t.kind == "enum" and t.full_name.endswith("Communication.Messages.MessageType"):
                enums.add(id(t))
        self.selected = [t for t in self.types if id(t) in contract or id(t) in enums]

    # -- step 4: output records
    def member_record(self, t, m):
        shape = m["shape"]
        ref = None
        primary = (shape["map"] or {}).get("value") or shape["element_type"] or m["type"]
        for leaf in leaf_names(primary)[:1]:
            target = self.resolve(leaf, t)
            ref = target.full_name if target and target in self.selected else None
        rec = {
            "tag": m["tag"], "name": m["name"], "type": m["type"], "nullable": shape["nullable"],
            "repeated": shape["repeated"], "element_type": shape["element_type"], "map": shape["map"],
            "options": m["options"], "default": m["default"] if m["has_default"] else None,
            "type_ref": ref, "declared_as": m["declared_as"], "source": "attributes",
        }
        if m["attributes"]:
            rec["attributes"] = m["attributes"]
        return rec

    def type_record(self, t):
        contract = find_attr(t.attrs, "ProtoContract")
        rec = {
            "name": t.name, "namespace": t.namespace, "full_name": t.full_name,
            "assembly": t.assembly, "kind": t.kind, "base": t.base, "abstract": t.abstract,
            "file": t.file, "nested_in": t.outer, "generic_params": t.generic,
            "interfaces": t.interfaces, "proto_lib": t.lib,
            "contract": contract["named"] if contract else None,
            "attributes": other_attrs(t.attrs),
        }
        if t.kind == "enum":
            rec.update(underlying=t.underlying, flags=find_attr(t.attrs, "Flags") is not None,
                       values=t.values)
            return rec
        includes = []
        for a in t.attrs:
            if a["name"] == "ProtoInclude" and len(a["positional"]) >= 2:
                tm = re.match(r"typeof\((.*)\)$", a["positional"][1].strip())
                includes.append({"tag": int(a["positional"][0]), "type": tm.group(1) if tm else a["positional"][1]})
        rec["includes"] = includes
        base_outer, base_args = split_generic(t.bases[0]) if t.bases else (None, [])
        if (base_outer in MAP_TYPES | REPEATED_TYPES) and not t.members:
            # protobuf-net/GProtobuf serialize a collection-derived contract as repeated field 1
            rec["implicit_collection"] = {"tag": 1, "base_type": t.bases[0], **{
                k: v for k, v in analyze_type(t.bases[0]).items() if k in ("repeated", "element_type", "map")}}
        rec["members"] = sorted((self.member_record(t, m) for m in t.members), key=lambda r: r["tag"])
        rec["buffer_members"] = t.buffer_members
        if self.is_message(t):
            rec["message"] = {
                "message_type": (t.overrides.get("MessageType") or "").replace("MessageType.", "") or None,
                "min_protocol_version_expr": t.overrides.get("MinProtocolVersion"),
                "min_protocol_version": None,
            }
        return rec

    def build(self):
        self.parse_sources()
        self.index()
        self.link_bases()
        self.select()
        records = sorted((self.type_record(t) for t in self.selected), key=lambda r: r["full_name"])
        self.records = {r["full_name"]: r for r in records}
        self.resolve_protocol_versions()
        self.sanity_warnings()
        return records

    def constants(self, class_name):
        out = {}
        for t in self.types:
            if t.name == class_name:
                for k, expr in t.consts.items():
                    out[k] = eval_const(expr, out)
        return out

    def resolve_protocol_versions(self):
        self.versions = self.constants("ProtocolVersions")
        self.versions_util = self.constants("ProtocolVersionsUtil")
        known = {**self.versions_util, **self.versions}
        for r in self.records.values():
            msg = r.get("message")
            if msg and msg["min_protocol_version_expr"] is not None:
                msg["min_protocol_version"] = eval_const(msg["min_protocol_version_expr"], known)

    def sanity_warnings(self):
        for r in self.records.values():
            tags = Counter(m["tag"] for m in r.get("members", []))
            dup = [k for k, v in tags.items() if v > 1]
            if dup:
                self.warnings.append(f"{r['full_name']}: duplicate tags {dup}")
            if r["kind"] == "enum":
                bad = [v["name"] for v in r["values"] if v["value"] is None]
                if bad:
                    self.warnings.append(f"{r['full_name']}: unresolved enum values {bad}")

    # -- messages section
    def messages(self):
        include_tag = {}
        for r in self.records.values():
            for inc in r.get("includes", []):
                target = next((x for x in self.by_name.get(inc["type"].split(".")[-1], [])), None)
                if target:
                    include_tag.setdefault(target.full_name, (r["name"], inc["tag"]))
        msgs = {r["name"]: r for r in self.records.values() if "message" in r}
        def inherited(name, key):  # first non-empty override up the base chain
            seen = 0
            while name in msgs and seen < 50:
                value = msgs[name]["message"][key]
                if value is not None:
                    return value, seen > 0
                name, seen = msgs[name]["base"], seen + 1
            return None, False

        out = []
        for name, r in sorted(msgs.items()):
            stem, role = name, None
            for suffix in ("Request", "Response"):
                if name.endswith(suffix):
                    stem, role = name[: -len(suffix)], suffix.lower()
            other = {"request": "Response", "response": "Request"}.get(role)
            pair = stem + other if other and stem + other in msgs else None
            parent, tag = include_tag.get(r["full_name"], (None, None))
            out.append({
                "name": name, "full_name": r["full_name"], "abstract": r["abstract"],
                "tag": tag, "included_by": parent, "base": r["base"],
                "message_type": inherited(name, "message_type")[0],
                "min_protocol_version": inherited(name, "min_protocol_version")[0],
                "min_protocol_version_expr": r["message"]["min_protocol_version_expr"],
                "min_protocol_version_inherited": inherited(name, "min_protocol_version")[1],
                "role": role, "pair": pair,
            })
        return out

    def document(self, crosscheck):
        records = list(self.records.values())
        enum_records = [r for r in records if r["kind"] == "enum"]
        msg_enum = next((r for r in enum_records if r["full_name"].endswith("Communication.Messages.MessageType")), None)
        member_count = sum(len(r.get("members", [])) for r in records)
        stats = {
            "types": len(records),
            "classes": sum(r["kind"] == "class" for r in records),
            "structs": sum(r["kind"] == "struct" for r in records),
            "enums": len(enum_records),
            "members": member_count,
            "includes": sum(len(r.get("includes", [])) for r in records),
            "messages": len(self.messages()),
            "proto_lib": dict(Counter(r["proto_lib"] for r in records)),
        }
        cov = self.coverage
        return {
            "variant": self.variant,
            "generator": "scripts/build_catalog.py",
            "stats": stats,
            "coverage": {
                "files_with_proto_attributes": cov["files_with_proto_attrs"],
                "proto_attributes_seen": dict(cov["attrs_seen"]),
                "unparsed": cov["unparsed_files"],
            },
            "serializer_crosscheck": crosscheck,
            "message_type_enum": msg_enum,
            "protocol_versions": self.versions,
            "protocol_versions_util": self.versions_util,
            "messages": self.messages(),
            "types": records,
            "unresolved_member_types": dict(self.unresolved.most_common()),
            "ambiguous_enum_references": self.ambiguous_enums,
            "warnings": self.warnings,
        }


# ---------------------------------------------------------------- serializer cross-check

def method_regions(text, pattern):
    """Yield (match, region_text) for each method whose signature matches `pattern` (line anchored)."""
    heads = [m for m in re.finditer(r"^\t(?:public|private|protected|internal)[^\n]*$", text, re.M)]
    wanted = re.compile(pattern)
    for i, h in enumerate(heads):
        m = wanted.match(h.group())
        if m:
            end = heads[i + 1].start() if i + 1 < len(heads) else len(text)
            yield m, text[h.start():end]


def shallow_tags(body, var):
    """Field numbers a reader loop dispatches on: `case N:` / `if (var == N)` at the shallowest indent."""
    hits = [(len(x.group(1)), int(x.group(2) or x.group(3)))
            for x in re.finditer(r"^(\t+)(?:case (\d+):|(?:else )?if \(%s == (\d+)\))" % var, body, re.M)]
    shallow = min((h[0] for h in hits), default=0)
    return {tag for indent, tag in hits if indent == shallow}


def official_serializer(src):
    """Per type: tags handled by the precompiled protobuf-net serializer, wire types of fields."""
    out = {}
    for path in Path(src).glob("*.Serializer/**/*.cs"):
        text = path.read_text(encoding="utf-8-sig", errors="replace")
        for m, body in method_regions(text, r"\tprivate static \S+ Read\((\S+) P_0, ProtoReader P_1\)"):
            out.setdefault(m.group(1).split(".")[-1], {})["tags"] = shallow_tags(body, "num")
        for m, body in method_regions(text, r"\tprivate static void Write\((\S+) P_0, ProtoWriter P_1\)"):
            wires = {}
            for tag, wire in re.findall(r"WriteFieldHeader\((\d+), WireType\.(\w+)", body):
                wires.setdefault(int(tag), wire)
            out.setdefault(m.group(1).split(".")[-1], {})["wires"] = wires
    return out


def generated_serializers(src):
    """GProtobuf.Core source generator output: own-field tags and subtype dispatch per type."""
    tags, subtypes = {}, {}
    for path in Path(src).glob("*/*.Serialization/SpanReaders.cs"):
        text = path.read_text(encoding="utf-8-sig", errors="replace")
        for m, body in method_regions(text, r"\tpublic static void Populate\w+\(ref SpanReader reader, (?:ref )?(\S+) \w+\)"):
            tags[m.group(1).split(".")[-1]] = shallow_tags(body, "fieldId")
        for m, body in method_regions(text, r"\tpublic static (\S+) Read\w+\(ref SpanReader reader\)$"):
            pairs = re.findall(r"(?:case (\d+):|fieldId == (\d+)\))\s*\{?[^}]*?Read(\w+)Content\(", body)
            real = {int(a or b): c for a, b, c in pairs if c != m.group(1).split(".")[-1]}
            if real:
                subtypes[m.group(1).split(".")[-1]] = real
    return tags, subtypes


def crosscheck(catalog, src):
    """Compare attribute-declared tags with what the generated serializers actually handle."""
    by_simple = defaultdict(list)
    for r in catalog.records.values():
        if r["kind"] != "enum":
            by_simple[r["name"]].append(r)
    declared = lambda r: ({m["tag"] for m in r["members"] if m["source"] == "attributes"}
                          | {b["tag"] for b in r["buffer_members"]}
                          | ({1} if r.get("implicit_collection") else set()))
    enum_names = {r["name"] for r in catalog.records.values() if r["kind"] == "enum"}
    report = {"mode": None, "enum_serializers": [], "serializer_types": 0, "matching_types": 0, "mismatches": [],
              "serializer_only_types": [], "attribute_only_types": [], "dispatcher_only_types": [], "subtype_mismatches": []}
    official = official_serializer(src)
    gen_tags, gen_sub = generated_serializers(src)
    if official:
        report["mode"] = "protobuf-net precompiled serializer (Read case labels and Write field headers)"
        seen, serial = set(), {k: v for k, v in official.items() if "tags" in v}
        for name, info in serial.items():
            report["serializer_types"] += 1
            cands = by_simple.get(name.split("<")[0], [])
            if not cands:
                if name.split("<")[0] in enum_names:
                    report["enum_serializers"].append(name)
                else:  # contract known only to the serializer (no attributes in the sources)
                    report["serializer_only_types"].append({"name": name, "tags": sorted(info["tags"]),
                                                            "wire_types": info.get("wires", {})})
                continue
            r = cands[0]
            seen.add(r["full_name"])
            expect = declared(r) | {i["tag"] for i in r["includes"]}
            if expect == info["tags"]:
                report["matching_types"] += 1
            else:
                report["mismatches"].append({
                    "type": r["full_name"], "attribute_only": sorted(expect - info["tags"]),
                    "serializer_only": sorted(info["tags"] - expect),
                })
            wires = info.get("wires", {})
            for m in r["members"]:
                if m["tag"] in wires:
                    m["wire_type"] = wires[m["tag"]]
            for tag in sorted(info["tags"] - expect):  # fields only the serializer knows
                r["members"].append({"tag": tag, "name": None, "type": None, "source": "serializer",
                                     "wire_type": wires.get(tag)})
        report["attribute_only_types"] = sorted(
            r["full_name"] for r in catalog.records.values()
            if r["kind"] != "enum" and r["full_name"] not in seen
            and (r["members"] or r["includes"]))
    elif gen_tags:
        report["mode"] = "GProtobuf.Core generated readers (Populate*OwnFields case labels, Read* subtype dispatch)"
        seen = set()
        for name, tags in gen_tags.items():
            report["serializer_types"] += 1
            cands = by_simple.get(name, [])
            if not cands:
                report["serializer_only_types" if name not in enum_names else "enum_serializers"].append(name)
                continue
            r = cands[0]
            seen.add(r["full_name"])
            if declared(r) == tags:
                report["matching_types"] += 1
            else:
                report["mismatches"].append({
                    "type": r["full_name"], "attribute_only": sorted(declared(r) - tags),
                    "serializer_only": sorted(tags - declared(r)),
                })
                for tag in sorted(tags - declared(r)):
                    r["members"].append({"tag": tag, "name": None, "type": None, "source": "serializer"})
        for name, sub in gen_sub.items():
            for r in by_simple.get(name, [])[:1]:
                attr = {i["tag"]: i["type"].split(".")[-1] for i in r["includes"]}
                fields = declared(r)  # message-typed fields also dispatch to Read*Content
                extra = {k: v for k, v in sub.items() if k not in fields and attr.get(k) != v}
                missing = {k: v for k, v in attr.items() if sub.get(k) != v}
                if extra or missing:
                    report["subtype_mismatches"].append(
                        {"type": r["full_name"], "attribute_only": missing, "serializer_only": extra})
        report["dispatcher_only_types"] = sorted(  # base types whose own fields are read inside Read<Type>
            r["full_name"] for r in catalog.records.values()
            if r["kind"] != "enum" and r["full_name"] not in seen and r["includes"])
        report["attribute_only_types"] = sorted(
            r["full_name"] for r in catalog.records.values()
            if r["kind"] != "enum" and r["full_name"] not in seen and r["members"] and not r["includes"])
    return report


# ---------------------------------------------------------------- diff report

NOTABLE = {
    "devices": re.compile(r"device|ccu|module|slave|thermostat|blind|scan", re.I),
    "packet parser": re.compile(r"packetparser|packet", re.I),
    "modbus": re.compile(r"modbus", re.I),
    "triggers / smart rules": re.compile(r"trigger|smartrule|rule|ifttt|script|scheduler|limitingcondition", re.I),
    "login / users": re.compile(r"login|reconnect|password|authoriz|session|token|forgotten|globaluser|localuser", re.I),
}


def load(path):
    return json.loads(Path(path).read_text())


def pair_types(old, new):
    """Match by full name, then by simple name when unambiguous (namespace moves)."""
    o = {t["full_name"]: t for t in old["types"]}
    n = {t["full_name"]: t for t in new["types"]}
    both = [(o[k], n[k]) for k in o if k in n]
    left, right = [o[k] for k in o if k not in n], [n[k] for k in n if k not in o]
    moved = []
    for rename in (lambda n: n, lambda n: n.replace("TriggerParameters", "SmartRuleParameters")):
        lc = Counter(rename(t["name"]) for t in left)
        rc = Counter(t["name"] for t in right)
        for t in list(left):
            key = rename(t["name"])
            if lc[key] == 1 and rc.get(key) == 1:
                u = next(x for x in right if x["name"] == key)
                moved.append((t, u))
                left.remove(t)
                right.remove(u)
    return both, moved, left, right


def describe_member(m):
    return f"{m['tag']} {m['name']}: {m['type']}"


def type_changes(a, b):
    out = []
    if a["kind"] == "enum":
        va = {v["name"]: v["value"] for v in a["values"]}
        vb = {v["name"]: v["value"] for v in b["values"]}
        out += [f"+value {k}={vb[k]}" for k in vb if k not in va]
        out += [f"-value {k}={va[k]}" for k in va if k not in vb]
        out += [f"value {k}: {va[k]} -> {vb[k]}" for k in va if k in vb and va[k] != vb[k]]
        return out
    ma = {m["tag"]: m for m in a.get("members", [])}
    mb = {m["tag"]: m for m in b.get("members", [])}
    out += [f"+field {describe_member(mb[t])}" for t in mb if t not in ma]
    out += [f"-field {describe_member(ma[t])}" for t in ma if t not in mb]
    for t in ma:
        if t in mb and (ma[t]["name"], ma[t]["type"]) != (mb[t]["name"], mb[t]["type"]):
            out.append(f"field {t}: {ma[t]['name']}: {ma[t]['type']} -> {mb[t]['name']}: {mb[t]['type']}")
    ia = {i["tag"]: i["type"] for i in a.get("includes", [])}
    ib = {i["tag"]: i["type"] for i in b.get("includes", [])}
    out += [f"+include {t} {ib[t]}" for t in ib if t not in ia]
    out += [f"-include {t} {ia[t]}" for t in ia if t not in ib]
    out += [f"include {t}: {ia[t]} -> {ib[t]}" for t in ia if t in ib and ia[t] != ib[t]]
    if a["base"] != b["base"]:
        out.append(f"base {a['base']} -> {b['base']}")
    ma_, mb_ = a.get("message"), b.get("message")
    if ma_ and mb_:
        for key in ("message_type", "min_protocol_version"):
            if ma_[key] != mb_[key]:
                out.append(f"{key} {ma_[key]} -> {mb_[key]}")
    return out


def notable_group(name):
    return [g for g, rx in NOTABLE.items() if rx.search(name)]


def write_diff(old_path, new_path, out_path):
    old, new = load(old_path), load(new_path)
    both, moved, removed, added = pair_types(old, new)
    changed = [(a, b, c) for a, b in both + moved if (c := type_changes(a, b))]
    om = {m["name"]: m for m in old["messages"]}
    nm = {m["name"]: m for m in new["messages"]}
    msg_added = sorted(set(nm) - set(om))
    msg_removed = sorted(set(om) - set(nm))
    msg_changed = sorted(k for k in om if k in nm and (om[k]["tag"], om[k]["min_protocol_version"], om[k]["message_type"])
                         != (nm[k]["tag"], nm[k]["min_protocol_version"], nm[k]["message_type"]))
    ov, nv = old["protocol_versions"], new["protocol_versions"]
    L = []
    w = L.append
    w(f"# Official vs Next: protobuf catalog diff\n")
    w(f"Generated by `scripts/build_catalog.py --diff` from `{Path(old_path).name}` and `{Path(new_path).name}`.\n")
    w("## Summary\n")
    w("| | Official | Next |\n|---|---|---|")
    for key in ("types", "classes", "structs", "enums", "members", "includes", "messages"):
        w(f"| {key} | {old['stats'][key]} | {new['stats'][key]} |")
    w(f"| protocol version constants | {len(ov)} (max {max(v for v in ov.values() if v)}) | {len(nv)} (max {max(v for v in nv.values() if v)}) |")
    w("")
    w(f"- Types: {len(added)} added, {len(removed)} removed, {len(moved)} moved or renamed (same name in another namespace, or TriggerParameters -> SmartRuleParameters), "
      f"{len(changed)} with changed fields, values or includes.")
    w(f"- Messages: {len(msg_added)} added, {len(msg_removed)} removed, {len(msg_changed)} with changed tag, "
      f"message type or minimum protocol version.")
    w(f"- Protocol version constants: {len(set(nv) - set(ov))} added, {len(set(ov) - set(nv))} removed, "
      f"{sum(1 for k in ov if k in nv and ov[k] != nv[k])} changed.")
    w(f"- Serialization library: Official uses protobuf-net (`ProtoBuf`) with a precompiled serializer, "
      f"Next uses GProtobuf.Core (`GProtobuf`, source generated). Type counts by library: "
      f"Official {old['stats']['proto_lib']}, Next {new['stats']['proto_lib']}.")
    w("")
    w("## Notable areas\n")
    all_names = {
        "messages added": msg_added, "messages removed": msg_removed,
        "types added": [t["name"] for t in added], "types removed": [t["name"] for t in removed],
        "types changed": [b["name"] for _, b, _ in changed],
    }
    for group in NOTABLE:
        w(f"### {group}\n")
        if group == "packet parser":
            w("No protobuf type or message mentions it. The packet parser is a formula function "
              "(`PacketParserFunction` in `TapHome.Core.Lib.Common`) configured through the JSON strings "
              "carried by the Modbus config messages.\n")
        for label, names in all_names.items():
            hits = sorted({n for n in names if group in notable_group(n)})
            if hits:
                w(f"- {label} ({len(hits)}): " + ", ".join(f"`{n}`" for n in hits))
        w("")
    w("## Messages added in Next\n")
    for k in msg_added:
        m = nm[k]
        w(f"- `{k}` tag {m['tag']}, {m['message_type']}, min protocol {m['min_protocol_version']}, pair {m['pair']}")
    w("\n## Messages removed in Next\n")
    for k in msg_removed:
        w(f"- `{k}` (tag {om[k]['tag']})")
    if not msg_removed:
        w("none")
    w("\n## Messages with changed header\n")
    for k in msg_changed:
        a, b = om[k], nm[k]
        w(f"- `{k}`: tag {a['tag']} -> {b['tag']}, type {a['message_type']} -> {b['message_type']}, "
          f"min protocol {a['min_protocol_version']} -> {b['min_protocol_version']}")
    if not msg_changed:
        w("none")
    w("\n## Protocol version constants\n")
    for k in sorted(set(nv) - set(ov)):
        w(f"- added `{k}` = {nv[k]}")
    for k in sorted(set(ov) - set(nv)):
        w(f"- removed `{k}` = {ov[k]}")
    for k in sorted(k for k in ov if k in nv and ov[k] != nv[k]):
        w(f"- changed `{k}`: {ov[k]} -> {nv[k]}")
    w("\n## Other types added in Next (non-message)\n")
    plain = sorted(t["full_name"] for t in added if "message" not in t)
    w(", ".join(f"`{n}`" for n in plain) or "none")
    w("\n## Other types removed in Next (non-message)\n")
    plain = sorted(t["full_name"] for t in removed if "message" not in t)
    w(", ".join(f"`{n}`" for n in plain) or "none")
    w("\n## Types moved or renamed\n")
    for a, b in moved:
        w(f"- `{a['full_name']}` -> `{b['full_name']}`")
    if not moved:
        w("none")
    w("\n## Changed types (fields, enum values, includes, base)\n")
    for a, b, changes in sorted(changed, key=lambda x: x[1]["full_name"]):
        w(f"- `{b['full_name']}`: " + "; ".join(changes[:12]) + (f"; ... {len(changes) - 12} more" if len(changes) > 12 else ""))
    Path(out_path).parent.mkdir(parents=True, exist_ok=True)
    Path(out_path).write_text("\n".join(L) + "\n")


# ---------------------------------------------------------------- self-test

SAMPLE = '''
using System;
using System.Collections.Generic;
using ProtoBuf;
namespace Demo.Communication.Messages;

public enum MessageType : byte { None, A, B = 5, C }
[Flags]
public enum Bits { X = 1, Y = 1 << 1, Z = X | Y }

[ProtoContract]
[ProtoInclude(7, typeof(MessageFooRequest))]
public abstract class MessageBase
{
    public abstract MessageType MessageType { get; }
    public virtual int MinProtocolVersion => 0;
    [ProtoMember(1)]
    public int RequestsId { get; set; }
}

[ProtoContract]
public class MessageFooRequest : MessageBase, IDisposable
{
    public override MessageType MessageType => MessageType.A;
    public override int MinProtocolVersion => ProtocolVersions.Foo;
    [ProtoMember(1, IsRequired = true)] // "tricky { comment"
    public List<Bits> Flags { get; set; }
    [ProtoMember(2, DataFormat = DataFormat.ZigZag), DefaultValue(-1)]
    public int[] Numbers;
    [ProtoMember(3)]
    public byte[] Blob { get; set; }
    [ProtoMember(4)]
    public Dictionary<Guid, List<int>> Map { get; set; }
    [ProtoMember(5)]
    public int? Maybe { get; set; }
    public string Text { get { return "}"; } }
}
public class ProtocolVersions { public const int Foo = 4; public const int Bar = Foo + 1; }
'''


def self_test():
    import tempfile
    with tempfile.TemporaryDirectory() as tmp:
        d = Path(tmp) / "src" / "Demo.Asm" / "Demo.Communication.Messages"
        d.mkdir(parents=True)
        (d / "Sample.cs").write_text(SAMPLE)
        cat = Catalog(Path(tmp) / "src", "test")
        cat.build()
        doc = cat.document({})
    recs = {r["name"]: r for r in doc["types"]}
    assert [v["value"] for v in recs["MessageType"]["values"]] == [0, 1, 5, 6]
    assert recs["Bits"]["flags"] and [v["value"] for v in recs["Bits"]["values"]] == [1, 2, 3]
    req = recs["MessageFooRequest"]
    members = {m["name"]: m for m in req["members"]}
    assert req["base"] == "MessageBase" and req["interfaces"] == ["IDisposable"]
    assert req["message"]["message_type"] == "A"
    assert req["message"]["min_protocol_version"] == 4
    assert members["Flags"]["repeated"] and members["Flags"]["type_ref"] == "Demo.Communication.Messages.Bits"
    assert members["Flags"]["options"] == {"IsRequired": True}
    assert members["Numbers"]["options"] == {"DataFormat": "DataFormat.ZigZag"} and members["Numbers"]["default"] == -1
    assert not members["Blob"]["repeated"]
    assert members["Map"]["map"] == {"key": "Guid", "value": "List<int>"}
    assert members["Maybe"]["nullable"]
    assert "Text" not in members
    assert recs["MessageBase"]["includes"] == [{"tag": 7, "type": "MessageFooRequest"}]
    assert doc["protocol_versions"] == {"Foo": 4, "Bar": 5}
    assert {m["name"]: m["tag"] for m in doc["messages"]}["MessageFooRequest"] == 7 and not doc["coverage"]["unparsed"], doc["coverage"]
    print("self-test ok")


# ---------------------------------------------------------------- main

def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("paths", nargs="*")
    ap.add_argument("--variant")
    ap.add_argument("--diff", action="store_true", help="paths: OLD_JSON NEW_JSON OUT_MD")
    ap.add_argument("--self-test", action="store_true")
    args = ap.parse_args(argv)
    if args.self_test:
        return self_test()
    if args.diff:
        if len(args.paths) != 3:
            ap.error("--diff needs OLD_JSON NEW_JSON OUT_MD")
        return write_diff(*args.paths)
    if len(args.paths) != 2:
        ap.error("need SRC_DIR OUT_JSON")
    src, out = Path(args.paths[0]), Path(args.paths[1])
    if not src.is_dir():
        ap.error(f"{src} is not a directory")
    cat = Catalog(src, args.variant or src.resolve().parent.name)
    cat.build()
    doc = cat.document(crosscheck(cat, src))
    doc["types"] = list(cat.records.values())  # crosscheck may append serializer-only members
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps(doc, indent=1, ensure_ascii=False, default=str) + "\n")
    s = doc["stats"]
    print(f"{doc['variant']}: {s['types']} types ({s['classes']} classes, {s['structs']} structs, "
          f"{s['enums']} enums), {s['members']} members, {s['messages']} messages; "
          f"{len(doc['coverage']['unparsed'])} unparsed, {len(doc['warnings'])} warnings")


if __name__ == "__main__":
    sys.exit(main())
