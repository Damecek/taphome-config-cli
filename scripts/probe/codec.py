"""Catalog-driven protobuf-net codec for the TapHome wire format.

The catalog (work/catalog/*.json, built by scripts/build_catalog.py) describes every type.
Values are plain Python data:

* ints, bools, floats, str; enums are ints.
* byte[]: lower-case hex string on decode (JSON friendly); bytes or hex string on encode.
* Guid: canonical uuid string (bcl.proto Guid, two fixed64 halves of Guid.ToByteArray()).
* DateTime: ISO string "YYYY-MM-DDTHH:MM:SS.fffffff" (7 digits = .NET ticks), naive/UTC,
  bcl.proto DateTime as a TimeSpan since 1970-01-01. Min/Max map to year 0001 / 9999 strings.
* TimeSpan: float seconds (exact value is ticks / 1e7). Min/Max map to -inf / inf.
* Dictionary<K,V>: dict (int keys stay ints; on encode string keys are coerced). A list or
  dict value is written as repeated field 2 inside the entry, as the generated serializer does.
* Lists, arrays, HashSet<T>: list. Non-packed on write, except members the catalog marks
  IsPacked. Packed and unpacked are both accepted on read.
* Nullable (T?): written whenever the key is present and not None.
* Inheritance: decode returns a flat dict with "$type" naming the concrete class; encode takes
  the concrete class from "$type" (default: the type given).
* Unknown fields: kept in "$unknown" as {"tag", "wt", "hex", "in"} and written back on encode.
"""
from __future__ import annotations

import json
import re
import struct
import uuid
from datetime import datetime, timedelta

EPOCH = datetime(1970, 1, 1)
TICKS_PER_SCALE = {0: 864000000000, 1: 36000000000, 2: 600000000, 3: 10000000, 4: 10000, 5: 1}
INT_TYPES = {"int", "short", "long", "sbyte", "uint", "ushort", "ulong", "byte", "char"}
SIGNED = {"int": 32, "short": 16, "long": 64, "sbyte": 8}
UNSIGNED = {"uint": 32, "ushort": 16, "ulong": 64, "byte": 8, "char": 16}
MAX_TICKS = 3155378975999999999  # DateTime.MaxValue.Ticks
MESSAGE_TYPE = {"None": 0, "Request": 1, "Response": 2, "Broadcast": 3, "Stream": 4,
                "Acknowledgement": 5, "Container": 6, "Progress": 7}


class CodecError(Exception):
    pass


# ---------------------------------------------------------------- low-level wire helpers

def varint(n: int) -> bytes:
    n &= (1 << 64) - 1
    out = bytearray()
    while n > 0x7F:
        out.append((n & 0x7F) | 0x80)
        n >>= 7
    out.append(n)
    return bytes(out)


def read_varint(b: bytes, i: int):
    shift = n = 0
    while True:
        if i >= len(b):
            raise CodecError("truncated varint")
        c = b[i]
        i += 1
        n |= (c & 0x7F) << shift
        if not c & 0x80:
            return n, i
        shift += 7


def zigzag(n: int) -> int:
    return ((n << 1) ^ (n >> 63)) & ((1 << 64) - 1)


def unzigzag(n: int) -> int:
    return (n >> 1) ^ -(n & 1)


def key(tag: int, wt: int) -> bytes:
    return varint((tag << 3) | wt)


def ld(tag: int, payload: bytes) -> bytes:
    return key(tag, 2) + varint(len(payload)) + payload


def read_fields(b: bytes):
    """Yield (tag, wire_type, value, raw) with value int (wt 0/1/5 as raw int) or bytes (wt 2)."""
    i = 0
    while i < len(b):
        k, i = read_varint(b, i)
        tag, wt = k >> 3, k & 7
        if wt == 0:
            v, j = read_varint(b, i)
            raw = b[i:j]
            i = j
            yield tag, wt, v, raw
        elif wt == 1 or wt == 5:
            n = 8 if wt == 1 else 4
            if i + n > len(b):
                raise CodecError("truncated fixed field")
            raw = b[i:i + n]
            i += n
            yield tag, wt, int.from_bytes(raw, "little"), raw
        elif wt == 2:
            n, i = read_varint(b, i)
            if i + n > len(b):
                raise CodecError("truncated length-delimited field")
            raw = b[i:i + n]
            i += n
            yield tag, wt, raw, raw
        else:
            raise CodecError("unsupported wire type %d (tag %d)" % (wt, tag))


# ---------------------------------------------------------------- bcl types

def _split_ticks(b: bytes) -> int:
    """Decode a bcl TimeSpan message to ticks. MinMax scale returns +-MAX_TICKS sentinels."""
    value = scale = 0
    for tag, wt, v, _ in read_fields(b):
        if tag == 1:
            value = unzigzag(v)
        elif tag == 2:
            scale = v
    if scale == 15:
        return MAX_TICKS + 1 if value > 0 else -(MAX_TICKS + 1)
    return value * TICKS_PER_SCALE[scale]


def _ticks_message(ticks: int, minmax: int = 0) -> bytes:
    if minmax:
        return vfield(1, zigzag(minmax)) + vfield(2, 15)
    scale = 5
    for s in (0, 1, 2, 3, 4):
        if ticks % TICKS_PER_SCALE[s] == 0:
            scale = s
            break
    value = ticks // TICKS_PER_SCALE[scale]
    out = b""
    if value:
        out += vfield(1, zigzag(value))
    if scale:
        out += vfield(2, scale)
    return out


def vfield(tag: int, n: int) -> bytes:  # varint field
    return key(tag, 0) + varint(n)


def enc_guid(s) -> bytes:
    if s is None or int(uuid.UUID(str(s))) == 0:
        return b""
    raw = uuid.UUID(str(s)).bytes_le
    return b"\x09" + raw[:8] + b"\x11" + raw[8:]


def dec_guid(b: bytes) -> str:
    lo = hi = b"\0" * 8
    for tag, wt, v, raw in read_fields(b):
        if tag == 1:
            lo = raw
        elif tag == 2:
            hi = raw
    return str(uuid.UUID(bytes_le=lo + hi))


def dec_datetime(b: bytes) -> str:
    t = _split_ticks(b)
    if t > MAX_TICKS:
        return "9999-12-31T23:59:59.9999999"
    if t < -MAX_TICKS:
        return "0001-01-01T00:00:00.0000000"
    sec, frac = divmod(t, 10_000_000)
    dt = EPOCH + timedelta(seconds=sec)
    return dt.strftime("%Y-%m-%dT%H:%M:%S") + ".%07d" % frac


def enc_datetime(s: str) -> bytes:
    m = re.match(r"^(\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d)(?:\.(\d{1,7}))?Z?$", s)
    if not m:
        raise CodecError("bad DateTime %r" % s)
    if s.startswith("9999-12-31T23:59:59.9999999"):
        return _ticks_message(0, 1)
    if s.startswith("0001-01-01T00:00:00.0000000"):
        return _ticks_message(0, -1)
    dt = datetime.strptime(m.group(1), "%Y-%m-%dT%H:%M:%S")
    frac = int((m.group(2) or "0").ljust(7, "0"))
    delta = dt - EPOCH
    ticks = (delta.days * 86400 + delta.seconds) * 10_000_000 + frac
    return _ticks_message(ticks)


def dec_timespan(b: bytes) -> float:
    t = _split_ticks(b)
    if t > MAX_TICKS:
        return float("inf")
    if t < -MAX_TICKS:
        return float("-inf")
    return t / 1e7


def enc_timespan(sec) -> bytes:
    if sec == float("inf"):
        return _ticks_message(0, 1)
    if sec == float("-inf"):
        return _ticks_message(0, -1)
    return _ticks_message(int(round(float(sec) * 1e7)))


# ---------------------------------------------------------------- type strings

def split_generic(s: str):
    """'Dictionary<int, List<Guid>>' -> ('Dictionary', ['int', 'List<Guid>'])."""
    s = s.strip()
    if "<" not in s:
        return s, []
    name, rest = s.split("<", 1)
    rest = rest[:-1]
    args, depth, cur = [], 0, ""
    for ch in rest:
        if ch == "<":
            depth += 1
        elif ch == ">":
            depth -= 1
        if ch == "," and depth == 0:
            args.append(cur.strip())
            cur = ""
        else:
            cur += ch
    args.append(cur.strip())
    return name, args


def classify(t: str):
    """Return (kind, inner...) for a type string: list/map/nullable/scalar name."""
    t = t.strip()
    if t == "ValueLogTypeHashSet":  # custom ICollection<ValueLogType>, written as repeated int32
        return "list", "int"
    if t == "byte[]":
        return "name", "byte[]"
    if t.endswith("[]"):
        return "list", t[:-2]
    if t.endswith("?"):
        return "nullable", t[:-1]
    name, args = split_generic(t)
    if name in ("List", "HashSet", "IList", "ICollection", "IEnumerable"):
        return "list", args[0]
    if name in ("Dictionary", "ConcurrentDictionary", "IDictionary"):
        return "map", args[0], args[1]
    if name in ("Tuple", "KeyValuePair"):
        return "tuple", args
    return "name", t


class Codec:
    def __init__(self, catalog: dict):
        self.catalog = catalog
        self.types = {t["name"]: t for t in catalog["types"]}
        self.parent = {}  # child name -> (parent name, include tag)
        for t in catalog["types"]:
            for inc in t.get("includes") or []:
                self.parent[inc["type"]] = (t["name"], inc["tag"])
        self.message_type = {m["name"]: m["message_type"] for m in catalog["messages"]}

    @classmethod
    def load(cls, path) -> "Codec":
        with open(path) as f:
            return cls(json.load(f))

    # ------------------------------------------------------------ resolution
    def resolve(self, name: str) -> dict:
        """Catalog entry for a (possibly nested 'Outer.Inner') type name."""
        short = name.split(".")[-1]
        if short not in self.types:
            raise CodecError("unknown type %r" % name)
        return self.types[short]

    def member_specs(self, tdef: dict):
        """Member list as (tag, name, type string, required, packed, wire_type, zigzag)."""
        out = []
        for m in tdef.get("members") or []:
            if m.get("map"):
                ts = "Dictionary<%s, %s>" % (m["map"]["key"], m["map"]["value"])
            elif m["repeated"] and m.get("element_type"):
                ts = "List<%s>" % m["element_type"]
            else:
                ts = m["type"]
            o = m.get("options") or {}
            out.append((m["tag"], m["name"], ts, bool(o.get("IsRequired")), bool(o.get("IsPacked")),
                        m.get("wire_type"), "ZigZag" in str(o.get("DataFormat", ""))))
        return sorted(out)

    # ------------------------------------------------------------ encode
    def encode(self, type_name: str, d: dict) -> bytes:
        concrete = d.get("$type", type_name)
        self.resolve(concrete)
        chain = [concrete]  # concrete first, then up to the root base class
        # protobuf-net always serializes from the root of the hierarchy, even when a member is declared as a
        # derived type (e.g. List<GenericDevice> is written as DeviceBase{100: GenericDevice fields}, base fields)
        while chain[-1] in self.parent:
            chain.append(self.parent[chain[-1]][0])
        if type_name.split(".")[-1] not in chain:
            raise CodecError("%s is not a subtype of %s" % (concrete, type_name))
        body = b""
        for level, tn in enumerate(chain):
            own = self._enc_members(tn, d)
            if level:
                tag = self.parent[chain[level - 1]][1]
                body = ld(tag, body) + own
            else:
                body = own
        return body

    def _enc_members(self, type_name: str, d: dict) -> bytes:
        tdef = self.resolve(type_name)
        out = b""
        for tag, name, ts, req, packed, wt, zz in self.member_specs(tdef):
            if d.get(name) is None:
                continue
            out += self._enc_field(tag, ts, d[name], req, packed, wt, zz)
        for u in d.get("$unknown", []):
            if u.get("in", type_name) == type_name:
                raw = bytes.fromhex(u["hex"])
                out += key(u["tag"], u["wt"]) + (varint(len(raw)) if u["wt"] == 2 else b"") + raw
        return out

    def _enc_field(self, tag, ts, v, force=False, packed=False, wt=None, zz=False):
        c = classify(ts)
        if c[0] == "nullable":
            return self._enc_field(tag, c[1], v, True, packed, wt, zz)
        if c[0] == "list":
            if packed and v:
                return ld(tag, b"".join(self._enc_scalar_payload(c[1], x, wt, zz) for x in v))
            return b"".join(self._enc_field(tag, c[1], x, True, False, wt, zz) for x in v)
        if c[0] == "map":
            out = b""
            for k, val in (v.items() if isinstance(v, dict) else v):
                entry = (self._enc_field(1, c[1], self._coerce(c[1], k), True)
                         + self._enc_field(2, c[2], val, True))
                out += ld(tag, entry)
            return out
        if c[0] == "tuple":
            entry = b"".join(self._enc_field(i + 1, a, x, True) for i, (a, x) in enumerate(zip(c[1], v)))
            return ld(tag, entry)
        return self._enc_named(tag, c[1], v, force, wt, zz)

    def _coerce(self, ts, k):
        if isinstance(k, str):
            base = ts.rstrip("?")
            if base in INT_TYPES or base in self.types and self.types[base]["kind"] == "enum":
                return int(k)
            if base == "bool":
                return k.lower() == "true"
        return k

    def _enc_scalar_payload(self, ts, v, wt, zz) -> bytes:
        """Payload without field header for a varint/fixed scalar (used by packed)."""
        if ts in ("double",):
            return struct.pack("<d", v)
        if ts == "float":
            return struct.pack("<f", v)
        if wt == "Fixed64":
            return struct.pack("<q" if ts == "long" else "<Q", v)
        if wt == "Fixed32":
            return struct.pack("<i" if ts == "int" else "<I", v)
        if ts == "bool":
            return varint(1 if v else 0)
        return varint(zigzag(int(v)) if zz else int(v))

    def _enc_named(self, tag, ts, v, force, wt, zz):
        if ts in INT_TYPES or (ts in self.types and self.types[ts]["kind"] == "enum") or ts == "bool" \
                or ts in ("double", "float"):
            if not force and v in (0, False, 0.0):
                return b""
            payload = self._enc_scalar_payload(ts if ts in INT_TYPES or ts in ("bool", "double", "float") else "int",
                                               v, wt, zz)
            if ts == "double" or wt == "Fixed64":
                return key(tag, 1) + payload
            if ts == "float" or wt == "Fixed32":
                return key(tag, 5) + payload
            return key(tag, 0) + payload
        if ts == "string":
            return ld(tag, v.encode("utf-8"))
        if ts == "byte[]":
            return ld(tag, bytes.fromhex(v) if isinstance(v, str) else bytes(v))
        if ts == "Guid":
            g = enc_guid(v)
            return b"" if (not g and not force) else ld(tag, g)
        if ts == "DateTime":
            return ld(tag, enc_datetime(v))
        if ts == "TimeSpan":
            return ld(tag, enc_timespan(v))
        return ld(tag, self.encode(ts, v))

    # ------------------------------------------------------------ decode
    def decode(self, type_name: str, data: bytes, _from_root: bool = True) -> dict:
        tdef = self.resolve(type_name)
        while _from_root and tdef["name"] in self.parent:  # the wire starts at the root base class
            tdef = self.resolve(self.parent[tdef["name"]][0])
        out = {"$type": tdef["name"]}
        specs = {s[0]: s for s in self.member_specs(tdef)}
        incs = {i["tag"]: i["type"] for i in tdef.get("includes") or []}
        unknown = []
        for tag, wt, v, raw in read_fields(data):
            if tag in incs and wt == 2:
                sub = self.decode(incs[tag], v, False)
                for k2, val in sub.items():
                    if k2 == "$unknown":
                        unknown += val
                    else:
                        out[k2] = val  # concrete "$type" wins; subtype fields merge
            elif tag in specs:
                _, name, ts, _, _, mwt, zz = specs[tag]
                try:
                    self._dec_field(out, name, ts, wt, v, mwt, zz)
                except (CodecError, ValueError, IndexError, TypeError, KeyError, OverflowError, struct.error) as e:
                    # keep going: one undecodable field must not hide the rest of a live message
                    out.setdefault("$errors", []).append(
                        {"field": name, "type": ts, "in": tdef["name"], "wt": wt, "error": str(e), "hex": raw.hex()})
            else:
                unknown.append({"tag": tag, "wt": wt, "hex": raw.hex(), "in": tdef["name"]})
        if unknown:
            out["$unknown"] = out.get("$unknown", []) + unknown
        return out

    def _dec_field(self, out, name, ts, wt, v, mwt, zz):
        c = classify(ts)
        if c[0] == "nullable":
            return self._dec_field(out, name, c[1], wt, v, mwt, zz)
        if c[0] == "list":
            lst = out.setdefault(name, [])
            if wt == 2 and self._is_scalar_numeric(c[1]):  # packed
                lst += self._dec_packed(c[1], v, mwt, zz)
            else:
                lst.append(self._dec_item(c[1], wt, v, mwt, zz))
        elif c[0] == "map":
            k, val = self._dec_entry(c, v)
            out.setdefault(name, {})[k] = val
        else:
            out[name] = self._dec_item(ts, wt, v, mwt, zz)

    def _is_scalar_numeric(self, ts):
        return ts in INT_TYPES or ts in ("bool", "double", "float") or \
            (ts in self.types and self.types[ts]["kind"] == "enum")

    def _dec_packed(self, ts, b, mwt, zz):
        out, i = [], 0
        if ts == "double" or mwt == "Fixed64":
            return [self._dec_num(ts, int.from_bytes(b[j:j + 8], "little"), 1, mwt, zz) for j in range(0, len(b), 8)]
        if ts == "float" or mwt == "Fixed32":
            return [self._dec_num(ts, int.from_bytes(b[j:j + 4], "little"), 5, mwt, zz) for j in range(0, len(b), 4)]
        while i < len(b):
            n, i = read_varint(b, i)
            out.append(self._dec_num(ts, n, 0, mwt, zz))
        return out

    def _dec_num(self, ts, n, wt, mwt, zz):
        if ts == "double":
            return struct.unpack("<d", n.to_bytes(8, "little"))[0]
        if ts == "float":
            return struct.unpack("<f", n.to_bytes(4, "little"))[0]
        if ts == "bool":
            return bool(n)
        bits = SIGNED.get(ts) or UNSIGNED.get(ts) or 32  # enums: int32
        if zz:
            return unzigzag(n)
        if wt == 1 or wt == 5:
            width = 64 if wt == 1 else 32
            if ts in SIGNED or ts not in UNSIGNED:
                return n - (1 << width) if n >> (width - 1) else n
            return n
        if ts in UNSIGNED:
            return n & ((1 << bits) - 1)
        if n >> 63:
            n -= 1 << 64
        return n

    def _dec_item(self, ts, wt, v, mwt, zz):
        c = classify(ts)
        if c[0] == "nullable":
            return self._dec_item(c[1], wt, v, mwt, zz)
        if c[0] == "tuple":
            vals = [None] * len(c[1])
            for tag, w2, v2, _ in read_fields(v):
                if 1 <= tag <= len(vals):
                    vals[tag - 1] = self._dec_item(c[1][tag - 1], w2, v2, None, False)
            return vals
        if c[0] != "name":
            raise CodecError("nested collection %r outside a map" % ts)
        ts = c[1]
        if self._is_scalar_numeric(ts):
            return self._dec_num(ts, v, wt, mwt, zz)
        if ts == "string":
            return v.decode("utf-8")
        if ts == "byte[]":
            return v.hex()
        if ts == "Guid":
            return dec_guid(v)
        if ts == "DateTime":
            return dec_datetime(v)
        if ts == "TimeSpan":
            return dec_timespan(v)
        return self.decode(ts, v)

    def _default(self, ts):
        c = classify(ts)
        if c[0] == "list":
            return []
        if c[0] == "map":
            return {}
        if c[0] == "nullable":
            return None
        ts = c[1]
        if ts == "string":
            return ""
        if ts == "bool":
            return False
        if ts in ("double", "float"):
            return 0.0
        if self._is_scalar_numeric(ts):
            return 0
        if ts == "Guid":
            return str(uuid.UUID(int=0))
        return None

    def _dec_entry(self, c, b):
        """Decode a map entry message {1: key, 2: value}; c = ('map', K, V)."""
        k, val = self._default(c[1]), self._default(c[2])
        vc = classify(c[2])
        for tag, wt, v, _ in read_fields(b):
            if tag == 1:
                k = self._dec_item(c[1], wt, v, None, False)
            elif tag == 2:
                if vc[0] == "list":
                    if wt == 2 and self._is_scalar_numeric(vc[1]):
                        val += self._dec_packed(vc[1], v, None, False)
                    else:
                        val.append(self._dec_item(vc[1], wt, v, None, False))
                elif vc[0] == "map":
                    k2, v2 = self._dec_entry(vc, v)
                    val[k2] = v2
                else:
                    val = self._dec_item(c[2], wt, v, None, False)
        return k, val

    # ------------------------------------------------------------ MessageBase helpers
    def encode_message(self, class_name: str, fields: dict, requests_id: int = 0) -> bytes:
        d = dict(fields)
        d["$type"] = class_name
        d["RequestsId"] = requests_id
        return self.encode("MessageBase", d)

    def decode_message(self, data: bytes) -> dict:
        return self.decode("MessageBase", data)

    def stamp(self, msg: dict) -> int:
        """Ack stamp: (MessageType << 32) | RequestsId, as a signed 64-bit value."""
        t = MESSAGE_TYPE[self.message_type.get(msg["$type"]) or "None"]
        return (t << 32) | msg.get("RequestsId", 0)


_default_codec = None


def _codec() -> Codec:
    global _default_codec
    if _default_codec is None:
        import os
        from pathlib import Path
        root = Path(__file__).resolve().parents[2]
        path = os.environ.get("TAPHOME_CATALOG", "work/catalog/official.json")
        p = Path(path)
        _default_codec = Codec.load(p if p.is_absolute() or p.exists() else root / p)
    return _default_codec


def encode(type_name, d):
    return _codec().encode(type_name, d)


def decode(type_name, data):
    return _codec().decode(type_name, data)


def encode_message(class_name, fields, requests_id=0):
    return _codec().encode_message(class_name, fields, requests_id)


def decode_message(data):
    return _codec().decode_message(data)
