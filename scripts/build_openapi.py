#!/usr/bin/env python3
"""Build openapi/taphome-core.yaml, the OpenAPI 3.1 description of the TapHome Core protocol.

Usage:
    uv run --with pyyaml scripts/build_openapi.py            # write openapi/taphome-core.yaml
    uv run --with pyyaml scripts/build_openapi.py --check    # build in memory, validate, compare with the file on disk

Inputs:
    openapi/operations/*.yaml     curated operation manifests (WebSocket request/response pairs, subscriptions)
    openapi/verification.yaml     which operations were tested against a real Core
    work/catalog/official.json    protobuf type catalog from scripts/build_catalog.py
    openapi/static.yaml           hand-written skeleton (info, tags, security, x-taphome), real HTTP endpoints, cloud DTOs

The TapHome app talks to the Core with protobuf-net messages over a WebSocket, not HTTP. Each request
message becomes a virtual path POST /Core/messages/<RequestClass>, each pushed broadcast becomes a
webhook. See the info.description of the output for the full mapping.

Standard library plus PyYAML. The output is deterministic (sorted paths, schemas and webhooks, properties
ordered by protobuf tag), so a regeneration only changes the lines that changed in the inputs.
"""
import argparse
import json
import re
import sys
from pathlib import Path

import yaml

ROOT = Path(__file__).resolve().parent.parent
OPS_DIR = ROOT / "openapi" / "operations"
VERIFICATION = ROOT / "openapi" / "verification.yaml"
CATALOG = ROOT / "work" / "catalog" / "official.json"
STATIC = ROOT / "openapi" / "static.yaml"
OUTPUT = ROOT / "openapi" / "taphome-core.yaml"

# Order decides which entry becomes the primary one when several ids share a request class.
OPS_FILES = ["session", "devices", "packet-parser", "automations", "admin"]
PROTO = "application/x-protobuf"
GENERIC = "MessageGenericResponse"
# Classes every client needs although no operation names them.
EXTRA_ROOTS = ["MessageContainer", "MessageReceiveAcknowledgement", "MessageLongRunningTaskProgress", "LocationInfo"]
BASE = "MessageBase"
KINDS = {"read", "write", "subscribe", "action"}
CONFIDENCE = {"observed-in-code", "inferred"}
WIRE = {"Variant": "varint", "Fixed64": "fixed64", "Fixed32": "fixed32", "String": "length-delimited", "SignedVariant": "zigzag"}

PRIMITIVES = {
    "int": {"type": "integer", "format": "int32"},
    "short": {"type": "integer", "format": "int32"},
    "sbyte": {"type": "integer", "format": "int32"},
    "byte": {"type": "integer", "format": "int32"},
    "ushort": {"type": "integer", "format": "int32"},
    "char": {"type": "integer", "format": "int32"},
    # shortcut: uint does not fit int32, so it is int64 here although the design note says int32.
    "uint": {"type": "integer", "format": "int64"},
    "long": {"type": "integer", "format": "int64"},
    "ulong": {"type": "integer", "format": "int64"},
    "bool": {"type": "boolean"},
    "string": {"type": "string"},
    "double": {"type": "number", "format": "double"},
    "float": {"type": "number", "format": "float"},
    "Guid": {"type": "string", "format": "uuid", "x-proto-encoding": "bcl-guid"},
    "DateTime": {"type": "string", "format": "date-time", "x-proto-encoding": "bcl-datetime"},
    "TimeSpan": {"type": "string", "format": "duration", "x-proto-encoding": "bcl-timespan"},
    "Type": {"type": "string", "description": "Assembly-qualified type name."},
}
KEY_NAMES = {"int": "int32", "short": "int32", "byte": "int32", "sbyte": "int32", "ushort": "int32", "uint": "int64",
             "long": "int64", "ulong": "int64", "bool": "boolean", "string": "string", "Guid": "uuid", "char": "int32"}
LISTS = {"List", "HashSet", "IList", "IEnumerable", "ICollection", "ISet", "Queue", "Stack", "Collection"}
MAPS = {"Dictionary", "ConcurrentDictionary", "SortedDictionary", "IDictionary", "ListDictionary", "SortedList"}


# ---------------------------------------------------------------- loading and validating

def load_yaml(path):
    with open(path, encoding="utf-8") as f:
        return yaml.safe_load(f)


def load_operations():
    """Return the operation entries in a stable order, each tagged with its source file."""
    ops = []
    for name in OPS_FILES:
        for entry in load_yaml(OPS_DIR / f"{name}.yaml") or []:
            entry["_file"] = name
            ops.append(entry)
    extra = {p.stem for p in OPS_DIR.glob("*.yaml")} - set(OPS_FILES)
    if extra:
        raise SystemExit(f"operations files not listed in OPS_FILES: {sorted(extra)}")
    return ops


def validate_operations(ops, types, tag_names):
    errors, seen = [], set()
    for e in ops:
        i = e.get("id")
        if i in seen:
            errors.append(f"duplicate operation id {i}")
        seen.add(i)
        if e.get("kind") not in KINDS:
            errors.append(f"{i}: bad kind {e.get('kind')}")
        if e.get("confidence") not in CONFIDENCE:
            errors.append(f"{i}: bad confidence {e.get('confidence')}")
        if e.get("tag") not in tag_names:
            errors.append(f"{i}: unknown tag {e.get('tag')}")
        for key in ("request", "response"):
            if e.get(key) is not None and e[key] not in types:
                errors.append(f"{i}: {key} class {e[key]} is not in the catalog")
        for b in e.get("broadcasts") or []:
            if b not in types:
                errors.append(f"{i}: broadcast {b} is not in the catalog")
    if errors:
        raise SystemExit("operation errors:\n  " + "\n  ".join(errors))


# ---------------------------------------------------------------- type expressions

TOKEN = re.compile(r"\s*([A-Za-z_][\w.]*|<|>|,|\[\]|\?)")


def parse_type(text):
    """Parse a C# type such as 'Dictionary<int, List<Guid>>' into (name, args, array_rank, nullable)."""
    tokens = TOKEN.findall(text)
    pos = 0

    def one():
        nonlocal pos
        name = tokens[pos]
        pos += 1
        args = []
        if pos < len(tokens) and tokens[pos] == "<":
            pos += 1
            args.append(one())
            while tokens[pos] == ",":
                pos += 1
                args.append(one())
            pos += 1  # '>'
        rank = 0
        nullable = False
        while pos < len(tokens) and tokens[pos] in ("[]", "?"):
            if tokens[pos] == "[]":
                rank += 1
            else:
                nullable = True
            pos += 1
        return (name, args, rank, nullable)

    result = one()
    if pos != len(tokens):
        raise ValueError(f"cannot parse type {text!r}")
    return result


def make_nullable(schema):
    if set(schema) == {"$ref"}:
        return {"oneOf": [schema, {"type": "null"}]}
    out = dict(schema)
    if isinstance(out.get("type"), str):
        out["type"] = [out["type"], "null"]
    return out


class Builder:
    """Collects component schemas for the transitive closure of the root classes."""

    def __init__(self, catalog):
        self.types = {t["name"]: t for t in catalog["types"]}
        self.messages = {m["name"]: m for m in catalog["messages"]}
        for name in self.types:
            if name + "Fields" in self.types:
                raise SystemExit(f"catalog type {name}Fields collides with the generated {name}Fields schema")
        self.schemas = {}
        self.warnings = []
        self.pending = []
        # child -> (parent, tag), from every ProtoInclude in the catalog
        self.included = {}
        for t in catalog["types"]:
            for inc in t.get("includes") or []:
                self.included[inc["type"]] = (t["name"], inc["tag"])

    def resolve(self, name):
        """Catalog name for a possibly dotted C# name (names are unique in the catalog)."""
        short = name.split(".")[-1]
        return short if short in self.types else None

    def ref(self, name):
        if name not in self.schemas and name not in self.pending:
            self.pending.append(name)
        return {"$ref": f"#/components/schemas/{name}"}

    # -- type expressions
    def schema_of(self, text):
        return self.schema_of_parsed(parse_type(text))

    def base_schema(self, name, args):
        short = name.split(".")[-1]
        if short in LISTS and len(args) == 1:
            return {"type": "array", "items": self.schema_of_parsed(args[0])}
        if short in MAPS and len(args) == 2:
            return {"type": "object", "additionalProperties": self.schema_of_parsed(args[1]),
                    "x-proto-map": {"key": self.key_name(args[0])}}
        if short == "KeyValuePair" and len(args) == 2:
            return {"type": "object", "x-proto-type": "key-value-pair",
                    "properties": {"Key": dict(self.schema_of_parsed(args[0]), **{"x-proto-tag": 1}),
                                   "Value": dict(self.schema_of_parsed(args[1]), **{"x-proto-tag": 2})}}
        if short == "Tuple" and args:
            return {"type": "object", "x-proto-type": "tuple",
                    "properties": {f"Item{n}": dict(self.schema_of_parsed(a), **{"x-proto-tag": n})
                                   for n, a in enumerate(args, 1)}}
        if short in PRIMITIVES:
            return dict(PRIMITIVES[short])
        resolved = self.resolve(name)
        if resolved:
            return self.ref(resolved)
        self.warnings.append(f"type {name} is not in the catalog")
        return {"description": f"Type {name} is not in the catalog, encoding unknown."}

    def schema_of_parsed(self, parsed):
        name, args, rank, nullable = parsed
        if name == "byte" and rank >= 1:
            s, rank = {"type": "string", "contentEncoding": "base64"}, rank - 1
        else:
            s = self.base_schema(name, args)
        for _ in range(rank):
            s = {"type": "array", "items": s}
        return make_nullable(s) if nullable else s

    def key_name(self, parsed):
        name = parsed[0].split(".")[-1]
        if name in KEY_NAMES:
            return KEY_NAMES[name]
        if self.resolve(name) and self.types[self.resolve(name)]["kind"] == "enum":
            return f"enum:{self.resolve(name)}"
        return name

    # -- classes and enums
    def property_of(self, m):
        s = self.schema_of(m["type"])
        if m.get("nullable") and "null" not in json.dumps(s):
            s = make_nullable(s)
        s["x-proto-tag"] = m["tag"]
        wire = WIRE.get(m.get("wire_type"))
        if wire:
            s["x-proto-wire"] = wire
        opts = m.get("options") or {}
        if opts.get("IsPacked"):
            s["x-proto-packed"] = True
        if "ZigZag" in str(opts.get("DataFormat", "")):
            s["x-proto-format"] = "zigzag"
        if opts.get("IsRequired"):
            s["x-proto-required"] = True
        if m.get("default") is not None:
            s["x-proto-default"] = str(m["default"])
        return s

    def enum_schema(self, t):
        values = t["values"]
        s = {"type": "integer"}
        if t.get("flags"):
            s["x-flags"] = True
            s["description"] = "Bit flags, values can be combined with bitwise or."
            s["x-enum-values"] = {v["name"]: v["value"] for v in values}
        else:
            uniq, names, aliases = [], [], {}
            for v in values:
                if v["value"] in uniq:
                    aliases[v["name"]] = v["value"]
                else:
                    uniq.append(v["value"])
                    names.append(v["name"])
            s["enum"] = uniq
            s["x-enum-varnames"] = names
            if aliases:
                s["x-enum-aliases"] = aliases
        s["x-proto-wire"] = "varint"
        return s

    def has_includes(self, name):
        return bool(self.types[name].get("includes"))

    def class_schema(self, name, fields_only=False):
        """Schema of a class.

        A class with subtypes gets two schemas. `Name` is polymorphic (oneOf over the subtypes). `NameFields` holds
        only the class own fields and is what derived classes extend with allOf. Splitting them keeps the reference
        graph free of cycles, which validators such as openapi-spec-validator cannot process.
        """
        t = self.types[name]
        own = {"type": "object",
               "properties": {m["name"]: self.property_of(m) for m in sorted(t.get("members") or [], key=lambda m: m["tag"])}}
        if not own["properties"]:
            del own["properties"]
        base = t.get("base")
        out = {}
        if name == BASE:
            out["description"] = ("Envelope of every WebSocket frame. RequestsId (field 1) correlates a response with its request. "
                                  "The concrete message is one nested length-delimited field, numbered by x-proto-include-tag, "
                                  "written before field 1.")
        msg = self.messages.get(name)
        if msg and msg.get("message_type"):
            out["x-message-type"] = msg["message_type"]
        base_ref = None
        if base:
            if self.resolve(base) and "<" not in base:
                resolved = self.resolve(base)
                base_ref = self.ref(resolved + "Fields" if self.has_includes(resolved) else resolved)
            elif "<" in base:
                out["x-proto-base"] = base
        if base_ref:
            out["allOf"] = [base_ref, own]
        else:
            out.update(own)
        if t.get("abstract"):
            out["x-proto-abstract"] = True
        parent = self.included.get(name)
        if parent:
            out["x-proto-include-tag"] = parent[1]
            out["x-proto-included-by"] = parent[0]
            path, cur = [], name
            while cur in self.included:
                path.append(self.included[cur][1])
                cur = self.included[cur][0]
            if len(path) > 1:
                out["x-proto-include-path"] = path[::-1]
        if msg and msg.get("min_protocol_version"):
            out["x-min-protocol-version"] = msg["min_protocol_version"]
        includes = sorted(t.get("includes") or [], key=lambda i: i["tag"])
        if includes and not fields_only:
            out["x-proto-includes"] = [{"tag": i["tag"], "schema": i["type"]} for i in includes]
            if name != BASE:  # MessageBase is completed in finish_base with the subtypes that exist
                options = [] if t.get("abstract") else [self.ref(name + "Fields")]
                out["oneOf"] = options + [self.ref(i["type"]) for i in includes]
            out["x-proto-subtype-encoding"] = ("The subtype is encoded as a nested length-delimited field, field number = tag, "
                                               "written before the base fields.")
        return out

    def build(self, roots):
        for r in roots:
            self.ref(r)
        while self.pending:
            name = self.pending.pop(0)
            if name in self.schemas:
                continue
            self.schemas[name] = None  # reserve against cycles
            if name not in self.types:
                self.schemas[name] = self.class_schema(name[:-len("Fields")], fields_only=True)
            elif self.types[name]["kind"] == "enum":
                self.schemas[name] = self.enum_schema(self.types[name])
            else:
                self.schemas[name] = self.class_schema(name)
        # MessageBase lists only the subtypes that ended up in the document
        if BASE in self.schemas:
            self.finish_base()

    def finish_base(self):
        t = self.types[BASE]
        present = [i for i in sorted(t["includes"], key=lambda i: i["tag"]) if i["type"] in self.schemas]
        s = self.schemas[BASE]
        s["x-proto-includes"] = [{"tag": i["tag"], "schema": i["type"]} for i in present]
        s["oneOf"] = [{"$ref": f"#/components/schemas/{i['type']}"} for i in present]
        assert t.get("abstract"), "MessageBase is expected to be abstract"
        s["x-proto-subtype-encoding"] = ("The subtype is encoded as a nested length-delimited field, field number = tag, "
                                         "written before field 1.")
        s["x-proto-includes-note"] = (f"Lists the {len(present)} subtypes that this document describes. "
                                      f"The catalog knows {len(t['includes'])} in total.")


# ---------------------------------------------------------------- operations to paths

def ref_schema(name):
    return {"$ref": f"#/components/schemas/{name}"}


def camel_words(name):
    return re.sub(r"(?<=[a-z0-9])(?=[A-Z])", " ", name)


def group_operations(ops):
    """Group wire operations by (endpoint, message class). Subscribe-only entries are returned separately."""
    groups, subs = {}, []
    for e in ops:
        if e.get("request") is None and e.get("response") is None:
            subs.append(e)
            continue
        cls = e.get("request") or e["response"]
        groups.setdefault((e.get("endpoint", "/Core"), cls), []).append(e)
    return groups, subs


def verification_for(ids, verification):
    for i in ids:
        if i in verification and verification[i].get("verified"):
            return verification[i]
    return None


def build_path_item(variants, builder, verification, tag_order):
    primary = variants[0]
    endpoint = primary.get("endpoint", "/Core")
    bare = primary.get("request") is None
    request = primary.get("request")
    responses = []
    for v in variants:
        r = v.get("response")
        if r and r not in responses:
            responses.append(r)
    typed = [r for r in responses if r != GENERIC]

    description = primary["description"]
    if len(variants) > 1:
        description += "\n\nThis message is used for several purposes. Variants:\n" + "\n".join(
            f"- `{v['id']}`: {v['summary']}" for v in variants)
    tags = sorted({v["tag"] for v in variants}, key=tag_order.index)
    op = {
        "operationId": primary["id"],
        "tags": tags,
        "summary": primary["summary"],
        "description": description,
        "security": [] if primary.get("security") == "none" else [{"coreSession": []}],
    }
    if request:
        op["requestBody"] = {"required": True, "content": {PROTO: {"schema": ref_schema(request)}}}
    if typed:
        options = [ref_schema(r) for r in typed] + [ref_schema(GENERIC)]
        schema = options[0] if len(options) == 1 else {"oneOf": options}
        text = "Typed response, or MessageGenericResponse when the Core reports an error." if not bare else \
            "Bare protobuf message, not wrapped in MessageBase."
        if bare:
            schema = ref_schema(typed[0])
    else:
        schema = ref_schema(GENERIC)
        text = ("MessageGenericResponse (ErrorCode 1 is success)." if responses
                else "No reply is expected. Errors, if any, come back as MessageGenericResponse.")
    op["responses"] = {"200": {"description": text, "content": {PROTO: {"schema": schema}}}}

    op["x-transport"] = "websocket"
    op["x-ws-endpoint"] = endpoint
    if bare:
        op["x-bare-message"] = True
    else:
        op["x-proto-include-tag"] = tag_of(builder, request)
    if typed and not bare:
        tags_ = [tag_of(builder, r) for r in typed]
        op["x-response-proto-include-tag"] = tags_[0] if len(tags_) == 1 else tags_
    op["x-kind"] = primary["kind"]
    op["x-min-protocol-version"] = min(min_version(v, builder) for v in variants)
    op["x-broadcasts"] = sorted({b for v in variants for b in v.get("broadcasts") or []})
    evidence = []
    for v in variants:
        for item in v.get("evidence") or []:
            if item not in evidence:
                evidence.append(item)
    op["x-evidence"] = evidence
    op["x-confidence"] = "observed-in-code" if any(v["confidence"] == "observed-in-code" for v in variants) else "inferred"
    found = verification_for([v["id"] for v in variants], verification)
    op["x-verified"] = bool(found)
    if found:
        op["x-verification"] = dict(found)
    if len(variants) > 1:
        op["x-variants"] = [{"id": v["id"], "summary": v["summary"], "description": v["description"]} for v in variants]
    cls = request or primary["response"]
    path = f"{endpoint}/messages/{cls}"
    return path, {"get" if bare else "post": op}


def tag_of(builder, cls):
    parent = builder.included.get(cls)
    return parent[1] if parent else None


def min_version(entry, builder):
    from_catalog = 0
    for key in ("request", "response"):
        cls = entry.get(key)
        if cls:
            from_catalog = max(from_catalog, (builder.messages.get(cls) or {}).get("min_protocol_version") or 0)
    return max(int(entry.get("minProtocolVersion") or 0), from_catalog)


WEBHOOK_TAGS = [("Permissions", "permissions"), ("Dashboard", "dashboards"), ("Trigger", "automations"),
                ("SmartRule", "automations"), ("WeekPlan", "access"), ("DateRange", "access"), ("AccessCard", "access"),
                ("AccessLog", "access"), ("User", "users"), ("Media", "media"), ("Doorbell", "media"),
                ("Export", "export"), ("Backup", "backup"), ("Update", "updates"), ("Slave", "taphome-api"),
                ("Modbus", "modbus"), ("Ccu", "interfaces"), ("Module", "modules"), ("Device", "devices")]


def build_webhooks(ops, subs, builder, verification, broadcast_names, tag_order):
    related, sub_by_class = {}, {}
    for e in ops:
        for b in e.get("broadcasts") or []:
            related.setdefault(b, []).append(e)
    for e in subs:
        for b in e.get("broadcasts") or []:
            sub_by_class.setdefault(b, []).append(e)
    hooks = {}
    for name in sorted(broadcast_names):
        subs_ = sub_by_class.get(name, [])
        triggers = [e for e in related.get(name, []) if e not in subs_]
        short = name.removeprefix("Message").removesuffix("Broadcast")
        summary = subs_[0]["summary"] if subs_ else f"Broadcast pushed by the Core: {camel_words(short).lower()}."
        description = subs_[0]["description"] if subs_ else (
            "The Core pushes this message to every logged-in client. The client does not reply with a response, "
            "it only acknowledges receipt (MessageReceiveAcknowledgement).")
        for extra in subs_[1:]:
            description += f"\n\n{extra['summary']}\n{extra['description']}"
        if triggers:
            description += "\n\nPushed after: " + ", ".join(f"`{e['id']}`" for e in triggers) + "."
        pool = subs_ + triggers
        tag = pool[0]["tag"] if pool else next((t for key, t in WEBHOOK_TAGS if key in name), "core")
        evidence = []
        for e in pool:
            for item in e.get("evidence") or []:
                if item not in evidence:
                    evidence.append(item)
        op_id = "broadcast" + short
        found = verification.get(op_id) if verification.get(op_id, {}).get("verified") else None
        op = {
            "operationId": op_id,
            "tags": [tag],
            "summary": summary,
            "description": description,
            "security": [{"coreSession": []}],
            "requestBody": {"required": True, "content": {PROTO: {"schema": ref_schema(name)}}},
            "responses": {"200": {"description": "No reply. The client acknowledges received messages with MessageReceiveAcknowledgement."}},
            "x-transport": "websocket",
            "x-ws-endpoint": "/Core",
            "x-kind": "broadcast",
            "x-proto-include-tag": tag_of(builder, name),
            "x-min-protocol-version": min_version({"request": name, "minProtocolVersion": 0}, builder),
            "x-triggered-by": sorted(e["id"] for e in triggers),
            "x-evidence": evidence,
            "x-confidence": "observed-in-code" if any(e["confidence"] == "observed-in-code" for e in pool) or not pool else "inferred",
            "x-verified": bool(found),
        }
        if subs_:
            op["x-operation-ids"] = [e["id"] for e in subs_]
        if found:
            op["x-verification"] = dict(found)
        hooks[name] = {"post": op}
    return hooks


# ---------------------------------------------------------------- assembling the document

def build(check=False):
    catalog = json.loads(CATALOG.read_text(encoding="utf-8"))
    static = load_yaml(STATIC)
    verification = load_yaml(VERIFICATION) or {}
    tag_names = [t["name"] for t in static["tags"]]
    builder = Builder(catalog)
    ops = load_operations()
    validate_operations(ops, builder.types, tag_names)

    groups, subs = group_operations(ops)
    paths = {}
    for key in sorted(groups):
        path, item = build_path_item(groups[key], builder, verification, tag_names)
        paths[path] = item

    broadcast_names = {m["name"] for m in catalog["messages"] if m["name"].endswith("Broadcast")}
    for e in ops:
        broadcast_names.update(e.get("broadcasts") or [])
    webhooks = build_webhooks(ops, subs, builder, verification, broadcast_names, tag_names)

    roots = {GENERIC, *EXTRA_ROOTS, *broadcast_names}
    for e in ops:
        roots.update(x for x in (e.get("request"), e.get("response")) if x)
    builder.build(sorted(roots))

    # hand-written HTTP endpoints, with verification applied
    for path, item in static["paths"].items():
        for method, op in item.items():
            found = verification.get(op["operationId"])
            found = found if found and found.get("verified") else None
            op["x-verified"] = bool(found)
            if found:
                op["x-verification"] = dict(found)
        paths[path] = item

    schemas = dict(builder.schemas)
    for name, schema in static["schemas"].items():
        if name in schemas:
            raise SystemExit(f"schema name {name} exists in the catalog and in static.yaml")
        schemas[name] = schema

    doc = {
        "openapi": "3.1.0",
        "info": static["info"],
        "servers": static["servers"],
        "tags": static["tags"],
        "paths": dict(sorted(paths.items())),
        "webhooks": dict(sorted(webhooks.items())),
        "components": {"securitySchemes": static["securitySchemes"], "schemas": dict(sorted(schemas.items()))},
        "x-taphome": static["x-taphome"],
    }
    return doc, builder.warnings, verification


# ---------------------------------------------------------------- checking

def iter_refs(node):
    if isinstance(node, dict):
        for k, v in node.items():
            if k == "$ref" and isinstance(v, str):
                yield v
            else:
                yield from iter_refs(v)
    elif isinstance(node, list):
        for v in node:
            yield from iter_refs(v)


WS_POST_REQUIRED = ["operationId", "tags", "summary", "description", "security", "requestBody", "responses",
                    "x-transport", "x-ws-endpoint", "x-proto-include-tag", "x-kind", "x-min-protocol-version",
                    "x-broadcasts", "x-evidence", "x-confidence", "x-verified"]
WEBHOOK_REQUIRED = [k for k in WS_POST_REQUIRED if k not in ("security", "x-broadcasts")]
HTTP_REQUIRED = ["operationId", "x-evidence", "x-verified"]


def check_document(doc, verification):
    errors = []
    schemas = doc["components"]["schemas"]
    for ref in sorted(set(iter_refs(doc))):
        if not ref.startswith("#/components/schemas/") or ref.rsplit("/", 1)[1] not in schemas:
            errors.append(f"unresolved $ref {ref}")
    tag_names = {t["name"] for t in doc["tags"]}
    op_ids = []

    def check_op(where, op, required):
        for key in required:
            if key not in op:
                errors.append(f"{where}: missing {key}")
        op_ids.append(op.get("operationId"))
        for t in op.get("tags", []):
            if t not in tag_names:
                errors.append(f"{where}: unknown tag {t}")
        if op.get("x-verified") and "x-verification" not in op:
            errors.append(f"{where}: x-verified without x-verification")

    for path, item in doc["paths"].items():
        for method, op in item.items():
            where = f"{method.upper()} {path}"
            if op.get("x-transport") == "websocket":
                required = [k for k in WS_POST_REQUIRED if not (method == "get" and k in ("requestBody", "x-proto-include-tag"))]
                check_op(where, op, required)
            else:
                check_op(where, op, HTTP_REQUIRED)
    for name, item in doc["webhooks"].items():
        check_op(f"webhook {name}", item["post"], WEBHOOK_REQUIRED)
    dupes = {i for i in op_ids if op_ids.count(i) > 1}
    if dupes:
        errors.append(f"duplicate operationId: {sorted(dupes)}")
    for key in verification:
        if key not in op_ids:
            errors.append(f"verification.yaml names unknown operation {key}")
    for name, schema in schemas.items():
        if name.startswith("Cloud"):
            continue
        for part in ([schema] + schema.get("allOf", [])):
            for prop, p in (part.get("properties") or {}).items():
                if "x-proto-tag" not in p:
                    errors.append(f"schema {name}.{prop}: missing x-proto-tag")
    return errors


# ---------------------------------------------------------------- output

class Dumper(yaml.SafeDumper):
    def ignore_aliases(self, data):
        return True


def _str(dumper, data):
    style = "|" if "\n" in data else None
    return dumper.represent_scalar("tag:yaml.org,2002:str", data, style=style)


Dumper.add_representer(str, _str)


def render(doc):
    header = ("# GENERATED by scripts/build_openapi.py. Do not edit.\n"
              "# Sources: openapi/operations/*.yaml, openapi/verification.yaml, openapi/static.yaml, work/catalog/official.json\n")
    return header + yaml.dump(doc, Dumper=Dumper, sort_keys=False, allow_unicode=True, width=110, default_flow_style=False)


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--check", action="store_true", help="validate in memory and compare with the file on disk, write nothing")
    args = ap.parse_args()
    doc, warnings, verification = build()
    errors = check_document(doc, verification)
    text = render(doc)
    for w in sorted(set(warnings)):
        print(f"warning: {w}", file=sys.stderr)
    if args.check:
        if not OUTPUT.exists() or OUTPUT.read_text(encoding="utf-8") != text:
            errors.append(f"{OUTPUT.relative_to(ROOT)} is missing or out of date, run the script without --check")
        for e in errors:
            print(f"error: {e}", file=sys.stderr)
        print(f"check {'failed' if errors else 'ok'}: {len(doc['paths'])} paths, {len(doc['webhooks'])} webhooks, "
              f"{len(doc['components']['schemas'])} schemas")
        return 1 if errors else 0
    if errors:
        for e in errors:
            print(f"error: {e}", file=sys.stderr)
        return 1
    OUTPUT.write_text(text, encoding="utf-8")
    print(f"wrote {OUTPUT.relative_to(ROOT)}: {len(doc['paths'])} paths, {len(doc['webhooks'])} webhooks, "
          f"{len(doc['components']['schemas'])} schemas, {text.count(chr(10))} lines")
    return 0


if __name__ == "__main__":
    sys.exit(main())
