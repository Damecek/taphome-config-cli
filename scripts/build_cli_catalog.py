#!/usr/bin/env python3
"""Build data/catalog.json, the CLI's protocol catalog (embedded in the Rust binary), from the committed OpenAPI spec.

Usage:
    uv run --with pyyaml scripts/build_cli_catalog.py            # write the catalog
    uv run --with pyyaml scripts/build_cli_catalog.py --check    # compare with the committed file, write nothing

Input: openapi/taphome-core.yaml only (PyYAML is a dev dependency, the CLI reads the JSON output).

Output keys, deterministic (sorted keys, no timestamps):
    types       [{name, kind: class|enum, members: [{tag, name, type, wire_type?, options?}], includes: [{tag, type}],
                  values: [{name, value}]}], the format of work/catalog/official.json reduced to what a codec reads. `type` is the effective C# type
                  string (List<T>, Dictionary<K, V>, Tuple<...>, T?), so no repeated/element_type/map keys are needed.
    messages    [{name, message_type}]
    operations  {RequestClass: {ids, kind, verified, response, endpoint, min_protocol}}. ids: primary operationId first,
                then the variants. response: the typed response class, MessageGenericResponse, or null when no reply
                is expected. The spec merges manifest entries per class and keeps write/action as the merged kind.
    enums       name -> {value as string: name} for enums that are not schema types (CommonErrorCodes, TriggerType, ...)
"""
from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SPEC = ROOT / "openapi" / "taphome-core.yaml"
OUTPUT = ROOT / "data" / "catalog.json"
REF = "#/components/schemas/"
WRITE_KINDS = ("write", "action")
# JSON schema format of a map key (x-proto-map.key) -> C# type
KEY_TYPES = {"int32": "int", "int64": "long", "boolean": "bool", "string": "string", "uuid": "Guid"}
WIRE = {"fixed64": "Fixed64", "fixed32": "Fixed32"}


def load_spec(path=SPEC) -> dict:
    import yaml  # dev-only dependency
    with open(path, encoding="utf-8") as f:
        return yaml.safe_load(f)


def ref_name(s: dict) -> str:
    return s["$ref"][len(REF):]


def type_of(s: dict) -> str:
    """Effective C# type string of a property schema."""
    if "$ref" in s:
        return ref_name(s)
    if "oneOf" in s:
        opts = [o for o in s["oneOf"] if o.get("type") != "null"]
        if len(opts) != 1 or len(s["oneOf"]) != 2:
            raise ValueError("unsupported oneOf %r" % s)
        return type_of(opts[0]) + "?"
    t = s.get("type")
    nullable = isinstance(t, list) and "null" in t
    if isinstance(t, list):
        rest = [x for x in t if x != "null"]
        if len(rest) != 1:
            raise ValueError("unsupported type list %r" % t)
        t = rest[0]
    if "x-proto-clr-type" in s:
        base = s["x-proto-clr-type"]
    elif t == "integer":
        base = {"int32": "int", "int64": "long"}[s["format"]]
    elif t == "number":
        base = s["format"]  # double | float
    elif t == "boolean":
        base = "bool"
    elif t == "string":
        if s.get("contentEncoding") == "base64":
            base = "byte[]"
        else:
            base = {"uuid": "Guid", "date-time": "DateTime", "duration": "TimeSpan"}.get(s.get("format"), "string")
    elif t == "array":
        base = "List<%s>" % type_of(s["items"])
    elif t == "object" and "x-proto-map" in s:
        k = s["x-proto-map"]["key"]
        key = s["x-proto-map"].get("clr-key") or (k[5:] if k.startswith("enum:") else KEY_TYPES.get(k, k))
        base = "Dictionary<%s, %s>" % (key, type_of(s["additionalProperties"]))
    elif t == "object" and s.get("x-proto-type") in ("tuple", "key-value-pair"):
        props = sorted(s["properties"].values(), key=lambda p: p["x-proto-tag"])
        base = "Tuple<%s>" % ", ".join(type_of(p) for p in props)
    else:
        raise ValueError("unsupported schema %r" % s)
    return base + "?" if nullable else base


def member_of(name: str, p: dict) -> dict:
    m = {"tag": p["x-proto-tag"], "name": name, "type": type_of(p)}
    if p.get("x-proto-wire") in WIRE:
        m["wire_type"] = WIRE[p["x-proto-wire"]]
    opts = {}
    if p.get("x-proto-packed"):
        opts["IsPacked"] = True
    if p.get("x-proto-format") == "zigzag":
        opts["DataFormat"] = "ZigZag"
    if p.get("x-proto-required"):
        opts["IsRequired"] = True
    if opts:
        m["options"] = opts
    return m


def own_properties(s: dict) -> dict:
    if "allOf" in s:
        return s["allOf"][1].get("properties") or {}
    return s.get("properties") or {}


def enum_values(s: dict) -> list:
    if s.get("x-flags"):
        pairs = s["x-enum-values"].items()
    else:
        pairs = list(zip(s["x-enum-varnames"], s["enum"])) + list((s.get("x-enum-aliases") or {}).items())
    return [{"name": n, "value": v} for n, v in sorted(pairs, key=lambda x: (x[1], x[0]))]


def build_types(schemas: dict):
    """Classes and enums of the protobuf model. `XFields` schemas hold the own fields of class X."""
    fields_of = set()
    for s in schemas.values():
        for r in s.get("allOf", [])[:1] + s.get("oneOf", []):
            if "$ref" in r and ref_name(r).endswith("Fields") and ref_name(r) not in fields_of:
                fields_of.add(ref_name(r))
    types, includes = {}, {}
    for name, s in schemas.items():
        if name.startswith("Cloud"):  # hand-written HTTP DTOs from openapi/static.yaml
            continue
        tname = name[:-len("Fields")] if name in fields_of else name
        if "enum" in s or s.get("x-flags"):
            types[tname] = {"name": tname, "kind": "enum", "values": enum_values(s)}
            continue
        members = sorted((member_of(n, p) for n, p in own_properties(s).items()), key=lambda m: m["tag"])
        if tname in types and types[tname]["members"] != members:
            raise SystemExit("%s and %sFields disagree on members" % (tname, tname))
        types[tname] = {"name": tname, "kind": "class", "members": members}
        for inc in s.get("x-proto-includes") or []:
            includes.setdefault(tname, {})[inc["tag"]] = inc["schema"]
        if "x-proto-included-by" in s:
            includes.setdefault(s["x-proto-included-by"], {})[s["x-proto-include-tag"]] = tname
    for parent, incs in includes.items():
        if parent not in types:
            raise SystemExit("include parent %s has no schema" % parent)
        types[parent]["includes"] = [{"tag": t, "type": incs[t]} for t in sorted(incs)]
    messages = [{"name": n, "message_type": s["x-message-type"]} for n, s in schemas.items() if "x-message-type" in s]
    return [types[n] for n in sorted(types)], sorted(messages, key=lambda m: m["name"])


def response_class(op: dict):
    resp = op["responses"]["200"]
    if resp["description"].startswith("No reply is expected"):
        return None
    schema = resp["content"]["application/x-protobuf"]["schema"]
    return ref_name(schema.get("oneOf", [schema])[0])


def build_operations(paths: dict) -> dict:
    ops = {}
    for path, item in paths.items():
        for op in item.values():
            if op.get("x-transport") != "websocket":
                continue
            cls = path.rsplit("/", 1)[1]
            if cls in ops:
                raise SystemExit("request class %s appears on two paths" % cls)
            ops[cls] = {"ids": [v["id"] for v in op.get("x-variants") or []] or [op["operationId"]],
                        "kind": op["x-kind"], "verified": bool(op["x-verified"]), "response": response_class(op),
                        "endpoint": op["x-ws-endpoint"], "min_protocol": op["x-min-protocol-version"]}
    return ops


def build_enums(spec: dict) -> dict:
    wire = spec["x-taphome"]["wire"]
    out = {"CommonErrorCodes": wire["errorModel"]["CommonErrorCodes"],
           "ResponseFlags": wire["errorModel"]["ResponseFlags"],
           "MessageType": wire["messageTypes"]}
    for name, table in wire["enums"].items():
        if isinstance(table, dict):
            out[name] = table
    return {n: {str(k): v for k, v in t.items()} for n, t in out.items()}


def build(spec: dict) -> dict:
    types, messages = build_types(spec["components"]["schemas"])
    return {"source": "openapi/taphome-core.yaml (scripts/build_cli_catalog.py)",
            "types": types, "messages": messages,
            "operations": build_operations(spec["paths"]), "enums": build_enums(spec)}


def render(catalog: dict) -> str:
    return json.dumps(catalog, sort_keys=True, separators=(",", ":"), ensure_ascii=False) + "\n"


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--check", action="store_true", help="compare with the committed file, write nothing")
    args = ap.parse_args()
    text = render(build(load_spec()))
    if args.check:
        ok = OUTPUT.exists() and OUTPUT.read_text(encoding="utf-8") == text
        print("check %s: %s" % ("ok" if ok else "failed, run without --check", OUTPUT.relative_to(ROOT)))
        return 0 if ok else 1
    OUTPUT.parent.mkdir(parents=True, exist_ok=True)
    OUTPUT.write_text(text, encoding="utf-8")
    print("wrote %s: %d bytes" % (OUTPUT.relative_to(ROOT), len(text)))
    return 0


if __name__ == "__main__":
    sys.exit(main())
