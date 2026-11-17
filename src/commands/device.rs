//! `taphome device list | get | set-props | rename | delete | add-virtual`.
use crate::core::{
    diff_entry, open_session, run_write, Check, Ctx, Snapshot, WriteFlags, WritePlan, WriteSpec,
};
use crate::errors::{CliError, Result};
use crate::output::REDACTED;
use clap::{Args, Subcommand};
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// Model to interface pairs that docs/configuration-model.md states with certainty.
const VIRTUAL_INTERFACES: [(&str, &str); 7] = [
    ("VirtualVariableGeneral", "Variable"),
    ("VirtualSwitch", "Switch"),
    ("VirtualThermostatDummy", "Thermostat"),
    ("VirtualAnalogOutput", "Dimmer"),
    ("VirtualPushButton", "PushButton"),
    ("VirtualReedContact", "ReedContact"),
    ("VirtualMultiValueSwitch", "MultiValueSwitch"),
];

/// Sentinel from a build closure that finds nothing to send. `run_write` has no "skip" outcome.
const NO_CHANGE: &str = "no_change";

#[derive(Subcommand)]
pub enum Cmd {
    /// List devices as compact rows
    List(ListArgs),
    /// Show one device with properties, current values and capabilities
    Get(GetArgs),
    /// Change device properties (write)
    SetProps(SetPropsArgs),
    /// Rename a device (write)
    Rename(RenameArgs),
    /// Delete a device (write)
    Delete(DeleteArgs),
    /// Create a virtual device (write)
    AddVirtual(AddVirtualArgs),
}

#[derive(Args)]
pub struct ListArgs {
    /// Model pattern, `*` matches any text, case-insensitive
    #[arg(long, value_name = "GLOB")]
    model: Option<String>,
    /// Case-insensitive part of the name
    #[arg(long, value_name = "SUBSTR")]
    name: Option<String>,
    /// Only children of this device (HardwareParentId)
    #[arg(long, value_name = "ID")]
    parent: Option<i64>,
    /// Only devices of this interface (CcuId)
    #[arg(long, value_name = "ID")]
    ccu: Option<i64>,
}

#[derive(Args)]
pub struct GetArgs {
    /// Device id or exact unique name
    device: String,
}

#[derive(Args)]
pub struct SetPropsArgs {
    /// Device id or exact unique name
    device: String,
    /// Property to set, split at the first `=`. Repeatable, wins over --props-file
    #[arg(long = "prop", value_name = "K=V")]
    props: Vec<String>,
    /// JSON object of string values
    #[arg(long, value_name = "FILE")]
    props_file: Option<PathBuf>,
    /// Allow a key the device does not have today (typo guard)
    #[arg(long)]
    allow_new_key: bool,
    #[command(flatten)]
    flags: WriteFlags,
}

#[derive(Args)]
pub struct RenameArgs {
    /// Device id or exact unique name
    device: String,
    /// New name
    name: String,
    #[command(flatten)]
    flags: WriteFlags,
}

#[derive(Args)]
pub struct DeleteArgs {
    /// Device id or exact unique name
    device: String,
    /// Delete although other devices hang under it (never tested on a real Core, needs --allow-unverified)
    #[arg(long)]
    force: bool,
    #[command(flatten)]
    flags: WriteFlags,
}

#[derive(Args)]
pub struct AddVirtualArgs {
    /// Model, for example VirtualVariableGeneral
    #[arg(long)]
    model: String,
    /// Device name
    #[arg(long)]
    name: String,
    /// Interface class name, required for a model without a known default
    #[arg(long)]
    interface: Option<String>,
    /// Initial property, split at the first `=`. Repeatable
    #[arg(long = "prop", value_name = "K=V")]
    props: Vec<String>,
    #[command(flatten)]
    flags: WriteFlags,
}

pub async fn run(ctx: &Ctx, cmd: Cmd) -> Result<Value> {
    match cmd {
        Cmd::List(args) => list(ctx, args).await,
        Cmd::Get(args) => get(ctx, args).await,
        Cmd::SetProps(args) => set_props(ctx, args).await,
        Cmd::Rename(args) => {
            let props = BTreeMap::from([("Name".to_string(), args.name)]);
            update_props(ctx, args.flags, &args.device, props, false).await
        }
        Cmd::Delete(args) => delete(ctx, args).await,
        Cmd::AddVirtual(args) => add_virtual(ctx, args).await,
    }
}

async fn snapshot(ctx: &Ctx) -> Result<Snapshot> {
    let session = open_session(ctx).await?;
    let snapshot = Snapshot::from_session(&session);
    session.close().await;
    snapshot
}

fn id_of(device: &Value) -> i64 {
    device["ID"].as_i64().unwrap_or(0)
}

// ---------------------------------------------------------------- read

/// `*` glob, case-insensitive. Without `*` the whole text must match.
fn glob_match(pattern: &str, text: &str) -> bool {
    let (pattern, text) = (pattern.to_lowercase(), text.to_lowercase());
    let parts: Vec<&str> = pattern.split('*').collect();
    let (first, last) = (parts[0], parts[parts.len() - 1]);
    if parts.len() == 1 {
        return pattern == text;
    }
    if text.len() < first.len() + last.len() || !text.starts_with(first) || !text.ends_with(last) {
        return false;
    }
    let mut rest = &text[first.len()..text.len() - last.len()];
    for part in &parts[1..parts.len() - 1] {
        match rest.find(part) {
            Some(at) => rest = &rest[at + part.len()..],
            None => return false,
        }
    }
    true
}

async fn list(ctx: &Ctx, args: ListArgs) -> Result<Value> {
    let snapshot = snapshot(ctx).await?;
    let needle = args.name.as_deref().map(str::to_lowercase);
    let rows: Vec<Value> = snapshot
        .devices
        .iter()
        .filter(|d| {
            args.model
                .as_deref()
                .is_none_or(|g| glob_match(g, d["Model"].as_str().unwrap_or_default()))
                && needle.as_deref().is_none_or(|n| {
                    d["Name"]
                        .as_str()
                        .unwrap_or_default()
                        .to_lowercase()
                        .contains(n)
                })
                && args
                    .parent
                    .is_none_or(|p| d["HardwareParentId"].as_i64() == Some(p))
                && args.ccu.is_none_or(|c| d["CcuId"].as_i64() == Some(c))
        })
        .map(|d| {
            json!({"ID": d["ID"], "Name": d["Name"], "Model": d["Model"], "Interface": d["Interface"], "CcuId": d["CcuId"],
                   "HardwareParentId": d["HardwareParentId"], "SerialNumber": d["SerialNumber"], "DeviceType": d["DeviceType"]})
        })
        .collect();
    Ok(json!({"count": rows.len(), "devices": rows}))
}

async fn get(ctx: &Ctx, args: GetArgs) -> Result<Value> {
    let snapshot = snapshot(ctx).await?;
    let device = snapshot.find_device(&args.device)?;
    Ok(json!({"device": device}))
}

// ---------------------------------------------------------------- write helpers

fn parse_kv(text: &str) -> Result<(String, String)> {
    match text.split_once('=') {
        Some((key, value)) if !key.is_empty() => Ok((key.to_string(), value.to_string())),
        _ => Err(CliError::invalid_input(format!(
            "\"{text}\" is not K=V with a non-empty key"
        ))),
    }
}

fn parse_props(items: &[String]) -> Result<BTreeMap<String, String>> {
    items.iter().map(|i| parse_kv(i)).collect()
}

fn read_props_file(path: &PathBuf) -> Result<BTreeMap<String, String>> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| CliError::invalid_input(format!("cannot read --props-file: {e}")))?;
    let Value::Object(map) = serde_json::from_str::<Value>(&text)
        .map_err(|e| CliError::invalid_input(format!("--props-file is not valid JSON: {e}")))?
    else {
        return Err(CliError::invalid_input(
            "--props-file must hold a JSON object",
        ));
    };
    map.into_iter()
        .map(|(k, v)| match v {
            Value::String(s) if !k.is_empty() => Ok((k, s)),
            _ => Err(CliError::invalid_input(format!(
                "--props-file: the value of \"{k}\" must be a string and the key non-empty"
            ))),
        })
        .collect()
}

/// Property value as the Core holds it: `Name` is a top-level field, the rest lives in `DeviceProperties`.
fn current_value(device: &Value, key: &str) -> Option<String> {
    let value = if key == "Name" {
        &device["Name"]
    } else {
        &device["DeviceProperties"][key]
    };
    value.as_str().map(str::to_string)
}

fn is_secret_key(key: &str) -> bool {
    let k = key.to_ascii_lowercase();
    k.contains("password") || k.contains("token")
}

/// The output redactor only sees maps named `*Properties`: diff values need their own masking.
fn shown(ctx: &Ctx, key: &str, value: Option<&str>) -> Value {
    match value {
        None => Value::Null,
        Some(v) if !v.is_empty() && !ctx.global.show_secrets && is_secret_key(key) => {
            json!(REDACTED)
        }
        Some(v) => json!(v),
    }
}

fn levenshtein(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut diagonal = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let above = row[j + 1];
            row[j + 1] = (diagonal + usize::from(ca != *cb))
                .min(above + 1)
                .min(row[j] + 1);
            diagonal = above;
        }
    }
    row[b.len()]
}

fn close_keys(device: &Value, key: &str) -> Vec<String> {
    let wanted = key.to_lowercase();
    device["DeviceProperties"]
        .as_object()
        .map(|m| {
            m.keys()
                .filter(|k| {
                    let k = k.to_lowercase();
                    levenshtein(&k, &wanted) <= 2
                        || (wanted.len() >= 3 && (k.contains(&wanted) || wanted.contains(&k)))
                })
                .cloned()
                .collect()
        })
        .unwrap_or_default()
}

/// Adds the diff to a real write result. A dry run already carries it.
fn with_diff(mut doc: Value, diff: Vec<Value>) -> Value {
    if doc["dry_run"] != json!(true) {
        doc["diff"] = json!(diff);
    }
    doc
}

// ---------------------------------------------------------------- set-props, rename

async fn set_props(ctx: &Ctx, args: SetPropsArgs) -> Result<Value> {
    let mut props = match &args.props_file {
        Some(path) => read_props_file(path)?,
        None => BTreeMap::new(),
    };
    props.extend(parse_props(&args.props)?);
    if props.is_empty() {
        return Err(CliError::invalid_input(
            "give at least one --prop K=V or a --props-file",
        ));
    }
    update_props(ctx, args.flags, &args.device, props, args.allow_new_key).await
}

async fn update_props(
    ctx: &Ctx,
    flags: WriteFlags,
    reference: &str,
    wanted: BTreeMap<String, String>,
    allow_new_key: bool,
) -> Result<Value> {
    let mut diff_out: Vec<Value> = vec![];
    let sink = &mut diff_out;
    let spec = WriteSpec::new(
        "updateMultipleDevicesProperties",
        "MessageUpdateMultipleDevicesPropertiesRequest",
        flags,
        move |snapshot| {
            let device = snapshot.find_device(reference)?;
            let id = id_of(device);
            if !allow_new_key {
                for key in wanted.keys() {
                    if current_value(device, key).is_none() {
                        let close = close_keys(device, key);
                        return Err(CliError::invalid_input(format!(
                            "device {id} has no property \"{key}\"{}: pass --allow-new-key to create it",
                            if close.is_empty() {
                                String::new()
                            } else {
                                format!(", did you mean {}?", close.join(", "))
                            }
                        ))
                        .with_details(json!({"key": key, "close_keys": close})));
                    }
                }
            }
            let (send, same): (Vec<_>, Vec<_>) = wanted
                .into_iter()
                .partition(|(k, v)| current_value(device, k).as_deref() != Some(v));
            if send.is_empty() {
                return Err(CliError::new(NO_CHANGE, "every value is already current"));
            }
            if !same.is_empty() {
                let keys: Vec<&str> = same.iter().map(|(k, _)| k.as_str()).collect();
                ctx.warn(format!(
                    "not sent, value already current: {}",
                    keys.join(", ")
                ));
            }
            let diff: Vec<Value> = send
                .iter()
                .map(|(k, v)| {
                    diff_entry(
                        "change",
                        "device",
                        json!(id),
                        k,
                        shown(ctx, k, current_value(device, k).as_deref()),
                        shown(ctx, k, Some(v)),
                    )
                })
                .collect();
            sink.extend(diff.iter().cloned());
            let sent: Map<String, Value> =
                send.iter().map(|(k, v)| (k.clone(), json!(v))).collect();
            let fields = json!({"DeviceIds": [id], "DeviceProperties": sent});
            Ok(WritePlan::new(fields, diff, move |after, _response| {
                send.iter()
                    .map(|(key, value)| {
                        let ok = after
                            .device(id)
                            .and_then(|d| current_value(d, key))
                            .as_deref()
                            == Some(value.as_str());
                        Check::new(format!("{key} reads back with the sent value"), ok)
                    })
                    .collect()
            }))
        },
    );
    match run_write(ctx, spec).await {
        Ok(doc) => Ok(with_diff(doc, diff_out)),
        Err(e) if e.code == NO_CHANGE => Ok(json!({"changed": false})),
        Err(e) => Err(e),
    }
}

// ---------------------------------------------------------------- delete

/// True when a decoded smart rule names the device: an integer under a key containing "DeviceId", or an
/// integer array under a key containing "DeviceIds" or "SensorIds".
fn refers_to(value: &Value, id: i64) -> bool {
    match value {
        Value::Object(map) => map.iter().any(|(key, v)| {
            let hit = match v {
                Value::Array(items) if key.contains("DeviceIds") || key.contains("SensorIds") => {
                    items.iter().any(|i| i.as_i64() == Some(id))
                }
                _ if key.contains("DeviceId") => v.as_i64() == Some(id),
                _ => false,
            };
            hit || refers_to(v, id)
        }),
        Value::Array(items) => items.iter().any(|v| refers_to(v, id)),
        _ => false,
    }
}

async fn delete(ctx: &Ctx, args: DeleteArgs) -> Result<Value> {
    let force = args.force;
    let mut diff_out: Vec<Value> = vec![];
    let sink = &mut diff_out;
    let mut spec = WriteSpec::new(
        "deleteDevice",
        "MessageDeleteDeviceRequest",
        args.flags,
        |snapshot| {
            let device = snapshot.find_device(&args.device)?;
            let id = id_of(device);
            let children: Vec<i64> = snapshot
                .devices
                .iter()
                .filter(|d| d["HardwareParentId"].as_i64() == Some(id))
                .map(id_of)
                .collect();
            if !children.is_empty() && !force {
                return Err(CliError::invalid_input(format!(
                    "device {id} has children {children:?}: delete them first (or --force, unverified)"
                ))
                .with_details(json!({"children": children})));
            }
            let rules: Vec<String> = snapshot
                .live_rules()
                .filter(|r| refers_to(&r.model, id))
                .map(|r| format!("{} \"{}\"", r.id, r.name))
                .collect();
            if !rules.is_empty() {
                ctx.warn(format!(
                    "device {id} is referenced by smart rules: {}",
                    rules.join(", ")
                ));
            }
            let (name, model, serial) = (
                device["Name"].clone(),
                device["Model"].clone(),
                device["SerialNumber"].clone(),
            );
            let diff = vec![diff_entry(
                "remove",
                "device",
                json!(id),
                "device",
                json!({"name": name, "model": model, "serial": serial}),
                Value::Null,
            )];
            sink.extend(diff.iter().cloned());
            let before_count = snapshot.devices.len();
            let has_children = !children.is_empty();
            Ok(WritePlan::new(
                json!({"DeviceId": id, "ForceDelete": force}),
                diff,
                move |after, _response| {
                    // Ids are reused: the id may be back, but then with another serial.
                    let gone = after.device(id).is_none_or(|d| d["SerialNumber"] != serial);
                    let count = after.devices.len();
                    // Whether force takes the children along was never observed.
                    let dropped = if has_children {
                        count < before_count
                    } else {
                        count + 1 == before_count
                    };
                    vec![
                        Check::new("the device is gone", gone),
                        Check::new("the device count dropped", dropped)
                            .detail(format!("{before_count} before, {count} after")),
                    ]
                },
            ))
        },
    );
    spec.variant_verified = !force;
    let doc = run_write(ctx, spec).await?;
    Ok(with_diff(doc, diff_out))
}

// ---------------------------------------------------------------- add-virtual

async fn add_virtual(ctx: &Ctx, args: AddVirtualArgs) -> Result<Value> {
    let props = parse_props(&args.props)?;
    let interface = match (
        &args.interface,
        VIRTUAL_INTERFACES.iter().find(|(m, _)| *m == args.model),
    ) {
        (Some(i), _) => i.clone(),
        (None, Some((_, i))) => i.to_string(),
        (None, None) => {
            let known: Vec<&str> = VIRTUAL_INTERFACES.iter().map(|(m, _)| *m).collect();
            return Err(CliError::invalid_input(format!(
                "no known interface for model \"{}\": pass --interface (models with a default: {})",
                args.model,
                known.join(", ")
            )));
        }
    };
    let verified_variant =
        args.model == "VirtualVariableGeneral" && interface == "Variable" && props.is_empty();
    let operation = if args.model == "VirtualVariableGeneral" {
        "addVirtualVariable"
    } else {
        "addVirtualDevice"
    };
    let (name, model) = (args.name.clone(), args.model.clone());
    let mut diff_out: Vec<Value> = vec![];
    let sink = &mut diff_out;
    let mut spec = WriteSpec::new(
        operation,
        "MessageAddDeviceRequest",
        args.flags,
        move |snapshot| {
            if snapshot
                .devices
                .iter()
                .any(|d| d["Name"].as_str() == Some(name.as_str()))
            {
                ctx.warn(format!(
                    "another device is already named \"{name}\": addressing it by name becomes ambiguous"
                ));
            }
            let mut fields = json!({"DeviceName": name, "Icon": 0, "Interface": interface, "Model": model, "DeviceTypeId": 0});
            if !props.is_empty() {
                fields["DeviceProperties"] = json!(props);
            }
            let shown_props: Map<String, Value> = props
                .iter()
                .map(|(k, v)| (k.clone(), shown(ctx, k, Some(v))))
                .collect();
            let diff = vec![diff_entry(
                "add",
                "device",
                Value::Null,
                "device",
                Value::Null,
                json!({"name": name, "model": model, "interface": interface, "properties": shown_props}),
            )];
            sink.extend(diff.iter().cloned());
            let before_count = snapshot.devices.len();
            Ok(WritePlan::new(fields, diff, move |after, response| {
                let added = &response["AddedDevice"];
                let id = id_of(added);
                let found = after.device(id);
                let matches =
                    found.is_some_and(|d| d["Name"] == json!(name) && d["Model"] == json!(model));
                let count = after.devices.len();
                vec![
                    Check::new("the answer names the new device", id != 0),
                    Check::new("the device exists with the sent name and model", matches),
                    Check::new("the device count grew by one", count == before_count + 1)
                        .detail(format!("{before_count} before, {count} after")),
                ]
            }))
        },
    );
    spec.expect = Some("MessageAddDeviceResponse");
    spec.variant_verified = verified_variant;
    let mut doc = run_write(ctx, spec).await?;
    if doc["dry_run"] != json!(true) {
        let added = doc["response"]["AddedDevice"].clone();
        let keys: Vec<&String> = added["DeviceProperties"]
            .as_object()
            .map(|m| m.keys().collect())
            .unwrap_or_default();
        doc["device"] =
            json!({"id": added["ID"], "serial": added["SerialNumber"], "property_keys": keys});
    }
    Ok(with_diff(doc, diff_out))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_matches_case_insensitive_with_stars() {
        assert!(glob_match("Virtual*", "VirtualSwitch"));
        assert!(glob_match("*switch", "VirtualSwitch"));
        assert!(glob_match("v*t*h", "VirtualSwitch"));
        assert!(glob_match("*", ""));
        assert!(glob_match("virtualswitch", "VirtualSwitch"));
        assert!(!glob_match("Virtual", "VirtualSwitch"));
        assert!(!glob_match("a*ab", "ab"));
        assert!(!glob_match("*x*", "VirtualSwitch"));
    }

    #[test]
    fn rule_references_cover_ids_and_id_lists() {
        let rule = json!({"A": [{"DeviceId": 7}], "B": {"SensorIds": [1, 9]}, "C": {"InputDeviceIds": [3]}});
        assert!(refers_to(&rule, 7) && refers_to(&rule, 9) && refers_to(&rule, 3));
        assert!(!refers_to(&rule, 8));
        assert!(!refers_to(&json!({"Value": 7}), 7));
    }

    #[test]
    fn distance_counts_edits() {
        assert_eq!(levenshtein("iconid", "iconid"), 0);
        assert_eq!(levenshtein("iconid", "icnid"), 1);
        assert_eq!(levenshtein("", "ab"), 2);
    }
}
