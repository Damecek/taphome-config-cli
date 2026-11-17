//! The protocol catalog embedded from `data/catalog.json`.
//!
//! `scripts/build_cli_catalog.py` generates the file from `openapi/taphome-core.yaml`. It holds the protobuf
//! types for the codec, the operation registry (request class -> operation ids, kind, verification) and enum
//! name tables.

use std::collections::{BTreeMap, HashMap};
use std::sync::OnceLock;

use serde::Deserialize;

const CATALOG_JSON: &str = include_str!("../data/catalog.json");

/// A parsed C# type expression of a member, as the codec needs it.
#[derive(Debug, Clone, PartialEq)]
pub enum TypeExpr {
    List(Box<TypeExpr>),
    Map(Box<TypeExpr>, Box<TypeExpr>),
    Nullable(Box<TypeExpr>),
    Tuple(Vec<TypeExpr>),
    /// A scalar (`int`, `string`, `byte[]`, `Guid`, ...), an enum or a message class. Namespaces are stripped.
    Name(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wire {
    Default,
    Fixed64,
    Fixed32,
}

#[derive(Debug, Clone)]
pub struct Member {
    pub tag: u32,
    pub name: String,
    /// The C# type string from the catalog, for error messages.
    pub type_name: String,
    pub ty: TypeExpr,
    pub required: bool,
    pub packed: bool,
    pub wire: Wire,
    pub zigzag: bool,
}

#[derive(Debug, Clone)]
pub struct TypeDef {
    pub name: String,
    pub is_enum: bool,
    /// Sorted by tag.
    pub members: Vec<Member>,
    /// (include tag, subtype name), sorted by tag.
    pub includes: Vec<(u32, String)>,
    /// (name, value) for enums.
    pub values: Vec<(String, i64)>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Operation {
    /// Primary operationId first, then the variants that share the request class.
    pub ids: Vec<String>,
    /// read | subscribe | write | action. A class used by any write or action operation is write or action.
    pub kind: String,
    pub verified: bool,
    /// Typed response class, `MessageGenericResponse`, or None when no reply is expected.
    pub response: Option<String>,
    /// WebSocket endpoint, `/Core`, `/Identify`, `/SetUp` or `/ForgottenPassword`.
    pub endpoint: String,
    pub min_protocol: Option<i64>,
}

#[derive(Debug)]
pub struct Catalog {
    pub types: HashMap<String, TypeDef>,
    /// Message class -> MessageType name (Request, Response, Broadcast, ...).
    pub messages: HashMap<String, String>,
    pub operations: BTreeMap<String, Operation>,
    /// Enum tables that are not schema types (CommonErrorCodes, LoginResult, TriggerType, ...): value -> name.
    pub enums: HashMap<String, BTreeMap<i64, String>>,
    /// Subtype -> (base class, include tag).
    pub parent: HashMap<String, (String, u32)>,
}

#[derive(Deserialize)]
struct RawCatalog {
    types: Vec<RawType>,
    messages: Vec<RawMessage>,
    #[serde(default)]
    operations: BTreeMap<String, Operation>,
    #[serde(default)]
    enums: HashMap<String, BTreeMap<String, String>>,
}

#[derive(Deserialize)]
struct RawType {
    name: String,
    kind: String,
    #[serde(default)]
    members: Option<Vec<RawMember>>,
    #[serde(default)]
    includes: Option<Vec<RawInclude>>,
    #[serde(default)]
    values: Option<Vec<RawValue>>,
}

#[derive(Deserialize)]
struct RawMember {
    tag: u32,
    name: String,
    #[serde(rename = "type")]
    ty: String,
    #[serde(default)]
    wire_type: Option<String>,
    #[serde(default)]
    options: Option<RawOptions>,
}

#[derive(Deserialize, Default)]
struct RawOptions {
    #[serde(rename = "IsPacked", default)]
    packed: bool,
    #[serde(rename = "IsRequired", default)]
    required: bool,
    #[serde(rename = "DataFormat", default)]
    data_format: Option<String>,
}

#[derive(Deserialize)]
struct RawInclude {
    tag: u32,
    #[serde(rename = "type")]
    ty: String,
}

#[derive(Deserialize)]
struct RawValue {
    name: String,
    value: i64,
}

#[derive(Deserialize)]
struct RawMessage {
    name: String,
    message_type: Option<String>,
}

fn short(name: &str) -> &str {
    name.rsplit('.').next().unwrap_or(name)
}

/// Split `Dictionary<int, List<Guid>>` into ("Dictionary", ["int", "List<Guid>"]).
fn split_generic(s: &str) -> (&str, Vec<&str>) {
    let Some(open) = s.find('<') else {
        return (s, Vec::new());
    };
    let inner = &s[open + 1..s.len() - 1];
    let (mut args, mut depth, mut start) = (Vec::new(), 0i32, 0usize);
    for (i, ch) in inner.char_indices() {
        match ch {
            '<' => depth += 1,
            '>' => depth -= 1,
            ',' if depth == 0 => {
                args.push(inner[start..i].trim());
                start = i + 1;
            }
            _ => {}
        }
    }
    args.push(inner[start..].trim());
    (&s[..open], args)
}

/// Parse a C# type string the way the Python probe codec's `classify` does.
pub fn parse_type(t: &str) -> TypeExpr {
    let t = t.trim();
    if t == "ValueLogTypeHashSet" {
        // custom ICollection<ValueLogType>, written as repeated int32
        return TypeExpr::List(Box::new(TypeExpr::Name("int".into())));
    }
    if t == "byte[]" {
        return TypeExpr::Name("byte[]".into());
    }
    if let Some(inner) = t.strip_suffix("[]") {
        return TypeExpr::List(Box::new(parse_type(inner)));
    }
    if let Some(inner) = t.strip_suffix('?') {
        return TypeExpr::Nullable(Box::new(parse_type(inner)));
    }
    let (name, args) = split_generic(t);
    match (short(name), args.len()) {
        ("List" | "HashSet" | "IList" | "ICollection" | "IEnumerable", 1) => {
            TypeExpr::List(Box::new(parse_type(args[0])))
        }
        ("Dictionary" | "ConcurrentDictionary" | "IDictionary", 2) => {
            TypeExpr::Map(Box::new(parse_type(args[0])), Box::new(parse_type(args[1])))
        }
        ("Tuple" | "KeyValuePair", n) if n > 0 => {
            TypeExpr::Tuple(args.iter().map(|a| parse_type(a)).collect())
        }
        _ => TypeExpr::Name(short(t).to_string()),
    }
}

impl Catalog {
    /// Parse a catalog in the `data/catalog.json` format. Fails on malformed data and when a class and one of
    /// its bases declare a member with the same name (decode flattens a hierarchy into one object).
    pub fn from_json(text: &str) -> Result<Catalog, String> {
        let raw: RawCatalog = serde_json::from_str(text).map_err(|e| format!("catalog: {e}"))?;
        let mut types = HashMap::new();
        let mut parent = HashMap::new();
        for t in raw.types {
            let mut members: Vec<Member> = t
                .members
                .unwrap_or_default()
                .into_iter()
                .map(|m| {
                    let o = m.options.unwrap_or_default();
                    Member {
                        tag: m.tag,
                        ty: parse_type(&m.ty),
                        type_name: m.ty,
                        name: m.name,
                        required: o.required,
                        packed: o.packed,
                        wire: match m.wire_type.as_deref() {
                            Some("Fixed64") => Wire::Fixed64,
                            Some("Fixed32") => Wire::Fixed32,
                            _ => Wire::Default,
                        },
                        zigzag: o.data_format.is_some_and(|f| f.contains("ZigZag")),
                    }
                })
                .collect();
            members.sort_by_key(|m| m.tag);
            let mut includes: Vec<(u32, String)> = t
                .includes
                .unwrap_or_default()
                .into_iter()
                .map(|i| (i.tag, short(&i.ty).to_string()))
                .collect();
            includes.sort();
            let name = short(&t.name).to_string();
            for (tag, sub) in &includes {
                parent.insert(sub.clone(), (name.clone(), *tag));
            }
            let values = t
                .values
                .unwrap_or_default()
                .into_iter()
                .map(|v| (v.name, v.value))
                .collect();
            types.insert(
                name.clone(),
                TypeDef {
                    name,
                    is_enum: t.kind == "enum",
                    members,
                    includes,
                    values,
                },
            );
        }
        let messages = raw
            .messages
            .into_iter()
            .filter_map(|m| m.message_type.map(|t| (short(&m.name).to_string(), t)))
            .collect();
        let mut enums = HashMap::new();
        for (name, table) in raw.enums {
            let mut out = BTreeMap::new();
            for (k, v) in table {
                let k: i64 = k
                    .parse()
                    .map_err(|_| format!("catalog: enum {name} has a non-integer value {k}"))?;
                out.insert(k, v);
            }
            enums.insert(name, out);
        }
        let cat = Catalog {
            types,
            messages,
            operations: raw.operations,
            enums,
            parent,
        };
        cat.check_name_collisions()?;
        Ok(cat)
    }

    fn check_name_collisions(&self) -> Result<(), String> {
        for start in self.parent.keys() {
            let mut seen: HashMap<&str, &str> = HashMap::new();
            let mut cur = start.as_str();
            loop {
                for m in self
                    .types
                    .get(cur)
                    .map(|t| t.members.as_slice())
                    .unwrap_or_default()
                {
                    if let Some(other) = seen.insert(&m.name, cur) {
                        return Err(format!(
                            "catalog: member {} is declared in both {other} and {cur}",
                            m.name
                        ));
                    }
                }
                match self.parent.get(cur) {
                    Some((p, _)) => cur = p,
                    None => break,
                }
            }
        }
        Ok(())
    }

    /// Type definition for a possibly dotted name.
    pub fn resolve(&self, name: &str) -> Option<&TypeDef> {
        self.types.get(short(name))
    }

    pub fn is_enum(&self, name: &str) -> bool {
        self.types.get(name).is_some_and(|t| t.is_enum)
    }

    pub fn enum_name(&self, enum_name: &str, value: i64) -> Option<&str> {
        if let Some(table) = self.enums.get(enum_name) {
            return table.get(&value).map(String::as_str);
        }
        let t = self.types.get(enum_name).filter(|t| t.is_enum)?;
        t.values
            .iter()
            .find(|(_, v)| *v == value)
            .map(|(n, _)| n.as_str())
    }

    pub fn enum_value(&self, enum_name: &str, name: &str) -> Option<i64> {
        if let Some(table) = self.enums.get(enum_name) {
            return table
                .iter()
                .find(|(_, n)| n.as_str() == name)
                .map(|(v, _)| *v);
        }
        let t = self.types.get(enum_name).filter(|t| t.is_enum)?;
        t.values.iter().find(|(n, _)| n == name).map(|(_, v)| *v)
    }
}

/// The embedded catalog, parsed once.
pub fn catalog() -> &'static Catalog {
    static CATALOG: OnceLock<Catalog> = OnceLock::new();
    CATALOG.get_or_init(|| {
        Catalog::from_json(CATALOG_JSON).expect("embedded data/catalog.json is valid")
    })
}

/// Registry entry of a request class (the class name in the spec path), or None when the spec does not know it.
pub fn operation(request_class: &str) -> Option<&'static Operation> {
    catalog().operations.get(request_class)
}

/// All operations, sorted by request class.
pub fn operations() -> impl Iterator<Item = (&'static str, &'static Operation)> {
    catalog().operations.iter().map(|(k, v)| (k.as_str(), v))
}

/// Name of an enum value: the spec's wire tables (CommonErrorCodes, ResponseFlags, MessageType, LoginResult,
/// ReconnectResult, TriggerType) first, then enum schemas (ValueLogType, DeviceStatus, ...). Aliases resolve to the
/// first name.
pub fn enum_name(enum_name: &str, value: i64) -> Option<&'static str> {
    catalog().enum_name(enum_name, value)
}

pub fn enum_value(enum_name: &str, name: &str) -> Option<i64> {
    catalog().enum_value(enum_name, name)
}

/// MessageType name of a message class (Request, Response, Broadcast, ...).
pub fn message_type(class: &str) -> Option<&'static str> {
    catalog().messages.get(class).map(String::as_str)
}
