//! `taphome pp inspect | import | script get | set | last-run`: Packet Parser templates and scripts.
//!
//! A template (`.xml`) or config (`.json`) becomes one `TemplateConfiguration`, the same JSON the app
//! sends in `MessageHttpApiConfigRequest.ConfigJson`. `inspect` shows it offline, `import` sends it.
use crate::core::{
    diff_entry, open_session, request, run_write, Check, Ctx, Snapshot, WriteFlags, WritePlan,
    WriteSpec,
};
use crate::errors::CliError;
use crate::output::{mask_value, REDACTED};
use clap::{ArgGroup, Args, Subcommand, ValueEnum};
use serde_json::{json, Map, Value};
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::rc::Rc;

const PP_PROTOCOL: i64 = 66;
const EMPTY_GUID: &str = "00000000-0000-0000-0000-000000000000";
const PP_CCU_MODEL: &str = "PacketParserCCU";
const VERIFIED_MODULE: &str = "PacketParserModuleHttp";
const PREFIX_PARAM: &str = "DeviceNamePrefix";
const VARIABLE_KEYS: [&str; 3] = ["CustomVariables", "DeviceVariables", "Variables"];

#[derive(Subcommand)]
pub enum Cmd {
    /// Parse a template (.xml) or config (.json) offline and print the ConfigJson it produces
    Inspect(InspectArgs),
    /// Create a module and its devices on the Core from a template or config
    Import(ImportArgs),
    /// Read, change and debug the scripts of a device
    Script {
        #[command(subcommand)]
        cmd: ScriptCmd,
    },
}

#[derive(Args)]
pub struct InspectArgs {
    /// Template (.xml) or config (.json)
    file: PathBuf,
    /// Value for a template parameter, repeatable
    #[arg(long = "param", value_name = "K=V")]
    params: Vec<String>,
}

#[derive(Args)]
pub struct ImportArgs {
    /// Template (.xml) or config (.json)
    file: PathBuf,
    /// Value for a template parameter, repeatable
    #[arg(long = "param", value_name = "K=V")]
    params: Vec<String>,
    /// Interface (id or exact name). Default: the only PacketParserCCU interface
    #[arg(long, value_name = "ID")]
    ccu: Option<String>,
    #[command(flatten)]
    flags: WriteFlags,
}

#[derive(Subcommand)]
pub enum ScriptCmd {
    /// List the script properties of a device, or print one in full
    Get(GetArgs),
    /// Replace one script property
    Set(SetArgs),
    /// Last run of a script: result, time, variables
    LastRun(LastRunArgs),
}

#[derive(Args)]
pub struct GetArgs {
    /// Device id or exact name
    device: String,
    /// Property key: print its full text
    #[arg(long)]
    key: Option<String>,
}

#[derive(Args)]
#[command(group(ArgGroup::new("source").required(true).args(["file", "text"])))]
pub struct SetArgs {
    /// Device id or exact name
    device: String,
    /// Property key of the script to change, for example ReadStateScriptPacketParser
    #[arg(long)]
    key: String,
    /// Read the new text from FILE, `-` for stdin
    #[arg(long, value_name = "FILE")]
    file: Option<PathBuf>,
    /// The new text
    #[arg(long)]
    text: Option<String>,
    /// Allow a key the device does not have yet
    #[arg(long)]
    allow_new_key: bool,
    #[command(flatten)]
    flags: WriteFlags,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum ObjectKind {
    Device,
    Ccu,
    Rule,
}

#[derive(Args)]
pub struct LastRunArgs {
    /// Device id or exact name (with --object ccu or rule: that object's id)
    #[arg(value_name = "DEV")]
    target: String,
    /// Script property key. Not needed for a rule
    #[arg(long)]
    key: Option<String>,
    /// What DEV refers to
    #[arg(long = "object", value_enum, default_value = "device")]
    object_kind: ObjectKind,
}

pub async fn run(ctx: &Ctx, cmd: Cmd) -> Result<Value, CliError> {
    match cmd {
        Cmd::Inspect(args) => inspect(ctx, args),
        Cmd::Import(args) => import(ctx, args).await,
        Cmd::Script { cmd } => match cmd {
            ScriptCmd::Get(args) => script_get(ctx, args).await,
            ScriptCmd::Set(args) => script_set(ctx, args).await,
            ScriptCmd::LastRun(args) => script_last_run(ctx, args).await,
        },
    }
}

fn bad(message: impl Into<String>) -> CliError {
    CliError::invalid_input(message)
}

// ---------------------------------------------------------------- loading

struct Param {
    id: String,
    label: Option<String>,
    default: Option<String>,
}

struct Loaded {
    source: &'static str,
    meta: Value,
    /// Declared by the template. Empty for JSON input.
    params: Vec<Param>,
    /// `TemplateConfiguration` as read: tokens unresolved, ids and defaults not yet filled in.
    config: Value,
    /// `CcuId` of a `{"CcuId", "ConfigJson"}` file.
    ccu_id: Option<i64>,
}

fn load(path: &Path) -> Result<Loaded, CliError> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase);
    let text = std::fs::read_to_string(path)
        .map_err(|e| bad(format!("cannot read {}: {e}", path.display())))?;
    match ext.as_deref() {
        Some("xml") => from_xml(&text),
        Some("json") => from_json(&text),
        _ => Err(bad("FILE must end in .xml (template) or .json (config)")),
    }
}

fn from_json(text: &str) -> Result<Loaded, CliError> {
    let root: Value =
        serde_json::from_str(text).map_err(|e| bad(format!("not valid JSON: {e}")))?;
    let Some(obj) = root.as_object() else {
        return Err(bad("the JSON file must hold an object"));
    };
    let (config, ccu_id) = if let Some(inner) = obj.get("ConfigJson") {
        let ccu = match obj.get("CcuId") {
            None | Some(Value::Null) => None,
            Some(v) => Some(v.as_i64().ok_or_else(|| bad("CcuId must be an integer"))?),
        };
        let config = match inner {
            Value::String(s) => serde_json::from_str(s)
                .map_err(|e| bad(format!("ConfigJson is not valid JSON: {e}")))?,
            other => other.clone(),
        };
        (config, ccu)
    } else {
        (root.clone(), None)
    };
    Ok(Loaded {
        source: "json",
        meta: json!({}),
        params: vec![],
        config,
        ccu_id,
    })
}

fn text_of(node: roxmltree::Node) -> String {
    node.children()
        .filter(|c| c.is_text())
        .filter_map(|c| c.text())
        .collect()
}

fn child<'a>(node: roxmltree::Node<'a, 'a>, name: &str) -> Option<roxmltree::Node<'a, 'a>> {
    node.children()
        .find(|c| c.is_element() && c.tag_name().name() == name)
}

fn children<'a>(
    node: roxmltree::Node<'a, 'a>,
    name: &'a str,
) -> impl Iterator<Item = roxmltree::Node<'a, 'a>> {
    node.children()
        .filter(move |c| c.is_element() && c.tag_name().name() == name)
}

fn child_text(node: roxmltree::Node, name: &str) -> Option<String> {
    child(node, name).map(|n| text_of(n).trim().to_string())
}

fn is_guid(s: &str) -> bool {
    s.len() == 36
        && s.char_indices().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => c == '-',
            _ => c.is_ascii_hexdigit(),
        })
}

fn device_from_xml(node: roxmltree::Node, module: bool) -> Result<Value, CliError> {
    let name = child_text(node, "Name").unwrap_or_default();
    let id = match child_text(node, "Id") {
        None => 0,
        Some(t) if t.is_empty() => 0,
        Some(t) => t
            .parse::<i64>()
            .map_err(|_| bad(format!("device \"{name}\": Id \"{t}\" is not an integer")))?,
    };
    let mut props = Map::new();
    if let Some(p) = child(node, "DeviceProperties") {
        for e in p.children().filter(|c| c.is_element()) {
            props.insert(e.tag_name().name().to_string(), json!(text_of(e)));
        }
    }
    let mut out = Map::new();
    // The app prefixes every name with the DeviceNamePrefix parameter.
    out.insert("Name".into(), json!(format!("$[{PREFIX_PARAM}]{name}")));
    out.insert(
        "Model".into(),
        json!(child_text(node, "Model").unwrap_or_default()),
    );
    out.insert("Id".into(), json!(id));
    out.insert("DeviceProperties".into(), Value::Object(props));
    if module {
        let devices = match child(node, "Devices") {
            Some(d) => children(d, "Device")
                .map(|n| device_from_xml(n, false))
                .collect::<Result<Vec<_>, _>>()?,
            None => vec![],
        };
        out.insert("Devices".into(), Value::Array(devices));
    }
    Ok(Value::Object(out))
}

fn from_xml(text: &str) -> Result<Loaded, CliError> {
    let doc = roxmltree::Document::parse(text).map_err(|e| bad(format!("not valid XML: {e}")))?;
    let root = doc.root_element();
    if root.tag_name().name() != "Templates" {
        return Err(bad("the root element must be <Templates>"));
    }
    let format = root.attribute("format").and_then(|f| f.parse::<i64>().ok());
    if !matches!(format, Some(0..=2)) {
        return Err(bad("Unsupported format version: format must be 0, 1 or 2"));
    }
    let protocol = root
        .attribute("protocolVersion")
        .and_then(|p| p.parse::<i64>().ok())
        .unwrap_or(PP_PROTOCOL);
    let templates: Vec<_> = children(root, "Template").collect();
    let [template] = templates.as_slice() else {
        return Err(bad(format!(
            "expected exactly one <Template>, found {}",
            templates.len()
        )));
    };
    let mut params = vec![];
    if let Some(list) = child(*template, "ImportParameters") {
        for p in children(list, "Parameter") {
            let id = child_text(p, "Id").unwrap_or_default();
            if id.is_empty() {
                return Err(bad("an ImportParameters entry has no Id"));
            }
            params.push(Param {
                label: child_text(p, "Name"),
                default: child(p, "Value").map(text_of),
                id,
            });
        }
    }
    if !params.iter().any(|p| p.id == PREFIX_PARAM) {
        params.push(Param {
            id: PREFIX_PARAM.into(),
            label: Some("Device name prefix".into()),
            default: Some(String::new()),
        });
    }
    let modules = children(*template, "Module")
        .map(|n| device_from_xml(n, true))
        .collect::<Result<Vec<_>, _>>()?;
    let devices = children(*template, "Device")
        .map(|n| device_from_xml(n, false))
        .collect::<Result<Vec<_>, _>>()?;
    let name = child_text(*template, "Name");
    let revision = template.attribute("revision");
    let guid = template.attribute("id").filter(|g| is_guid(g));
    let mut config = Map::new();
    config.insert("ProtocolVersion".into(), json!(protocol));
    if let Some(n) = &name {
        config.insert("Name".into(), json!(n));
    }
    config.insert("Id".into(), json!(guid.unwrap_or(EMPTY_GUID)));
    if let Some(r) = revision {
        config.insert("Revision".into(), json!(r));
    }
    config.insert("Modules".into(), Value::Array(modules));
    config.insert("Devices".into(), Value::Array(devices));
    Ok(Loaded {
        source: "xml",
        meta: json!({"name": name, "revision": revision, "id": guid, "format": format,
                     "protocol_version": protocol, "ccu_model": child_text(*template, "CcuModel")}),
        params,
        config: Value::Object(config),
        ccu_id: None,
    })
}

// ---------------------------------------------------------------- normalising

/// Negative placeholder ids for modules and devices that have none.
struct Placeholders {
    used: BTreeSet<i64>,
    last: i64,
}

impl Placeholders {
    fn next(&mut self) -> i64 {
        loop {
            self.last -= 1;
            if !self.used.contains(&self.last) {
                return self.last;
            }
        }
    }
}

fn list<'a>(obj: &'a Map<String, Value>, key: &str) -> Result<&'a [Value], CliError> {
    match obj.get(key) {
        None | Some(Value::Null) => Ok(&[]),
        Some(Value::Array(items)) => Ok(items),
        Some(_) => Err(bad(format!("{key} must be a list"))),
    }
}

fn opt_int(obj: &Map<String, Value>, key: &str, at: &str) -> Result<i64, CliError> {
    match obj.get(key) {
        None | Some(Value::Null) => Ok(0),
        Some(v) => v
            .as_i64()
            .ok_or_else(|| bad(format!("{at}: {key} must be an integer"))),
    }
}

fn opt_text<'a>(
    obj: &'a Map<String, Value>,
    key: &str,
    at: &str,
) -> Result<Option<&'a str>, CliError> {
    match obj.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s)),
        Some(_) => Err(bad(format!("{at}: {key} must be a string"))),
    }
}

fn required_text<'a>(
    obj: &'a Map<String, Value>,
    key: &str,
    at: &str,
) -> Result<&'a str, CliError> {
    match opt_text(obj, key, at)? {
        Some(s) if !s.trim().is_empty() => Ok(s),
        _ => Err(bad(format!(
            "{at}: {key} is required and must be a non-empty string"
        ))),
    }
}

/// The Core stores every property as text: numbers and booleans are converted the way the app does.
fn prop_text(v: &Value, at: &str, key: &str) -> Result<Option<String>, CliError> {
    match v {
        Value::Null => Ok(None),
        Value::String(s) => Ok(Some(s.clone())),
        Value::Bool(b) => Ok(Some(if *b { "True" } else { "False" }.to_string())),
        Value::Number(n) => Ok(Some(n.to_string())),
        _ => Err(bad(format!(
            "{at}: property {key} must be a string, number or boolean (write JSON values as text)"
        ))),
    }
}

fn normalize_device(
    v: &Value,
    at: &str,
    module: bool,
    top_level: bool,
    ids: &mut Placeholders,
) -> Result<Value, CliError> {
    let obj = v
        .as_object()
        .ok_or_else(|| bad(format!("{at} must be an object")))?;
    let name = required_text(obj, "Name", at)?;
    let at = &format!("{at} \"{name}\"");
    let model = required_text(obj, "Model", at)?;
    let id = match opt_int(obj, "Id", at)? {
        0 => ids.next(),
        given if given > 0 => {
            return Err(bad(format!(
                "{at}: ids in a config are placeholders and must be 0 or negative; the Core allocates real ids"
            )))
        }
        given => given,
    };
    let parent = opt_int(obj, "HardwareParentId", at)?;
    if top_level && parent == 0 {
        return Err(bad(format!(
            "{at}: a device outside a module needs the HardwareParentId of an existing module"
        )));
    }
    let mut props = Map::new();
    match obj.get("DeviceProperties") {
        None | Some(Value::Null) => {}
        Some(Value::Object(map)) => {
            for (k, value) in map {
                if let Some(text) = prop_text(value, at, k)? {
                    props.insert(k.clone(), json!(text));
                }
            }
        }
        Some(_) => return Err(bad(format!("{at}: DeviceProperties must be an object"))),
    }
    let guid = |key: &str| -> Result<String, CliError> {
        match opt_text(obj, key, at)? {
            None | Some("") => Ok(EMPTY_GUID.to_string()),
            Some(g) if is_guid(g) => Ok(g.to_string()),
            Some(g) => Err(bad(format!("{at}: {key} \"{g}\" is not a Guid"))),
        }
    };
    let mut out = Map::new();
    out.insert("Name".into(), json!(name));
    out.insert("Model".into(), json!(model));
    out.insert("Id".into(), json!(id));
    out.insert("HardwareParentId".into(), json!(parent));
    out.insert("CategoryId".into(), json!(guid("CategoryId")?));
    out.insert("ZoneId".into(), json!(guid("ZoneId")?));
    out.insert("DeviceProperties".into(), Value::Object(props));
    if module {
        let mut devices = vec![];
        for (i, d) in list(obj, "Devices")?.iter().enumerate() {
            devices.push(normalize_device(
                d,
                &format!("{at} Devices[{i}]"),
                false,
                false,
                ids,
            )?);
        }
        out.insert("Devices".into(), Value::Array(devices));
    }
    Ok(Value::Object(out))
}

fn explicit_ids(obj: &Map<String, Value>, used: &mut BTreeSet<i64>) {
    let mut note = |d: &Value| {
        if let Some(id) = d["Id"].as_i64().filter(|i| *i != 0) {
            used.insert(id);
        }
    };
    for key in ["Modules", "Devices"] {
        for d in obj.get(key).and_then(Value::as_array).into_iter().flatten() {
            note(d);
            for c in d["Devices"].as_array().into_iter().flatten() {
                note(c);
            }
        }
    }
}

/// Validates the shape and fills in what the confirmed payload has: placeholder ids, `HardwareParentId` 0,
/// empty Guids, text-only properties. Returns the canonical `TemplateConfiguration`.
fn normalize(config: &Value) -> Result<Value, CliError> {
    let obj = config
        .as_object()
        .ok_or_else(|| bad("the config must be a JSON object"))?;
    let protocol = match obj.get("ProtocolVersion") {
        None | Some(Value::Null) => PP_PROTOCOL,
        Some(v) => v
            .as_i64()
            .ok_or_else(|| bad("ProtocolVersion must be an integer"))?,
    };
    let (modules, devices) = (list(obj, "Modules")?, list(obj, "Devices")?);
    if modules.is_empty() && devices.is_empty() {
        return Err(bad("the config has no Modules and no Devices"));
    }
    let mut used = BTreeSet::new();
    explicit_ids(obj, &mut used);
    let mut ids = Placeholders { used, last: 0 };
    let mut out = Map::new();
    out.insert("ProtocolVersion".into(), json!(protocol));
    for key in ["Name", "Id", "Revision"] {
        if let Some(t) = opt_text(obj, key, "config")? {
            out.insert(key.into(), json!(t));
        }
    }
    let mut norm = |items: &[Value], key: &str, module: bool| -> Result<(), CliError> {
        if items.is_empty() {
            return Ok(());
        }
        let mut done = vec![];
        for (i, d) in items.iter().enumerate() {
            done.push(normalize_device(
                d,
                &format!("{key}[{i}]"),
                module,
                !module,
                &mut ids,
            )?);
        }
        out.insert(key.into(), Value::Array(done));
        Ok(())
    };
    norm(modules, "Modules", true)?;
    norm(devices, "Devices", false)?;
    Ok(Value::Object(out))
}

// ---------------------------------------------------------------- parameters

fn is_token_id(s: &str) -> bool {
    let mut chars = s.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
}

/// Plain replacement of `$[Id]`. An id with no value stays in the text and is recorded in `missing`.
/// Things such as the JSON path `$[0]` are not tokens.
fn replace_tokens(
    text: &str,
    values: &BTreeMap<String, String>,
    missing: &mut BTreeSet<String>,
) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("$[") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        match after.find(']').filter(|end| is_token_id(&after[..*end])) {
            Some(end) => {
                let id = &after[..end];
                match values.get(id) {
                    Some(v) => out.push_str(v),
                    None => {
                        missing.insert(id.to_string());
                        out.push_str(&rest[start..start + end + 3]);
                    }
                }
                rest = &after[end + 1..];
            }
            None => {
                out.push_str("$[");
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Substitutes the tokens in every name and property value of a normalised config.
fn substitute(
    config: &mut Value,
    values: &BTreeMap<String, String>,
    missing: &mut BTreeSet<String>,
) {
    fn text(v: &mut Value, values: &BTreeMap<String, String>, missing: &mut BTreeSet<String>) {
        if let Some(t) = v.as_str() {
            *v = json!(replace_tokens(t, values, missing));
        }
    }
    fn device(d: &mut Value, values: &BTreeMap<String, String>, missing: &mut BTreeSet<String>) {
        if let Some(n) = d.get_mut("Name") {
            text(n, values, missing);
        }
        if let Some(props) = d.get_mut("DeviceProperties").and_then(Value::as_object_mut) {
            props.values_mut().for_each(|v| text(v, values, missing));
        }
    }
    if let Some(n) = config.get_mut("Name") {
        text(n, values, missing);
    }
    for key in ["Modules", "Devices"] {
        for d in config
            .get_mut(key)
            .and_then(Value::as_array_mut)
            .into_iter()
            .flatten()
        {
            device(d, values, missing);
            for c in d
                .get_mut("Devices")
                .and_then(Value::as_array_mut)
                .into_iter()
                .flatten()
            {
                device(c, values, missing);
            }
        }
    }
}

fn parse_params(raw: &[String]) -> Result<BTreeMap<String, String>, CliError> {
    let mut out = BTreeMap::new();
    for item in raw {
        match item.split_once('=') {
            Some((k, v)) if !k.is_empty() => {
                out.insert(k.to_string(), v.to_string());
            }
            _ => return Err(bad(format!("--param \"{item}\" must look like KEY=VALUE"))),
        }
    }
    Ok(out)
}

struct Prepared {
    source: &'static str,
    meta: Value,
    ccu_id: Option<i64>,
    /// Normalised, tokens replaced where a value exists.
    config: Value,
    parameters: Vec<Value>,
    /// Token ids that have no value and no default.
    unresolved: Vec<String>,
    modbus: bool,
}

fn prepare(file: &Path, raw_params: &[String]) -> Result<Prepared, CliError> {
    let loaded = load(file)?;
    let supplied = parse_params(raw_params)?;
    let mut config = normalize(&loaded.config)?;
    let mut found = BTreeSet::new();
    substitute(&mut config.clone(), &BTreeMap::new(), &mut found);
    let declared: Vec<(String, Option<String>, Option<String>)> = if loaded.source == "xml" {
        loaded
            .params
            .iter()
            .map(|p| (p.id.clone(), p.label.clone(), p.default.clone()))
            .collect()
    } else {
        found.iter().map(|id| (id.clone(), None, None)).collect()
    };
    let unknown: Vec<&String> = supplied
        .keys()
        .filter(|k| !declared.iter().any(|(id, _, _)| id == *k))
        .collect();
    if !unknown.is_empty() {
        let known: Vec<&String> = declared.iter().map(|(id, _, _)| id).collect();
        return Err(bad(format!(
            "unknown parameter id(s): {}. Known: {}",
            unknown
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", "),
            known
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ))
        .with_details(json!({"unknown": unknown, "known": known})));
    }
    let mut values: BTreeMap<String, String> = declared
        .iter()
        .filter_map(|(id, _, default)| Some((id.clone(), default.clone()?)))
        .collect();
    values.extend(supplied.clone());
    let mut missing = BTreeSet::new();
    substitute(&mut config, &values, &mut missing);
    let parameters = declared
        .iter()
        .map(|(id, label, default)| {
            json!({"id": id, "label": label, "default": default, "supplied": supplied.contains_key(id),
                   "value": values.get(id)})
        })
        .collect();
    let modbus = is_modbus(&config, &loaded.meta);
    Ok(Prepared {
        source: loaded.source,
        meta: loaded.meta,
        ccu_id: loaded.ccu_id,
        config,
        parameters,
        unresolved: missing.into_iter().collect(),
        modbus,
    })
}

// ---------------------------------------------------------------- reading a config

fn is_modbus(config: &Value, meta: &Value) -> bool {
    config["ProtocolVersion"] == json!(56)
        || meta["ccu_model"]
            .as_str()
            .is_some_and(|m| m.starts_with("Modbus"))
        || devices_of(config).any(|d| d["Model"].as_str().is_some_and(|m| m.starts_with("Modbus")))
}

/// Every module and device of a normalised config, modules first within their group.
fn devices_of(config: &Value) -> impl Iterator<Item = &Value> {
    ["Modules", "Devices"]
        .into_iter()
        .flat_map(|key| config[key].as_array().into_iter().flatten())
        .flat_map(|d| std::iter::once(d).chain(d["Devices"].as_array().into_iter().flatten()))
}

fn is_script_key(key: &str) -> bool {
    key.contains("Script") || VARIABLE_KEYS.contains(&key)
}

fn keys_of(props: &Value) -> Vec<&str> {
    props
        .as_object()
        .map(|m| m.keys().map(String::as_str).collect())
        .unwrap_or_default()
}

fn shorten(line: &str) -> String {
    match line.char_indices().nth(120) {
        Some((i, _)) => format!("{}...", &line[..i]),
        None => line.to_string(),
    }
}

fn script_summary(text: &str) -> Value {
    json!({"length": text.chars().count(), "first_line": shorten(text.lines().next().unwrap_or(""))})
}

fn device_summary(d: &Value) -> Value {
    let props = &d["DeviceProperties"];
    let scripts: Vec<&str> = keys_of(props)
        .into_iter()
        .filter(|k| is_script_key(k))
        .collect();
    let mut out = json!({"name": d["Name"], "model": d["Model"], "id": d["Id"],
                         "property_keys": keys_of(props), "script_keys": scripts});
    if let Some(children) = d["Devices"].as_array() {
        out["devices"] = children.iter().map(device_summary).collect();
    }
    out
}

/// What gets verified: one entry per module and device in the config.
struct Expect {
    placeholder: i64,
    name: String,
    model: String,
    parent: Parent,
    props: Vec<(String, String)>,
}

#[derive(Clone, Copy)]
enum Parent {
    None,
    Module(i64),
    Real(i64),
}

fn expected(config: &Value) -> Vec<Expect> {
    let make = |d: &Value, parent: Parent| Expect {
        placeholder: d["Id"].as_i64().unwrap_or(0),
        name: d["Name"].as_str().unwrap_or_default().to_string(),
        model: d["Model"].as_str().unwrap_or_default().to_string(),
        parent,
        props: d["DeviceProperties"]
            .as_object()
            .map(|m| {
                m.iter()
                    .map(|(k, v)| (k.clone(), v.as_str().unwrap_or_default().to_string()))
                    .collect()
            })
            .unwrap_or_default(),
    };
    let mut out = vec![];
    for m in config["Modules"].as_array().into_iter().flatten() {
        let me = make(m, Parent::None);
        let id = me.placeholder;
        out.push(me);
        for c in m["Devices"].as_array().into_iter().flatten() {
            out.push(make(c, Parent::Module(id)));
        }
    }
    for d in config["Devices"].as_array().into_iter().flatten() {
        out.push(make(
            d,
            Parent::Real(d["HardwareParentId"].as_i64().unwrap_or(0)),
        ));
    }
    out
}

impl Expect {
    fn diff(&self) -> Value {
        let parent = match self.parent {
            Parent::None => Value::Null,
            Parent::Module(p) | Parent::Real(p) => json!(p),
        };
        let scripts: Map<String, Value> = self
            .props
            .iter()
            .filter(|(k, _)| is_script_key(k))
            .map(|(k, v)| (k.clone(), script_summary(v)))
            .collect();
        let keys: Vec<&String> = self.props.iter().map(|(k, _)| k).collect();
        diff_entry(
            "add",
            "device",
            json!(self.placeholder),
            &self.name,
            Value::Null,
            json!({"name": self.name, "model": self.model, "parent": parent,
                   "property_keys": keys, "scripts": scripts}),
        )
    }
}

// ---------------------------------------------------------------- inspect

fn variant_notes(config: &Value) -> Vec<String> {
    let mut notes = vec![];
    for m in config["Modules"].as_array().into_iter().flatten() {
        let model = m["Model"].as_str().unwrap_or_default();
        if model != VERIFIED_MODULE {
            notes.push(format!(
                "module model {model} was never imported on a real Core (only {VERIFIED_MODULE} was)"
            ));
        }
    }
    if config["Devices"].as_array().is_some_and(|d| !d.is_empty()) {
        notes.push("devices without a new module (HardwareParentId of an existing module) were never imported on a real Core".into());
    }
    notes
}

fn inspect(ctx: &Ctx, args: InspectArgs) -> Result<Value, CliError> {
    let p = prepare(&args.file, &args.params)?;
    if p.modbus {
        ctx.warn("this is a Modbus config: pp import refuses it (different message class, not supported)");
    }
    let notes = variant_notes(&p.config);
    let summaries = |key: &str| -> Vec<Value> {
        p.config[key]
            .as_array()
            .into_iter()
            .flatten()
            .map(device_summary)
            .collect()
    };
    Ok(json!({"source": p.source, "template": p.meta,
              "kind": if p.modbus { "modbus" } else { "packet_parser" },
              "ccu_id": p.ccu_id, "parameters": p.parameters,
              "modules": summaries("Modules"), "devices": summaries("Devices"),
              "unresolved_tokens": p.unresolved,
              "variant": {"verified": notes.is_empty() && !p.modbus, "notes": notes},
              "config": p.config}))
}

// ---------------------------------------------------------------- import

#[derive(Default)]
struct Outcome {
    ccu_id: Option<i64>,
    created: Value,
    removed: Vec<Value>,
    differences: Vec<Value>,
}

fn pick_ccu(
    ctx: &Ctx,
    snapshot: &Snapshot,
    flag: Option<&str>,
    from_file: Option<i64>,
) -> Result<i64, CliError> {
    let id_of = |c: &Value| c["CcuId"].as_i64().unwrap_or(0);
    let chosen = if let Some(reference) = flag {
        Some(snapshot.find_interface(reference)?)
    } else if let Some(id) = from_file {
        Some(snapshot.interface(id).ok_or_else(|| {
            CliError::new(
                "not_found",
                format!("the file names interface {id}, the Core has none with that id"),
            )
        })?)
    } else {
        None
    };
    if let Some(c) = chosen {
        if c["Model"] != PP_CCU_MODEL {
            ctx.warn(format!(
                "interface {} has model {}, not {PP_CCU_MODEL}",
                id_of(c),
                c["Model"]
            ));
        }
        return Ok(id_of(c));
    }
    let found: Vec<&Value> = snapshot
        .interfaces
        .iter()
        .filter(|c| c["Model"] == PP_CCU_MODEL)
        .collect();
    match found.as_slice() {
        [one] => Ok(id_of(one)),
        [] => Err(CliError::new(
            "not_found",
            format!("the Core has no {PP_CCU_MODEL} interface: create it first (raw call MessageAddCcuRequest, unverified)"),
        )
        .with_details(json!({"hint": "taphome raw call MessageAddCcuRequest --allow-write --allow-unverified --json '{\"Model\": \"PacketParserCCU\"}'; check the fields with raw ops"}))),
        many => Err(CliError::new(
            "ambiguous",
            format!("{} {PP_CCU_MODEL} interfaces exist: pass --ccu", many.len()),
        )
        .with_details(json!({"ccu_ids": many.iter().map(|c| id_of(c)).collect::<Vec<_>>()}))),
    }
}

fn redact_diff(key: &str, value: &str, show: bool) -> Value {
    mask_value(key, value, show)
}

fn verify_import(
    after: &Snapshot,
    response: &Value,
    expect: &[Expect],
    before_count: usize,
    show_secrets: bool,
) -> (Vec<Check>, Outcome) {
    let added: Vec<&Value> = response["AddedDevices"]
        .as_array()
        .into_iter()
        .flatten()
        .collect();
    let mut taken = vec![false; expect.len()];
    let mut pairs: Vec<(&Value, Option<usize>)> = vec![];
    for a in added.iter().copied() {
        let hit = (0..expect.len()).find(|&i| {
            !taken[i]
                && a["Name"].as_str() == Some(expect[i].name.as_str())
                && a["Model"].as_str() == Some(expect[i].model.as_str())
        });
        if let Some(i) = hit {
            taken[i] = true;
        }
        pairs.push((a, hit));
    }
    let mut id_of: BTreeMap<i64, i64> = BTreeMap::new();
    for (a, hit) in &pairs {
        if let (Some(i), Some(id)) = (hit, a["ID"].as_i64()) {
            id_of.insert(expect[*i].placeholder, id);
        }
    }

    let unmatched_sent: Vec<&str> = (0..expect.len())
        .filter(|i| !taken[*i])
        .map(|i| expect[i].name.as_str())
        .collect();
    let unmatched_added: Vec<String> = pairs
        .iter()
        .filter(|(_, hit)| hit.is_none())
        .map(|(a, _)| {
            format!(
                "{} ({})",
                a["Name"].as_str().unwrap_or("?"),
                a["Model"].as_str().unwrap_or("?")
            )
        })
        .collect();
    let listed = Check::new(
        "AddedDevices lists exactly the modules and devices sent",
        unmatched_sent.is_empty() && unmatched_added.is_empty(),
    )
    .detail(format!(
        "sent {}, Core added {}; not added: [{}]; unexpected: [{}]",
        expect.len(),
        added.len(),
        unmatched_sent.join(", "),
        unmatched_added.join(", ")
    ));

    let mut missing = vec![];
    let mut wrong_parent = vec![];
    let mut differences = vec![];
    for (a, hit) in &pairs {
        let id = a["ID"].as_i64().unwrap_or(0);
        let stored = after
            .device(id)
            .filter(|d| d["Name"] == a["Name"] && d["Model"] == a["Model"]);
        let Some(stored) = stored else {
            missing.push(id.to_string());
            continue;
        };
        let Some(e) = hit.map(|i| &expect[i]) else {
            continue;
        };
        let want_parent = match e.parent {
            Parent::None => None,
            Parent::Module(p) => Some(id_of.get(&p).copied()),
            Parent::Real(r) => Some(Some(r)),
        };
        if let Some(want) = want_parent {
            if want != stored["HardwareParentId"].as_i64() {
                wrong_parent.push(format!("{} (id {id})", e.name));
            }
        }
        for (k, sent) in &e.props {
            let kept = stored["DeviceProperties"][k].as_str();
            if kept != Some(sent.as_str()) {
                differences.push(json!({"device_id": id, "name": e.name, "key": k,
                    "sent": redact_diff(k, sent, show_secrets),
                    "stored": kept.map(|s| redact_diff(k, s, show_secrets))}));
            }
        }
    }
    let exist = Check::new(
        "every added id exists in the fresh snapshot with the sent name and model",
        missing.is_empty(),
    )
    .detail(format!("missing or different: [{}]", missing.join(", ")));
    let parents = Check::new(
        "children have the new module id as HardwareParentId",
        wrong_parent.is_empty(),
    )
    .detail(format!("wrong parent: [{}]", wrong_parent.join(", ")));
    let grew = Check::new(
        format!("device count grew by {}", expect.len()),
        after.devices.len() == before_count + expect.len(),
    )
    .detail(format!(
        "before {before_count}, after {}",
        after.devices.len()
    ));

    let mut modules = vec![];
    let mut devices = vec![];
    for (a, hit) in &pairs {
        let Some(i) = hit else { continue };
        let e = &expect[*i];
        let mut entry = json!({"id": a["ID"], "name": a["Name"], "model": a["Model"], "placeholder": e.placeholder});
        if matches!(e.parent, Parent::None) {
            modules.push(entry);
        } else {
            entry["parent_id"] = a["HardwareParentId"].clone();
            devices.push(entry);
        }
    }
    let removed = response["RemovedDevices"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|d| json!({"id": d["ID"], "name": d["Name"], "model": d["Model"]}))
        .collect();
    (
        vec![listed, exist, parents, grew],
        Outcome {
            ccu_id: response["CcuId"].as_i64(),
            created: json!({"modules": modules, "devices": devices}),
            removed,
            differences,
        },
    )
}

async fn import(ctx: &Ctx, args: ImportArgs) -> Result<Value, CliError> {
    let p = prepare(&args.file, &args.params)?;
    if p.modbus {
        return Err(bad("Modbus configs use another message class (MessageModbusConfigRequest) and are not supported by pp import"));
    }
    if !p.unresolved.is_empty() {
        return Err(bad(format!(
            "no value and no default for parameter(s): {}. Pass --param ID=VALUE",
            p.unresolved.join(", ")
        ))
        .with_details(json!({"missing": p.unresolved})));
    }
    let notes = variant_notes(&p.config);
    let devices_only = p.config["Modules"].as_array().is_none_or(|m| m.is_empty());
    let config_text = p.config.to_string();
    let expect = expected(&p.config);
    let names: Vec<String> = expect.iter().map(|e| e.name.clone()).collect();
    let diff: Vec<Value> = expect.iter().map(Expect::diff).collect();
    let show_secrets = ctx.global.show_secrets;
    let outcome = Rc::new(RefCell::new(Outcome::default()));
    let (for_build, for_verify) = (outcome.clone(), outcome.clone());
    let operation = if devices_only {
        "addTemplateDevicesToExistingPacketParserModule"
    } else {
        "addPacketParserModule"
    };
    let (ccu_flag, ccu_file) = (args.ccu.clone(), p.ccu_id);
    let mut spec = WriteSpec::new(
        operation,
        "MessageHttpApiConfigRequest",
        args.flags,
        |snapshot| {
            let ccu = pick_ccu(ctx, snapshot, ccu_flag.as_deref(), ccu_file)?;
            for name in &names {
                if let Some(d) = snapshot.devices.iter().find(|d| d["Name"] == json!(name)) {
                    ctx.warn(format!(
                        "a device named \"{name}\" already exists (id {})",
                        d["ID"]
                    ));
                }
            }
            for e in &expect {
                if let Parent::Real(parent) = e.parent {
                    snapshot.device(parent).ok_or_else(|| {
                        CliError::new(
                            "not_found",
                            format!("\"{}\": no device with id {parent} to attach to", e.name),
                        )
                    })?;
                }
            }
            for_build.borrow_mut().ccu_id = Some(ccu);
            let before_count = snapshot.devices.len();
            Ok(WritePlan::new(
                json!({"CcuId": ccu, "ConfigJson": config_text}),
                diff,
                move |after, response| {
                    let (checks, outcome) =
                        verify_import(after, response, &expect, before_count, show_secrets);
                    *for_verify.borrow_mut() = outcome;
                    checks
                },
            ))
        },
    );
    spec.expect = Some("MessageHttpApiConfigResponse");
    spec.variant_verified = notes.is_empty();
    let result = run_write(ctx, spec).await;
    let done = outcome.take();
    let mut doc = match result {
        Ok(doc) => doc,
        Err(mut e) => {
            // After a failed check the caller still needs the ids to clean up.
            if !done.created.is_null() {
                if let Some(details) = e.details.as_object_mut() {
                    details.insert("created".into(), done.created);
                }
            }
            return Err(e);
        }
    };
    if doc["dry_run"] == true {
        doc["variant_notes"] = json!(notes);
        return Ok(doc);
    }
    doc["ccu_id"] = json!(done.ccu_id);
    doc["created"] = done.created;
    if !done.removed.is_empty() {
        doc["removed"] = json!(done.removed);
    }
    if !done.differences.is_empty() {
        ctx.warn(format!(
            "the Core stored {} propert{} differently than sent (it drops or normalises some keys): see property_differences",
            done.differences.len(),
            if done.differences.len() == 1 { "y" } else { "ies" }
        ));
    }
    doc["property_differences"] = json!(done.differences);
    Ok(doc)
}

// ---------------------------------------------------------------- scripts

fn device_ref(d: &Value) -> Value {
    json!({"id": d["ID"], "name": d["Name"], "model": d["Model"]})
}

fn script_keys(d: &Value) -> Vec<&str> {
    keys_of(&d["DeviceProperties"])
        .into_iter()
        .filter(|k| is_script_key(k))
        .collect()
}

fn missing_key(d: &Value, key: &str) -> CliError {
    let keys = script_keys(d);
    bad(format!(
        "device {} has no property \"{key}\". Script keys: {}",
        d["ID"],
        keys.join(", ")
    ))
    .with_details(json!({"script_keys": keys}))
}

async fn script_get(ctx: &Ctx, args: GetArgs) -> Result<Value, CliError> {
    let session = open_session(ctx).await?;
    let snapshot = Snapshot::from_session(&session);
    session.close().await;
    let snapshot = snapshot?;
    let d = snapshot.find_device(&args.device)?;
    let props = &d["DeviceProperties"];
    if let Some(key) = &args.key {
        let Some(text) = props[key.as_str()].as_str() else {
            return Err(missing_key(d, key));
        };
        return Ok(json!({"device": device_ref(d), "key": key,
                         "length": text.chars().count(),
                         "text": mask_value(key, text, ctx.global.show_secrets)}));
    }
    let scripts: Vec<Value> = script_keys(d)
        .into_iter()
        .map(|k| {
            let mut s = script_summary(props[k].as_str().unwrap_or_default());
            s["key"] = json!(k);
            s
        })
        .collect();
    Ok(json!({"device": device_ref(d), "scripts": scripts}))
}

/// Length and the first line that differs from `other`, so a diff never carries a whole script.
fn change_summary(text: &str, other: &str) -> Value {
    let (mut mine, mut theirs) = (text.lines(), other.lines());
    let mut number = 1;
    let line = loop {
        match (mine.next(), theirs.next()) {
            (Some(a), Some(b)) if a == b => number += 1,
            (a, _) => break a.map(shorten),
        }
    };
    json!({"length": text.chars().count(), "first_differing_line": {"number": number, "text": line}})
}

fn read_new_text(args: &SetArgs) -> Result<String, CliError> {
    if let Some(text) = &args.text {
        return Ok(text.clone());
    }
    let Some(path) = &args.file else {
        return Err(bad("pass --file or --text"));
    };
    if path == Path::new("-") {
        let mut text = String::new();
        std::io::stdin()
            .read_to_string(&mut text)
            .map_err(|e| bad(format!("cannot read stdin: {e}")))?;
        return Ok(text);
    }
    std::fs::read_to_string(path).map_err(|e| bad(format!("cannot read {}: {e}", path.display())))
}

async fn script_set(ctx: &Ctx, args: SetArgs) -> Result<Value, CliError> {
    let text = read_new_text(&args)?;
    let (reference, key) = (args.device.clone(), args.key.clone());
    let allow_new = args.allow_new_key;
    let show_secrets = ctx.global.show_secrets;
    let spec = WriteSpec::new(
        "updateMultipleDevicesProperties",
        "MessageUpdateMultipleDevicesPropertiesRequest",
        args.flags,
        |snapshot| {
            let d = snapshot.find_device(&reference)?;
            let id = d["ID"].as_i64().unwrap_or(0);
            let before = d["DeviceProperties"][key.as_str()].as_str();
            if before.is_none() && !allow_new {
                return Err(missing_key(d, &key));
            }
            if before == Some(text.as_str()) {
                // run_write has no "nothing to do" outcome: this code is turned into a result below.
                return Err(CliError::new("pp_unchanged", "the script already has this text")
                    .with_details(json!({"device": device_ref(d), "key": key, "length": text.chars().count()})));
            }
            let (old, new) = if mask_value(&key, "x", show_secrets) == json!("x") {
                (
                    before.map_or(Value::Null, |b| change_summary(b, &text)),
                    change_summary(&text, before.unwrap_or("")),
                )
            } else {
                // a secret script: even its first differing line stays out of the diff
                (
                    before.map_or(Value::Null, |_| json!(REDACTED)),
                    json!(REDACTED),
                )
            };
            let diff = diff_entry("change", "device", json!(id), &key, old, new);
            let (key, text) = (key.clone(), text.clone());
            let sent = json!({"DeviceIds": [id], "DeviceProperties": {key.clone(): text.clone()}});
            Ok(WritePlan::new(sent, vec![diff], move |after, _response| {
                let stored = after
                    .device(id)
                    .and_then(|d| d["DeviceProperties"][key.as_str()].as_str());
                vec![Check::new(
                    "the stored text equals the sent text",
                    stored == Some(text.as_str()),
                )]
            }))
        },
    );
    match run_write(ctx, spec).await {
        Err(e) if e.code == "pp_unchanged" => {
            let mut doc = *e.details;
            doc["changed"] = json!(false);
            Ok(doc)
        }
        Ok(mut doc) => {
            doc["changed"] = json!(true);
            Ok(doc)
        }
        Err(e) => Err(e),
    }
}

// ---------------------------------------------------------------- last run

const TICKS_PER_SECOND: i64 = 10_000_000;
/// .NET ticks at 1970-01-01T00:00:00Z.
const UNIX_EPOCH_TICKS: i64 = 621_355_968_000_000_000;

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * ((m + 9) % 12) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

fn ticks_to_iso(ticks: i64) -> String {
    let since_unix = ticks - UNIX_EPOCH_TICKS;
    let secs = since_unix.div_euclid(TICKS_PER_SECOND);
    let frac = since_unix.rem_euclid(TICKS_PER_SECOND);
    let (y, m, d) = civil_from_days(secs.div_euclid(86_400));
    let s = secs.rem_euclid(86_400);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{frac:07}Z",
        s / 3600,
        s / 60 % 60,
        s % 60
    )
}

/// The codec delivers a DateTime as `YYYY-MM-DDTHH:MM:SS[.fffffff]` (UTC, no zone suffix).
fn iso_to_ticks(text: &str) -> Option<i64> {
    let text = text.strip_suffix('Z').unwrap_or(text);
    let (date, time) = text.split_once('T')?;
    let mut d = date.splitn(3, '-').map(|p| p.parse::<i64>().ok());
    let (y, m, day) = (d.next()??, d.next()??, d.next()??);
    let (clock, frac) = time.split_once('.').unwrap_or((time, ""));
    let mut t = clock.splitn(3, ':').map(|p| p.parse::<i64>().ok());
    let (h, mi, s) = (t.next()??, t.next()??, t.next()??);
    if frac.len() > 7 || !frac.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let frac_ticks = format!("{frac:0<7}").parse::<i64>().ok()?;
    let secs = days_from_civil(y, m, day) * 86_400 + h * 3600 + mi * 60 + s;
    Some(UNIX_EPOCH_TICKS + secs * TICKS_PER_SECOND + frac_ticks)
}

fn finite(x: f64) -> Value {
    if x.is_finite() {
        json!(x)
    } else {
        json!(x.to_string())
    }
}

/// `SerializerValueTypeId` 4 is a double. Any other id is returned untouched: its meaning is not known.
fn decode_data_type(v: &Value) -> Value {
    let type_id = v["SerializerValueTypeId"].as_i64();
    match (type_id, v["SerializerValueBytes"].as_i64()) {
        (Some(4), Some(bits)) => {
            json!({"type_id": 4, "type": "double", "value": finite(f64::from_bits(bits as u64)), "raw": bits})
        }
        _ => json!({"type_id": type_id, "raw": v}),
    }
}

async fn script_last_run(ctx: &Ctx, args: LastRunArgs) -> Result<Value, CliError> {
    let session = open_session(ctx).await?;
    let found = last_run(ctx, &session, &args).await;
    session.close().await;
    found
}

async fn last_run(
    ctx: &Ctx,
    session: &crate::client::Session,
    args: &LastRunArgs,
) -> Result<Value, CliError> {
    let (type_id, kind) = match args.object_kind {
        ObjectKind::Device => (0, "device"),
        ObjectKind::Ccu => (1, "ccu"),
        ObjectKind::Rule => (2, "rule"),
    };
    let (object, name) = if type_id == 0 {
        let snapshot = Snapshot::from_session(session)?;
        let d = snapshot.find_device(&args.target)?;
        (d["ID"].as_i64().unwrap_or(0), d["Name"].clone())
    } else {
        let id = args.target.trim().parse::<i32>().map_err(|_| {
            bad(format!(
                "with --object {kind} the target must be the object's integer id"
            ))
        })?;
        (i64::from(id), Value::Null)
    };
    let key = match (&args.key, type_id) {
        (Some(k), _) => k.clone(),
        (None, 2) => String::new(),
        (None, _) => return Err(bad("--key is required for a device or interface script")),
    };
    let fields =
        json!({"LastRunId": {"ObjectIdType": type_id, "ObjectId": object, "ScriptKey": key}});
    let response = request(
        ctx,
        session,
        "MessageGetScriptLastRunValuesRequest",
        &fields,
        Some("MessageGetScriptLastRunValuesReponse"),
    )
    .await?;
    let values = &response["Values"];
    let has_run = values.as_object().is_some_and(|m| {
        ["Result", "RunTimestampUtc", "ValuesParameter"]
            .iter()
            .any(|k| !m.get(*k).is_none_or(Value::is_null))
    });
    let mut out = json!({"object": {"kind": kind, "type_id": type_id, "id": object, "name": name},
                         "key": key, "has_run": has_run});
    if !has_run {
        return Ok(out);
    }
    if !values["Result"].is_null() {
        out["result"] = decode_data_type(&values["Result"]);
    }
    let stamp = &values["RunTimestampUtc"];
    let ticks = stamp
        .as_i64()
        .or_else(|| stamp.as_str().and_then(iso_to_ticks));
    if let Some(t) = ticks {
        out["run_timestamp_utc"] = json!(ticks_to_iso(t));
        out["run_timestamp_ticks"] = json!(t);
    } else if !stamp.is_null() {
        out["run_timestamp_utc"] = stamp.clone();
    }
    if !values["RunDuration"].is_null() {
        out["run_duration_seconds"] = values["RunDuration"].clone();
    }
    if let Some(phases) = values["ValuesParameter"].as_object() {
        let decoded: Map<String, Value> = phases
            .iter()
            .map(|(phase, vars)| {
                let vars: Map<String, Value> = vars
                    .as_object()
                    .into_iter()
                    .flatten()
                    .map(|(n, v)| (n.clone(), decode_data_type(v)))
                    .collect();
                (phase.clone(), Value::Object(vars))
            })
            .collect();
        out["variables_by_phase"] = Value::Object(decoded);
    }
    Ok(out)
}
