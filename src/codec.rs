//! Catalog-driven protobuf-net codec for the TapHome wire format.
//!
//! A port of `scripts/probe/codec.py` that works on `serde_json::Value`. Value conventions (same as Python):
//!
//! * Integers stay exact JSON integers (i64, or u64 for unsigned values above i64::MAX); enums are integers.
//! * `byte[]`: lower-case hex string.
//! * Guid: canonical lower-case uuid string (bcl.proto Guid, two fixed64 halves of `Guid.ToByteArray()`).
//! * DateTime: ISO string `YYYY-MM-DDTHH:MM:SS.fffffff` (7 digits = .NET ticks), naive/UTC, bcl.proto DateTime
//!   as a TimeSpan since 1970-01-01. Min/Max map to year 0001 / 9999 strings.
//! * TimeSpan: seconds as a JSON float (exact value is ticks / 1e7).
//! * `Dictionary<K, V>`: JSON object. Keys are the JSON text of the key (`"5"`, `"true"`, a Guid string, or the
//!   compact JSON object of a message key such as `DeviceValueAggregation`). A list
//!   or map value is written as repeated field 2 inside the entry. A message value missing from the wire is null.
//! * Lists, arrays, HashSet: JSON array. Non-packed on write, except packed members; an empty packed list is
//!   written as a zero-length field (a live Core sends that). Both forms are accepted on read.
//! * Nullable (`T?`): written whenever the key is present and not null.
//! * Inheritance: decode returns one flat object with `"$type"` naming the concrete class; encode takes the concrete
//!   class from `"$type"` (default: the type given). The catalog guarantees unique member names per chain.
//! * Unknown fields: kept in `"$unknown"` as `{"tag", "wt", "hex", "in"}` and written back on encode.
//! * A field that fails to decode is reported in `"$errors"` and the rest of the message is still decoded.
//!
//! Deliberate differences from the Python codec:
//!
//! * JSON has no NaN or infinity. A non-finite double, float or TimeSpan is the string `"NaN"`, `"Infinity"` or
//!   `"-Infinity"` (TimeSpan.MaxValue / MinValue are `"Infinity"` / `"-Infinity"`). Encode accepts those strings.
//! * Encode is strict: a key that is not a member of the type chain (other than `$type`, `$unknown`, `$errors`)
//!   is an error, and values must have the member's JSON type (integer, bool, number, string, object, array).
//!   An integer outside its declared type's range (`int` i32, `short` i16, `sbyte` i8, `long` i64, `byte` u8,
//!   `ushort` and `char` u16, `uint` u32, `ulong` u64, enums i32) is an error. So are a tuple array longer than
//!   the tuple, an `$unknown` entry whose `"in"` is not in the type chain, and a non-empty `$errors` (the value
//!   was decoded lossily, so re-encoding would drop data).
//! * Decode never panics on corrupt data: lengths use checked arithmetic, a varint is at most 10 bytes and
//!   messages nest at most 100 levels deep. A field that exceeds the limit lands in `$errors`.
//! * An `$unknown` entry without `"in"` is written once, at the concrete class level.
//! * `$errors` messages are Rust texts, not Python exception texts.

use std::fmt;

use serde_json::{Map, Number, Value};

use crate::catalog::{catalog, Catalog, Member, TypeDef, TypeExpr, Wire};

/// A codec failure: unknown type, malformed wire data, or a value that does not fit its member.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodecError {
    pub message: String,
}

impl fmt::Display for CodecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for CodecError {}

type Result<T> = std::result::Result<T, CodecError>;

fn err<T>(message: impl Into<String>) -> Result<T> {
    Err(CodecError {
        message: message.into(),
    })
}

const TICKS_PER_SCALE: [i128; 6] = [
    864_000_000_000,
    36_000_000_000,
    600_000_000,
    10_000_000,
    10_000,
    1,
];
const MAX_TICKS: i128 = 3_155_378_975_999_999_999; // DateTime.MaxValue.Ticks
const TICKS_PER_SECOND: i128 = 10_000_000;
const MESSAGE_TYPES: [&str; 8] = [
    "None",
    "Request",
    "Response",
    "Broadcast",
    "Stream",
    "Acknowledgement",
    "Container",
    "Progress",
];

fn signed_bits(ts: &str) -> Option<u32> {
    match ts {
        "int" => Some(32),
        "short" => Some(16),
        "long" => Some(64),
        "sbyte" => Some(8),
        _ => None,
    }
}

fn unsigned_bits(ts: &str) -> Option<u32> {
    match ts {
        "uint" => Some(32),
        "ushort" => Some(16),
        "ulong" => Some(64),
        "byte" => Some(8),
        "char" => Some(16),
        _ => None,
    }
}

fn is_int_type(ts: &str) -> bool {
    signed_bits(ts).is_some() || unsigned_bits(ts).is_some()
}

// ---------------------------------------------------------------- low-level wire helpers

fn put_varint(out: &mut Vec<u8>, mut n: u64) {
    while n > 0x7F {
        out.push((n as u8 & 0x7F) | 0x80);
        n >>= 7;
    }
    out.push(n as u8);
}

fn read_varint(b: &[u8], mut i: usize) -> Result<(u64, usize)> {
    let (mut n, mut shift) = (0u64, 0u32);
    loop {
        let Some(&c) = b.get(i) else {
            return err("truncated varint");
        };
        i += 1;
        if shift < 64 {
            n |= u64::from(c & 0x7F) << shift;
        }
        if c & 0x80 == 0 {
            return Ok((n, i));
        }
        shift += 7;
        if shift >= 70 {
            return err("varint is longer than 10 bytes");
        }
    }
}

fn zigzag(n: i64) -> u64 {
    ((n << 1) ^ (n >> 63)) as u64
}

fn unzigzag(n: u64) -> i64 {
    ((n >> 1) as i64) ^ -((n & 1) as i64)
}

fn put_key(out: &mut Vec<u8>, tag: u64, wt: u8) {
    put_varint(out, (tag << 3) | u64::from(wt));
}

fn put_ld(out: &mut Vec<u8>, tag: u64, payload: &[u8]) {
    put_key(out, tag, 2);
    put_varint(out, payload.len() as u64);
    out.extend_from_slice(payload);
}

fn put_vfield(out: &mut Vec<u8>, tag: u64, n: u64) {
    put_key(out, tag, 0);
    put_varint(out, n);
}

#[derive(Clone, Copy)]
enum FieldValue<'a> {
    Int(u64),
    Bytes(&'a [u8]),
}

struct Field<'a> {
    tag: u64,
    wt: u8,
    value: FieldValue<'a>,
    raw: &'a [u8],
}

fn read_fields(b: &[u8]) -> Result<Vec<Field<'_>>> {
    let (mut out, mut i) = (Vec::new(), 0usize);
    while i < b.len() {
        let (k, j) = read_varint(b, i)?;
        i = j;
        let (tag, wt) = (k >> 3, (k & 7) as u8);
        match wt {
            0 => {
                let (v, j) = read_varint(b, i)?;
                out.push(Field {
                    tag,
                    wt,
                    value: FieldValue::Int(v),
                    raw: &b[i..j],
                });
                i = j;
            }
            1 | 5 => {
                let n = if wt == 1 { 8 } else { 4 };
                if n > b.len() - i {
                    return err("truncated fixed field");
                }
                let raw = &b[i..i + n];
                let mut buf = [0u8; 8];
                buf[..n].copy_from_slice(raw);
                out.push(Field {
                    tag,
                    wt,
                    value: FieldValue::Int(u64::from_le_bytes(buf)),
                    raw,
                });
                i += n;
            }
            2 => {
                let (n, j) = read_varint(b, i)?;
                i = j;
                let n = usize::try_from(n).ok().filter(|n| *n <= b.len() - i);
                let Some(n) = n else {
                    return err("truncated length-delimited field");
                };
                let raw = &b[i..i + n];
                out.push(Field {
                    tag,
                    wt,
                    value: FieldValue::Bytes(raw),
                    raw,
                });
                i += n;
            }
            _ => return err(format!("unsupported wire type {wt} (tag {tag})")),
        }
    }
    Ok(out)
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn unhex(s: &str) -> Result<Vec<u8>> {
    if !s.len().is_multiple_of(2) || !s.bytes().all(|c| c.is_ascii_hexdigit()) {
        return err(format!("bad hex string {s:?}"));
    }
    Ok((0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect())
}

// ---------------------------------------------------------------- bcl types

fn split_ticks(b: &[u8]) -> Result<i128> {
    let (mut value, mut scale) = (0i64, 0u64);
    for f in read_fields(b)? {
        match (f.tag, f.value) {
            (1, FieldValue::Int(v)) => value = unzigzag(v),
            (2, FieldValue::Int(v)) => scale = v,
            _ => {}
        }
    }
    if scale == 15 {
        return Ok(if value > 0 {
            MAX_TICKS + 1
        } else {
            -(MAX_TICKS + 1)
        });
    }
    match TICKS_PER_SCALE.get(scale as usize) {
        Some(t) => Ok(i128::from(value) * t),
        None => err(format!("unknown TimeSpan scale {scale}")),
    }
}

fn ticks_message(ticks: i128, minmax: i64) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    if minmax != 0 {
        put_vfield(&mut out, 1, zigzag(minmax));
        put_vfield(&mut out, 2, 15);
        return Ok(out);
    }
    let scale = (0..5)
        .find(|&s| ticks % TICKS_PER_SCALE[s] == 0)
        .unwrap_or(5);
    let Ok(value) = i64::try_from(ticks / TICKS_PER_SCALE[scale]) else {
        return err("TimeSpan out of range");
    };
    if value != 0 {
        put_vfield(&mut out, 1, zigzag(value));
    }
    if scale != 0 {
        put_vfield(&mut out, 2, scale as u64);
    }
    Ok(out)
}

fn parse_guid(s: &str) -> Result<[u8; 16]> {
    let h: String = s
        .trim_matches(|c| c == '{' || c == '}')
        .chars()
        .filter(|&c| c != '-')
        .collect();
    if h.len() != 32 {
        return err(format!("bad Guid {s:?}"));
    }
    let b = unhex(&h)?;
    // .NET byte order (Python uuid.bytes_le): the first three groups are little-endian
    let mut le = [0u8; 16];
    le[..4].copy_from_slice(&[b[3], b[2], b[1], b[0]]);
    le[4..6].copy_from_slice(&[b[5], b[4]]);
    le[6..8].copy_from_slice(&[b[7], b[6]]);
    le[8..].copy_from_slice(&b[8..]);
    Ok(le)
}

fn format_guid(le: &[u8; 16]) -> String {
    let b = [le[3], le[2], le[1], le[0], le[5], le[4], le[7], le[6]];
    let (a, rest) = (hex(&b), hex(&le[8..]));
    format!(
        "{}-{}-{}-{}-{}",
        &a[..8],
        &a[8..12],
        &a[12..16],
        &rest[..4],
        &rest[4..]
    )
}

fn enc_guid(s: &str) -> Result<Vec<u8>> {
    let le = parse_guid(s)?;
    if le == [0u8; 16] {
        return Ok(Vec::new());
    }
    let mut out = vec![0x09];
    out.extend_from_slice(&le[..8]);
    out.push(0x11);
    out.extend_from_slice(&le[8..]);
    Ok(out)
}

fn dec_guid(b: &[u8]) -> Result<String> {
    let mut le = [0u8; 16];
    for f in read_fields(b)? {
        let half = match f.tag {
            1 => 0,
            2 => 8,
            _ => continue,
        };
        if f.raw.len() != 8 {
            return err("Guid half is not fixed64");
        }
        le[half..half + 8].copy_from_slice(f.raw);
    }
    Ok(format_guid(&le))
}

// Days from 1970-01-01 to a civil date and back (Howard Hinnant's algorithms).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

fn dec_datetime(b: &[u8]) -> Result<String> {
    let t = split_ticks(b)?;
    if t > MAX_TICKS {
        return Ok("9999-12-31T23:59:59.9999999".into());
    }
    if t < -MAX_TICKS {
        return Ok("0001-01-01T00:00:00.0000000".into());
    }
    let (sec, frac) = (
        t.div_euclid(TICKS_PER_SECOND),
        t.rem_euclid(TICKS_PER_SECOND),
    );
    let (days, sod) = (sec.div_euclid(86_400) as i64, sec.rem_euclid(86_400) as i64);
    let (y, m, d) = civil_from_days(days);
    if !(1..=9999).contains(&y) {
        return err("DateTime out of range");
    }
    Ok(format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{frac:07}",
        sod / 3600,
        sod / 60 % 60,
        sod % 60
    ))
}

fn enc_datetime(s: &str) -> Result<Vec<u8>> {
    let bad = || CodecError {
        message: format!("bad DateTime {s:?}"),
    };
    let body = s.strip_suffix('Z').unwrap_or(s);
    let (main, frac) = match body.split_once('.') {
        Some((m, f)) if (1..=7).contains(&f.len()) && f.bytes().all(|c| c.is_ascii_digit()) => {
            (m, f)
        }
        Some(_) => return Err(bad()),
        None => (body, ""),
    };
    let b = main.as_bytes();
    let pattern_ok = b.len() == 19
        && b.iter().enumerate().all(|(i, c)| match i {
            4 | 7 => *c == b'-',
            10 => *c == b'T',
            13 | 16 => *c == b':',
            _ => c.is_ascii_digit(),
        });
    if !pattern_ok {
        return Err(bad());
    }
    if s.starts_with("9999-12-31T23:59:59.9999999") {
        return ticks_message(0, 1);
    }
    if s.starts_with("0001-01-01T00:00:00.0000000") {
        return ticks_message(0, -1);
    }
    let num = |r: std::ops::Range<usize>| main[r].parse::<i64>().unwrap();
    let (y, mo, d, h, mi, se) = (
        num(0..4),
        num(5..7),
        num(8..10),
        num(11..13),
        num(14..16),
        num(17..19),
    );
    let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    let mdays = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    if y < 1
        || !(1..=12).contains(&mo)
        || d < 1
        || d > mdays[(mo - 1) as usize]
        || h > 23
        || mi > 59
        || se > 59
    {
        return Err(bad());
    }
    let frac = if frac.is_empty() {
        0
    } else {
        format!("{frac:0<7}").parse::<i128>().unwrap()
    };
    let secs = i128::from(days_from_civil(y, mo, d)) * 86_400 + i128::from(h * 3600 + mi * 60 + se);
    ticks_message(secs * TICKS_PER_SECOND + frac, 0)
}

fn float_value(f: f64) -> Value {
    match Number::from_f64(f) {
        Some(n) => Value::Number(n),
        None if f.is_nan() => Value::String("NaN".into()),
        None if f > 0.0 => Value::String("Infinity".into()),
        None => Value::String("-Infinity".into()),
    }
}

fn as_float(v: &Value) -> Result<f64> {
    match v {
        Value::Number(n) => Ok(n.as_f64().unwrap_or(f64::NAN)),
        Value::String(s) if s == "NaN" => Ok(f64::NAN),
        Value::String(s) if s == "Infinity" => Ok(f64::INFINITY),
        Value::String(s) if s == "-Infinity" => Ok(f64::NEG_INFINITY),
        _ => err(format!("expected a number, got {v}")),
    }
}

fn dec_timespan(b: &[u8]) -> Result<Value> {
    let t = split_ticks(b)?;
    if t > MAX_TICKS {
        return Ok(float_value(f64::INFINITY));
    }
    if t < -MAX_TICKS {
        return Ok(float_value(f64::NEG_INFINITY));
    }
    Ok(float_value(t as f64 / 1e7))
}

fn enc_timespan(v: &Value) -> Result<Vec<u8>> {
    let sec = as_float(v)?;
    if sec == f64::INFINITY {
        return ticks_message(0, 1);
    }
    if sec == f64::NEG_INFINITY {
        return ticks_message(0, -1);
    }
    if sec.is_nan() {
        return err("TimeSpan is NaN");
    }
    // Python round() rounds half to even
    ticks_message((sec * 1e7).round_ties_even() as i128, 0)
}

// ---------------------------------------------------------------- the codec

/// A codec bound to one catalog. The module-level functions use the embedded catalog.
pub struct Codec<'a> {
    cat: &'a Catalog,
    /// Current message nesting while decoding, bounded by `MAX_DEPTH`.
    depth: std::cell::Cell<usize>,
}

/// Deepest message nesting the decoder accepts; real configs stay far below it.
const MAX_DEPTH: usize = 100;

/// The range of an integer type, enums count as `int`.
fn int_range(ts: &str) -> (i128, i128) {
    match (unsigned_bits(ts), signed_bits(ts)) {
        (Some(b), _) => (0, (1i128 << b) - 1),
        (_, Some(b)) => (-(1i128 << (b - 1)), (1i128 << (b - 1)) - 1),
        _ => (i128::from(i32::MIN), i128::from(i32::MAX)),
    }
}

fn as_int(v: &Value) -> Result<i128> {
    match v {
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Ok(i128::from(i))
            } else if let Some(u) = n.as_u64() {
                Ok(i128::from(u))
            } else {
                err(format!("expected an integer, got {n}"))
            }
        }
        _ => err(format!("expected an integer, got {v}")),
    }
}

/// JSON object key for a decoded map key.
fn key_string(k: &Value) -> Result<String> {
    match k {
        Value::String(s) => Ok(s.clone()),
        Value::Number(_) | Value::Bool(_) | Value::Null => Ok(k.to_string()),
        // a message key (e.g. DeviceValueAggregation) is its decoded object as compact JSON text
        Value::Object(_) => Ok(k.to_string()),
        _ => err("map key is not a scalar"),
    }
}

impl<'a> Codec<'a> {
    pub fn new(cat: &'a Catalog) -> Self {
        Codec {
            cat,
            depth: std::cell::Cell::new(0),
        }
    }

    fn resolve(&self, name: &str) -> Result<&'a TypeDef> {
        match self.cat.resolve(name) {
            Some(t) => Ok(t),
            None => err(format!("unknown type {name:?}")),
        }
    }

    fn is_scalar_numeric(&self, ts: &str) -> bool {
        is_int_type(ts) || matches!(ts, "bool" | "double" | "float") || self.cat.is_enum(ts)
    }

    // ------------------------------------------------------------ encode

    pub fn encode(&self, type_name: &str, value: &Value) -> Result<Vec<u8>> {
        let Value::Object(d) = value else {
            return err(format!("{type_name}: expected an object, got {value}"));
        };
        let concrete = match d.get("$type") {
            None => type_name,
            Some(Value::String(s)) => s.as_str(),
            Some(other) => return err(format!("$type must be a string, got {other}")),
        };
        let concrete = self.resolve(concrete)?.name.as_str();
        // protobuf-net always serializes from the root of the hierarchy, even when a member is declared as a
        // derived type (a List<GenericDevice> is written as DeviceBase{100: GenericDevice fields}, base fields)
        let mut chain = vec![concrete];
        while let Some((p, _)) = self.cat.parent.get(*chain.last().unwrap()) {
            chain.push(p);
        }
        let wanted = self.resolve(type_name)?.name.as_str();
        if !chain.contains(&wanted) {
            return err(format!("{concrete} is not a subtype of {type_name}"));
        }
        for k in d.keys() {
            let known = matches!(k.as_str(), "$type" | "$unknown" | "$errors")
                || chain
                    .iter()
                    .any(|t| self.cat.types[*t].members.iter().any(|m| &m.name == k));
            if !known {
                return err(format!("{concrete} has no member {k:?}"));
            }
        }
        match d.get("$errors") {
            None | Some(Value::Null) => {}
            Some(Value::Array(a)) if a.is_empty() => {}
            Some(_) => {
                return err(format!(
                    "{concrete} carries $errors from a lossy decode; re-encoding would drop data"
                ))
            }
        }
        let unknown = match d.get("$unknown") {
            None | Some(Value::Null) => &[][..],
            Some(Value::Array(a)) => a.as_slice(),
            Some(_) => return err("$unknown must be a list"),
        };
        for u in unknown {
            if let Some(t) = u.get("in").and_then(Value::as_str) {
                if !chain.contains(&t) {
                    return err(format!(
                        "$unknown entry is in {t}, which is not in the chain of {concrete}"
                    ));
                }
            }
        }
        let mut body = Vec::new();
        for (level, tn) in chain.iter().enumerate() {
            let own = self.enc_members(tn, d, unknown, *tn == concrete)?;
            if level == 0 {
                body = own;
            } else {
                let tag = self.cat.parent[chain[level - 1]].1;
                let mut next = Vec::new();
                put_ld(&mut next, u64::from(tag), &body);
                next.extend_from_slice(&own);
                body = next;
            }
        }
        Ok(body)
    }

    fn enc_members(
        &self,
        tn: &str,
        d: &Map<String, Value>,
        unknown: &[Value],
        concrete: bool,
    ) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        for m in &self.cat.types[tn].members {
            match d.get(&m.name) {
                None | Some(Value::Null) => {}
                Some(v) => self
                    .enc_field(
                        &mut out,
                        u64::from(m.tag),
                        &m.ty,
                        v,
                        m.required,
                        m.packed,
                        m.wire,
                        m.zigzag,
                    )
                    .map_err(|e| CodecError {
                        message: format!("{tn}.{}: {}", m.name, e.message),
                    })?,
            }
        }
        for u in unknown {
            let in_type = u.get("in").and_then(Value::as_str);
            if in_type.map_or(!concrete, |t| t != tn) {
                continue;
            }
            let (Some(tag), Some(wt), Some(h)) = (
                u.get("tag").and_then(Value::as_u64),
                u.get("wt").and_then(Value::as_u64),
                u.get("hex").and_then(Value::as_str),
            ) else {
                return err(format!("bad $unknown entry {u}"));
            };
            let raw = unhex(h)?;
            put_key(&mut out, tag, wt as u8);
            if wt == 2 {
                put_varint(&mut out, raw.len() as u64);
            }
            out.extend_from_slice(&raw);
        }
        Ok(out)
    }

    #[allow(clippy::too_many_arguments)]
    fn enc_field(
        &self,
        out: &mut Vec<u8>,
        tag: u64,
        ty: &TypeExpr,
        v: &Value,
        force: bool,
        packed: bool,
        wire: Wire,
        zz: bool,
    ) -> Result<()> {
        match ty {
            TypeExpr::Nullable(inner) => self.enc_field(out, tag, inner, v, true, packed, wire, zz),
            TypeExpr::List(inner) => {
                let Value::Array(items) = v else {
                    return err(format!("expected a list, got {v}"));
                };
                if packed {
                    let TypeExpr::Name(ts) = inner.as_ref() else {
                        return err("packed list of a collection");
                    };
                    let mut payload = Vec::new();
                    for x in items {
                        self.enc_scalar_payload(&mut payload, ts, x, wire, zz)?;
                    }
                    put_ld(out, tag, &payload);
                    return Ok(());
                }
                if matches!(inner.as_ref(), TypeExpr::List(_) | TypeExpr::Map(..)) {
                    return err("nested collection outside a map");
                }
                for x in items {
                    self.enc_field(out, tag, inner, x, true, false, wire, zz)?;
                }
                Ok(())
            }
            TypeExpr::Map(kt, vt) => {
                let Value::Object(entries) = v else {
                    return err(format!("expected an object (map), got {v}"));
                };
                for (k, val) in entries {
                    let mut entry = Vec::new();
                    self.enc_field(
                        &mut entry,
                        1,
                        kt,
                        &self.coerce_key(kt, k)?,
                        true,
                        false,
                        Wire::Default,
                        false,
                    )?;
                    if !val.is_null() {
                        // a null message value is an entry without field 2, as decode returns it
                        self.enc_field(&mut entry, 2, vt, val, true, false, Wire::Default, false)?;
                    }
                    put_ld(out, tag, &entry);
                }
                Ok(())
            }
            TypeExpr::Tuple(args) => {
                let Value::Array(items) = v else {
                    return err(format!("expected a list (tuple), got {v}"));
                };
                if items.len() > args.len() {
                    return err(format!(
                        "tuple has {} items, the type takes {}",
                        items.len(),
                        args.len()
                    ));
                }
                let mut entry = Vec::new();
                for (i, (a, x)) in args.iter().zip(items).enumerate() {
                    if !x.is_null() {
                        self.enc_field(
                            &mut entry,
                            i as u64 + 1,
                            a,
                            x,
                            true,
                            false,
                            Wire::Default,
                            false,
                        )?;
                    }
                }
                put_ld(out, tag, &entry);
                Ok(())
            }
            TypeExpr::Name(ts) => self.enc_named(out, tag, ts, v, force, wire, zz),
        }
    }

    /// Map keys are JSON object keys (strings); turn them back into the key type's value.
    fn coerce_key(&self, kt: &TypeExpr, k: &str) -> Result<Value> {
        let base = match kt {
            TypeExpr::Name(n) => n.as_str(),
            TypeExpr::Nullable(inner) => match inner.as_ref() {
                TypeExpr::Name(n) => n.as_str(),
                _ => return err("unsupported map key type"),
            },
            _ => return err("unsupported map key type"),
        };
        if is_int_type(base) || self.cat.is_enum(base) {
            return match k.trim().parse::<i64>() {
                Ok(i) => Ok(Value::from(i)),
                Err(_) => k
                    .trim()
                    .parse::<u64>()
                    .map(Value::from)
                    .or_else(|_| err(format!("bad integer key {k:?}"))),
            };
        }
        match base {
            "bool" => Ok(Value::Bool(k.eq_ignore_ascii_case("true"))),
            "double" | "float" => k
                .parse::<f64>()
                .map(float_value)
                .or_else(|_| as_float(&Value::String(k.into())).map(float_value)),
            "string" | "Guid" | "DateTime" | "TimeSpan" | "byte[]" => Ok(Value::String(k.into())),
            _ if self.cat.resolve(base).is_some() => match serde_json::from_str::<Value>(k) {
                Ok(obj @ Value::Object(_)) => Ok(obj),
                _ => err(format!(
                    "map key of type {base} must be the JSON text of an object, got {k:?}"
                )),
            },
            _ => err(format!("unsupported map key type {base}")),
        }
    }

    fn enc_scalar_payload(
        &self,
        out: &mut Vec<u8>,
        ts: &str,
        v: &Value,
        wire: Wire,
        zz: bool,
    ) -> Result<()> {
        if ts == "double" {
            out.extend_from_slice(&as_float(v)?.to_le_bytes());
            return Ok(());
        }
        if ts == "float" {
            out.extend_from_slice(&(as_float(v)? as f32).to_le_bytes());
            return Ok(());
        }
        if ts == "bool" {
            let Value::Bool(b) = v else {
                return err(format!("expected a bool, got {v}"));
            };
            put_varint(out, u64::from(*b));
            return Ok(());
        }
        let n = as_int(v)?;
        let (lo, hi) = int_range(ts);
        if n < lo || n > hi {
            return err(format!("{n} is out of range for {ts} ({lo} to {hi})"));
        }
        match wire {
            Wire::Fixed64 => {
                let ok = if ts == "long" {
                    i64::try_from(n).is_ok()
                } else {
                    u64::try_from(n).is_ok()
                };
                if !ok {
                    return err(format!("{n} does not fit fixed64 {ts}"));
                }
                out.extend_from_slice(&(n as u64).to_le_bytes());
            }
            Wire::Fixed32 => {
                let ok = if ts == "int" {
                    i32::try_from(n).is_ok()
                } else {
                    u32::try_from(n).is_ok()
                };
                if !ok {
                    return err(format!("{n} does not fit fixed32 {ts}"));
                }
                out.extend_from_slice(&(n as u32).to_le_bytes());
            }
            Wire::Default if zz => {
                let Ok(i) = i64::try_from(n) else {
                    return err(format!("{n} does not fit zigzag int64"));
                };
                put_varint(out, zigzag(i));
            }
            Wire::Default => put_varint(out, n as u64), // negative values are sign-extended to 64 bits
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn enc_named(
        &self,
        out: &mut Vec<u8>,
        tag: u64,
        ts: &str,
        v: &Value,
        force: bool,
        wire: Wire,
        zz: bool,
    ) -> Result<()> {
        if self.is_scalar_numeric(ts) {
            let zero = match v {
                Value::Bool(b) => !b,
                Value::Number(n) => n.as_f64() == Some(0.0),
                _ => false,
            };
            if !force && zero {
                return Ok(());
            }
            let pts = if is_int_type(ts) || matches!(ts, "bool" | "double" | "float") {
                ts
            } else {
                "int"
            };
            let mut payload = Vec::new();
            self.enc_scalar_payload(&mut payload, pts, v, wire, zz)?;
            let wt = if ts == "double" || wire == Wire::Fixed64 {
                1
            } else if ts == "float" || wire == Wire::Fixed32 {
                5
            } else {
                0
            };
            put_key(out, tag, wt);
            out.extend_from_slice(&payload);
            return Ok(());
        }
        let text = |v: &Value| match v {
            Value::String(s) => Ok(s.clone()),
            _ => err(format!("expected a string for {ts}, got {v}")),
        };
        match ts {
            "string" => put_ld(out, tag, text(v)?.as_bytes()),
            "byte[]" => put_ld(out, tag, &unhex(&text(v)?)?),
            "Guid" => {
                let g = enc_guid(&text(v)?)?;
                if !g.is_empty() || force {
                    put_ld(out, tag, &g);
                }
            }
            "DateTime" => put_ld(out, tag, &enc_datetime(&text(v)?)?),
            "TimeSpan" => put_ld(out, tag, &enc_timespan(v)?),
            _ => put_ld(out, tag, &self.encode(ts, v)?),
        }
        Ok(())
    }

    // ------------------------------------------------------------ decode

    pub fn decode(&self, type_name: &str, data: &[u8]) -> Result<Value> {
        self.decode_at(type_name, data, true)
    }

    fn decode_at(&self, type_name: &str, data: &[u8], from_root: bool) -> Result<Value> {
        if self.depth.get() >= MAX_DEPTH {
            return err(format!(
                "messages are nested deeper than {MAX_DEPTH} levels"
            ));
        }
        self.depth.set(self.depth.get() + 1);
        let result = self.decode_level(type_name, data, from_root);
        self.depth.set(self.depth.get() - 1);
        result
    }

    fn decode_level(&self, type_name: &str, data: &[u8], from_root: bool) -> Result<Value> {
        let mut tdef = self.resolve(type_name)?;
        if from_root {
            // the wire starts at the root base class
            while let Some((p, _)) = self.cat.parent.get(&tdef.name) {
                tdef = self.resolve(p)?;
            }
        }
        let mut out = Map::new();
        out.insert("$type".into(), Value::String(tdef.name.clone()));
        let mut unknown = Vec::new();
        for f in read_fields(data)? {
            let include = tdef.includes.iter().find(|(t, _)| u64::from(*t) == f.tag);
            let member = tdef.members.iter().find(|m| u64::from(m.tag) == f.tag);
            match (include, member, f.value) {
                (Some((_, sub)), _, FieldValue::Bytes(b)) => {
                    let Value::Object(sub) = self.decode_at(sub, b, false)? else { unreachable!() };
                    for (k, val) in sub {
                        if k == "$unknown" {
                            if let Value::Array(a) = val {
                                unknown.extend(a);
                            }
                        } else {
                            out.insert(k, val); // concrete "$type" wins; subtype fields merge
                        }
                    }
                }
                (_, Some(m), _) => {
                    if let Err(e) = self.dec_field(&mut out, m, f.wt, f.value) {
                        // keep going: one undecodable field must not hide the rest of a live message
                        let entry = serde_json::json!({"field": m.name, "type": m.type_name, "in": tdef.name,
                            "wt": f.wt, "error": e.message, "hex": hex(f.raw)});
                        match out.entry("$errors").or_insert_with(|| Value::Array(Vec::new())) {
                            Value::Array(a) => a.push(entry),
                            other => *other = Value::Array(vec![entry]),
                        }
                    }
                }
                _ => unknown.push(serde_json::json!({"tag": f.tag, "wt": f.wt, "hex": hex(f.raw), "in": tdef.name})),
            }
        }
        if !unknown.is_empty() {
            match out
                .entry("$unknown")
                .or_insert_with(|| Value::Array(Vec::new()))
            {
                Value::Array(a) => a.extend(unknown),
                other => *other = Value::Array(unknown),
            }
        }
        Ok(Value::Object(out))
    }

    fn dec_field(
        &self,
        out: &mut Map<String, Value>,
        m: &Member,
        wt: u8,
        v: FieldValue,
    ) -> Result<()> {
        let mut ty = &m.ty;
        while let TypeExpr::Nullable(inner) = ty {
            ty = inner;
        }
        match ty {
            TypeExpr::List(inner) => {
                let slot = out
                    .entry(m.name.clone())
                    .or_insert_with(|| Value::Array(Vec::new()));
                let Value::Array(lst) = slot else {
                    return err("list member decoded twice with different shapes");
                };
                match (inner.as_ref(), v) {
                    (TypeExpr::Name(ts), FieldValue::Bytes(b))
                        if wt == 2 && self.is_scalar_numeric(ts) =>
                    {
                        lst.extend(self.dec_packed(ts, b, m.wire, m.zigzag)?);
                    }
                    _ => {
                        let item = self.dec_item(inner, wt, v, m.zigzag)?;
                        let Some(Value::Array(lst)) = out.get_mut(&m.name) else {
                            unreachable!()
                        };
                        lst.push(item);
                    }
                }
            }
            TypeExpr::Map(kt, vt) => {
                let FieldValue::Bytes(b) = v else {
                    return err("map entry is not length-delimited");
                };
                let (k, val) = self.dec_entry(kt, vt, b)?;
                let slot = out
                    .entry(m.name.clone())
                    .or_insert_with(|| Value::Object(Map::new()));
                let Value::Object(map) = slot else {
                    return err("map member decoded twice with different shapes");
                };
                map.insert(key_string(&k)?, val);
            }
            _ => {
                let item = self.dec_item(ty, wt, v, m.zigzag)?;
                out.insert(m.name.clone(), item);
            }
        }
        Ok(())
    }

    fn dec_packed(&self, ts: &str, b: &[u8], wire: Wire, zz: bool) -> Result<Vec<Value>> {
        let width = if ts == "double" || wire == Wire::Fixed64 {
            8
        } else if ts == "float" || wire == Wire::Fixed32 {
            4
        } else {
            0
        };
        if width > 0 {
            if !b.len().is_multiple_of(width) {
                return err("packed fixed field has a partial element");
            }
            let wt = if width == 8 { 1 } else { 5 };
            return b
                .chunks(width)
                .map(|c| {
                    let mut buf = [0u8; 8];
                    buf[..width].copy_from_slice(c);
                    self.dec_num(ts, u64::from_le_bytes(buf), wt, zz)
                })
                .collect();
        }
        let (mut out, mut i) = (Vec::new(), 0);
        while i < b.len() {
            let (n, j) = read_varint(b, i)?;
            i = j;
            out.push(self.dec_num(ts, n, 0, zz)?);
        }
        Ok(out)
    }

    fn dec_num(&self, ts: &str, n: u64, wt: u8, zz: bool) -> Result<Value> {
        match ts {
            "double" => return Ok(float_value(f64::from_bits(n))),
            "float" => match u32::try_from(n) {
                Ok(bits) => return Ok(float_value(f64::from(f32::from_bits(bits)))),
                Err(_) => return err("float value does not fit 32 bits"),
            },
            "bool" => return Ok(Value::Bool(n != 0)),
            _ => {}
        }
        if zz {
            return Ok(Value::from(unzigzag(n)));
        }
        let unsigned = unsigned_bits(ts);
        if wt == 1 || wt == 5 {
            if unsigned.is_some() {
                return Ok(Value::from(n));
            }
            return Ok(Value::from(if wt == 1 {
                n as i64
            } else {
                i64::from(n as u32 as i32)
            }));
        }
        if let Some(bits) = unsigned {
            return Ok(Value::from(if bits == 64 {
                n
            } else {
                n & ((1u64 << bits) - 1)
            }));
        }
        Ok(Value::from(n as i64)) // signed and enums: sign-extended 64-bit varint
    }

    fn dec_item(&self, ty: &TypeExpr, wt: u8, v: FieldValue, zz: bool) -> Result<Value> {
        match ty {
            TypeExpr::Nullable(inner) => self.dec_item(inner, wt, v, zz),
            TypeExpr::Tuple(args) => {
                let FieldValue::Bytes(b) = v else {
                    return err("tuple is not length-delimited");
                };
                let mut vals = vec![Value::Null; args.len()];
                for f in read_fields(b)? {
                    if f.tag >= 1 && (f.tag as usize) <= args.len() {
                        let i = f.tag as usize - 1;
                        vals[i] = self.dec_item(&args[i], f.wt, f.value, false)?;
                    }
                }
                Ok(Value::Array(vals))
            }
            TypeExpr::List(_) | TypeExpr::Map(..) => err("nested collection outside a map"),
            TypeExpr::Name(ts) => {
                if self.is_scalar_numeric(ts) {
                    let FieldValue::Int(n) = v else {
                        return err(format!("{ts} is length-delimited on the wire"));
                    };
                    return self.dec_num(ts, n, wt, zz);
                }
                let FieldValue::Bytes(b) = v else {
                    return err(format!("{ts} is not length-delimited on the wire"));
                };
                match ts.as_str() {
                    "string" => match std::str::from_utf8(b) {
                        Ok(s) => Ok(Value::String(s.into())),
                        Err(e) => err(format!("invalid UTF-8: {e}")),
                    },
                    "byte[]" => Ok(Value::String(hex(b))),
                    "Guid" => dec_guid(b).map(Value::String),
                    "DateTime" => dec_datetime(b).map(Value::String),
                    "TimeSpan" => dec_timespan(b),
                    _ => self.decode(ts, b),
                }
            }
        }
    }

    fn default_value(&self, ty: &TypeExpr) -> Value {
        match ty {
            TypeExpr::List(_) => Value::Array(Vec::new()),
            TypeExpr::Map(..) => Value::Object(Map::new()),
            TypeExpr::Nullable(_) | TypeExpr::Tuple(_) => Value::Null,
            TypeExpr::Name(ts) => match ts.as_str() {
                "string" => Value::String(String::new()),
                "bool" => Value::Bool(false),
                "double" | "float" => float_value(0.0),
                "Guid" => Value::String("00000000-0000-0000-0000-000000000000".into()),
                t if self.is_scalar_numeric(t) => Value::from(0),
                _ => Value::Null,
            },
        }
    }

    /// Decode a map entry message {1: key, 2: value}.
    fn dec_entry(&self, kt: &TypeExpr, vt: &TypeExpr, b: &[u8]) -> Result<(Value, Value)> {
        let (mut k, mut val) = (self.default_value(kt), self.default_value(vt));
        for f in read_fields(b)? {
            match f.tag {
                1 => k = self.dec_item(kt, f.wt, f.value, false)?,
                2 => match (vt, &mut val) {
                    (TypeExpr::List(inner), Value::Array(lst)) => match (inner.as_ref(), f.value) {
                        (TypeExpr::Name(ts), FieldValue::Bytes(b))
                            if f.wt == 2 && self.is_scalar_numeric(ts) =>
                        {
                            lst.extend(self.dec_packed(ts, b, Wire::Default, false)?);
                        }
                        _ => lst.push(self.dec_item(inner, f.wt, f.value, false)?),
                    },
                    (TypeExpr::Map(k2t, v2t), Value::Object(map)) => {
                        let FieldValue::Bytes(b) = f.value else {
                            return err("map entry is not length-delimited");
                        };
                        let (k2, v2) = self.dec_entry(k2t, v2t, b)?;
                        map.insert(key_string(&k2)?, v2);
                    }
                    _ => val = self.dec_item(vt, f.wt, f.value, false)?,
                },
                _ => {}
            }
        }
        Ok((k, val))
    }

    // ------------------------------------------------------------ MessageBase helpers

    pub fn encode_message(&self, class: &str, fields: &Value, requests_id: i64) -> Result<Vec<u8>> {
        let mut d = match fields {
            Value::Object(m) => m.clone(),
            Value::Null => Map::new(),
            _ => return err(format!("message fields must be an object, got {fields}")),
        };
        d.insert("$type".into(), Value::String(class.into()));
        d.insert("RequestsId".into(), Value::from(requests_id));
        self.encode("MessageBase", &Value::Object(d))
    }

    pub fn decode_message(&self, data: &[u8]) -> Result<Value> {
        self.decode("MessageBase", data)
    }

    /// Ack stamp: (MessageType << 32) | RequestsId, as a signed 64-bit value.
    pub fn ack_stamp(&self, msg: &Value) -> i64 {
        let class = msg.get("$type").and_then(Value::as_str).unwrap_or("");
        let name = self.cat.messages.get(class).map_or("None", String::as_str);
        let t = MESSAGE_TYPES.iter().position(|n| *n == name).unwrap_or(0) as i64;
        let id = msg.get("RequestsId").and_then(Value::as_i64).unwrap_or(0);
        (t << 32) | id
    }
}

/// Encode a value of `type_name` with the embedded catalog.
pub fn encode(type_name: &str, value: &Value) -> Result<Vec<u8>> {
    Codec::new(catalog()).encode(type_name, value)
}

/// Decode `data` as `type_name` (from the root of its hierarchy) with the embedded catalog.
pub fn decode(type_name: &str, data: &[u8]) -> Result<Value> {
    Codec::new(catalog()).decode(type_name, data)
}

/// One complete WebSocket frame: `MessageBase` with the subtype `class`, its `fields` and `RequestsId`.
pub fn encode_message(class: &str, fields: &Value, requests_id: i64) -> Result<Vec<u8>> {
    Codec::new(catalog()).encode_message(class, fields, requests_id)
}

pub fn decode_message(data: &[u8]) -> Result<Value> {
    Codec::new(catalog()).decode_message(data)
}

/// Acknowledgement stamp of a decoded message: (MessageType << 32) | RequestsId.
pub fn ack_stamp(msg: &Value) -> i64 {
    Codec::new(catalog()).ack_stamp(msg)
}
