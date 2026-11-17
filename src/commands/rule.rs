//! `taphome rule list | get | apply | enable | disable | delete | errors`: smart rules (`TriggerModel`).
//!
//! Every write sends whole models in `MessageUpdateTriggerModelsRequest` (`TriggerModels` holds
//! `TriggerModel` objects, not serialized bytes) or ids in `MessageDeleteTriggerModelsRequest`.
//! Only the create of a disabled formula rule (type 12) ran against a real Core. Update, enable,
//! disable and every other create are gated behind `--allow-unverified`.
use crate::catalog;
use crate::codec;
use crate::core::{
    diff_entry, open_session, request, run_write, Check, Ctx, Rule, Snapshot, WriteFlags,
    WritePlan, WriteSpec,
};
use crate::errors::{CliError, EXIT_GATE};
use crate::output::hex;
use clap::{Args, Subcommand};
use serde_json::{json, Map, Value};
use std::cell::Cell;
use std::io::Read;
use std::rc::Rc;

const UPSERT: &str = "MessageUpdateTriggerModelsRequest";
const FORMULA_TYPE: i64 = 12;
/// Fields the Core fills or derives. They are not diffed and not compared after a write.
const MANAGED: [&str; 4] = ["ChangesetId", "TriggerInputs", "Priorities", "IsDeleted"];
/// Marks "nothing to send" inside `run_write`'s build step, turned into `{"changed": false}`.
const NOOP: &str = "rule_noop";

#[derive(Subcommand)]
pub enum Cmd {
    /// List live rules (tombstones with --all)
    List(ListArgs),
    /// Print the decoded model, in the form `rule apply` accepts
    Get { rule: String },
    /// Create (Id 0 or absent) or update (Id of a live rule) a rule from a JSON file
    Apply(ApplyArgs),
    /// Enable a rule (unverified variant)
    Enable(ToggleArgs),
    /// Disable a rule (unverified variant)
    Disable(ToggleArgs),
    /// Delete a rule (the Core keeps a tombstone)
    Delete(ToggleArgs),
    /// Errors the Core reports for a rule
    Errors { rule: String },
}

#[derive(Args)]
pub struct ListArgs {
    /// Include deleted rules (tombstones)
    #[arg(long)]
    all: bool,
    /// Only rules of this trigger type id
    #[arg(long = "type", value_name = "ID")]
    type_id: Option<i64>,
    /// Only rules whose name contains this text
    #[arg(long, value_name = "SUBSTR")]
    name: Option<String>,
}

#[derive(Args)]
pub struct ApplyArgs {
    /// File with one TriggerModel object, or - for stdin
    file: String,
    #[command(flatten)]
    flags: WriteFlags,
}

#[derive(Args)]
pub struct ToggleArgs {
    /// Rule id or exact name
    rule: String,
    #[command(flatten)]
    flags: WriteFlags,
}

pub async fn run(ctx: &Ctx, cmd: Cmd) -> Result<Value, CliError> {
    match cmd {
        Cmd::List(args) => list(ctx, args).await,
        Cmd::Get { rule } => get(ctx, &rule).await,
        Cmd::Apply(args) => apply(ctx, args).await,
        Cmd::Enable(a) => set_enabled(ctx, a, true).await,
        Cmd::Disable(a) => set_enabled(ctx, a, false).await,
        Cmd::Delete(a) => delete(ctx, a).await,
        Cmd::Errors { rule } => errors(ctx, &rule).await,
    }
}

// ---------------------------------------------------------------- reads

async fn snapshot(ctx: &Ctx) -> Result<Snapshot, CliError> {
    let session = open_session(ctx).await?;
    let snapshot = Snapshot::from_session(&session);
    session.close().await;
    snapshot
}

fn type_name(type_id: i64) -> Value {
    json!(catalog::enum_name("TriggerType", type_id))
}

fn summary(rule: &Rule) -> Value {
    json!({"id": rule.id, "name": rule.name, "type_id": rule.type_id, "type_name": type_name(rule.type_id),
           "version": rule.version, "enabled": rule.enabled, "changeset_id": rule.model["ChangesetId"],
           "deleted": rule.deleted})
}

async fn list(ctx: &Ctx, args: ListArgs) -> Result<Value, CliError> {
    let snapshot = snapshot(ctx).await?;
    let rules: Vec<Value> = snapshot
        .rules
        .iter()
        .filter(|r| args.all || !r.deleted)
        .filter(|r| args.type_id.is_none_or(|t| r.type_id == t))
        .filter(|r| args.name.as_deref().is_none_or(|n| r.name.contains(n)))
        .map(summary)
        .collect();
    Ok(json!({"count": rules.len(), "rules": rules}))
}

async fn get(ctx: &Ctx, reference: &str) -> Result<Value, CliError> {
    let snapshot = snapshot(ctx).await?;
    let rule = snapshot.find_rule(reference)?;
    // `model` is the decoded object unchanged, so `jq .data.model` is a valid `rule apply` file.
    Ok(
        json!({"id": rule.id, "name": rule.name, "type_id": rule.type_id, "type_name": type_name(rule.type_id),
              "model": rule.model}),
    )
}

/// TapHome time is seconds since 2016-01-01 UTC.
fn taphome_time_iso(seconds: i64) -> String {
    const EPOCH_2016: i64 = 1_451_606_400;
    let unix = EPOCH_2016 + seconds;
    let (days, rest) = (unix.div_euclid(86_400), unix.rem_euclid(86_400));
    // Civil date from a day count (H. Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    )
}

async fn errors(ctx: &Ctx, reference: &str) -> Result<Value, CliError> {
    let session = open_session(ctx).await?;
    let result = async {
        let snapshot = Snapshot::from_session(&session)?;
        let rule = snapshot.find_rule(reference)?;
        let response = request(
            ctx,
            &session,
            "MessageGetTriggerErrorsRequest",
            &json!({"TriggerId": rule.id, "ShouldSubscribe": false}),
            Some("MessageGetTriggerErrorsResponse"),
        )
        .await?;
        Ok::<_, CliError>((rule.id, rule.name.clone(), response))
    }
    .await;
    session.close().await;
    let (id, name, response) = result?;
    let errors: Vec<Value> = response["TriggerErrors"]
        .as_array()
        .map(|list| list.iter().map(describe_error).collect())
        .unwrap_or_default();
    Ok(json!({"rule": {"id": id, "name": name}, "count": errors.len(), "errors": errors}))
}

fn describe_error(error: &Value) -> Value {
    let mut out = error.clone();
    let code = error["ErrorCodeId"].as_i64().unwrap_or(0);
    out["ErrorCodeName"] = json!(match code {
        201 => Some("FormulaParseException"),
        202 => Some("FormulaEvaluationException"),
        203 => Some("LiteralArgumentException"),
        _ => None,
    });
    out["SeverityName"] = json!(match error["SeverityId"].as_i64().unwrap_or(0) {
        1 => Some("Info"),
        2 => Some("Warning"),
        3 => Some("Error"),
        _ => None,
    });
    // The decoder leaves out a zero, which is the TapHome epoch itself.
    let seconds = error["TapHomeTime"].as_i64().unwrap_or(0);
    out["TapHomeTime"] = json!(seconds);
    out["TimeIso"] = json!(taphome_time_iso(seconds));
    out
}

// ---------------------------------------------------------------- model helpers

/// The decoded model re-encoded must give the bytes the Core sent, else the codec would lose data.
fn roundtrip_gate(rule: &Rule) -> Result<(), CliError> {
    let mismatch = |why: String| {
        CliError::new(
            "roundtrip_mismatch",
            format!(
                "rule {} does not survive decode and encode ({why}): nothing was sent",
                rule.id
            ),
        )
        .with_exit(EXIT_GATE)
        .with_details(json!({"rule_id": rule.id}))
    };
    match codec::encode("TriggerModel", &rule.model) {
        Ok(bytes) if hex(&bytes).eq_ignore_ascii_case(&rule.raw_hex) => Ok(()),
        Ok(bytes) => Err(mismatch(format!(
            "{} bytes sent by the Core, {} bytes encoded",
            rule.raw_hex.len() / 2,
            bytes.len()
        ))),
        Err(e) => Err(mismatch(e.to_string())),
    }
}

fn encode_check(model: &Value) -> Result<(), CliError> {
    codec::encode("TriggerModel", model)
        .map(|_| ())
        .map_err(|e| CliError::invalid_input(format!("the rule does not encode: {e}")))
}

fn read_model(file: &str) -> Result<Map<String, Value>, CliError> {
    let text = if file == "-" {
        let mut text = String::new();
        std::io::stdin()
            .read_to_string(&mut text)
            .map_err(|e| CliError::invalid_input(format!("cannot read stdin: {e}")))?;
        text
    } else {
        std::fs::read_to_string(file)
            .map_err(|e| CliError::invalid_input(format!("cannot read {file}: {e}")))?
    };
    match serde_json::from_str::<Value>(&text) {
        Ok(Value::Object(map)) => Ok(map),
        Ok(_) => Err(CliError::invalid_input(
            "the file must hold one TriggerModel JSON object",
        )),
        Err(e) => Err(CliError::invalid_input(format!(
            "the file is not valid JSON: {e}"
        ))),
    }
}

fn has_params_type(model: &Map<String, Value>) -> bool {
    model["TriggerParameters"]["$type"].is_string()
}

/// `TriggerParameters."$type"` names the concrete class and no table maps type ids to classes.
fn require_params_type(
    snapshot: &Snapshot,
    model: &Map<String, Value>,
    type_id: i64,
) -> Result<(), CliError> {
    if has_params_type(model) {
        return Ok(());
    }
    let mut seen: Vec<&str> = snapshot
        .live_rules()
        .filter(|r| r.type_id == type_id)
        .filter_map(|r| r.model["TriggerParameters"]["$type"].as_str())
        .collect();
    seen.sort_unstable();
    seen.dedup();
    let hint = if seen.is_empty() {
        "The Core has no live rule of this type to copy it from.".to_string()
    } else {
        format!("Seen on this Core for type {type_id}: {}.", seen.join(", "))
    };
    Err(CliError::invalid_input(format!(
        "TriggerParameters must hold \"$type\", the concrete parameters class. Copy it from `taphome rule get` of an existing rule of type {type_id}. {hint}"
    ))
    .with_details(json!({"type_id": type_id, "seen_types": seen})))
}

fn is_default(v: &Value) -> bool {
    match v {
        Value::Null => true,
        Value::Bool(b) => !b,
        Value::Number(n) => n.as_f64() == Some(0.0),
        Value::String(s) => s.is_empty(),
        Value::Array(a) => a.is_empty(),
        Value::Object(o) => o.is_empty(),
    }
}

/// True when everything in `sent` reads back in `stored`. The decoder leaves out fields that hold
/// their default, so a default-like sent value also matches an absent one.
fn covers(stored: &Value, sent: &Value) -> bool {
    match (stored, sent) {
        (_, s) if is_default(s) && is_default(stored) => true,
        (Value::Object(st), Value::Object(se)) => se
            .iter()
            .filter(|(k, _)| *k != "$errors")
            .all(|(k, v)| covers(st.get(k).unwrap_or(&Value::Null), v)),
        (Value::Array(st), Value::Array(se)) => {
            st.len() == se.len() && st.iter().zip(se).all(|(a, b)| covers(a, b))
        }
        (Value::Number(a), Value::Number(b)) => a.as_f64() == b.as_f64(),
        (a, b) => a == b,
    }
}

fn without_managed(model: &Value) -> Value {
    let mut model = model.clone();
    if let Some(map) = model.as_object_mut() {
        for key in MANAGED {
            map.remove(key);
        }
    }
    model
}

/// `change` entries between two models, one per differing leaf, Core-managed fields ignored.
fn model_diff(id: i64, before: &Value, after: &Value) -> Vec<Value> {
    fn walk(id: i64, path: &str, before: &Value, after: &Value, out: &mut Vec<Value>) {
        match (before, after) {
            (Value::Object(b), Value::Object(a)) => {
                let mut keys: Vec<&String> = b.keys().chain(a.keys()).collect();
                keys.sort();
                keys.dedup();
                for key in keys {
                    let sub = if path.is_empty() {
                        key.clone()
                    } else {
                        format!("{path}.{key}")
                    };
                    walk(
                        id,
                        &sub,
                        b.get(key).unwrap_or(&Value::Null),
                        a.get(key).unwrap_or(&Value::Null),
                        out,
                    );
                }
            }
            (Value::Array(b), Value::Array(a)) if b.len() == a.len() => {
                for (i, (x, y)) in b.iter().zip(a).enumerate() {
                    walk(id, &format!("{path}[{i}]"), x, y, out);
                }
            }
            _ if before == after => {}
            _ => out.push(diff_entry(
                "change",
                "rule",
                json!(id),
                path,
                before.clone(),
                after.clone(),
            )),
        }
    }
    let mut out = vec![];
    walk(
        id,
        "",
        &without_managed(before),
        &without_managed(after),
        &mut out,
    );
    out
}

fn noop(details: Value) -> CliError {
    CliError::new(NOOP, "nothing to change").with_details(details)
}

/// Turns the build step's no-op marker into `{"changed": false}` (also for `--dry-run`).
fn finish(result: Result<Value, CliError>, rule_id: i64) -> Result<Value, CliError> {
    match result {
        Err(e) if e.code == NOOP => {
            Ok(json!({"changed": false, "rule_id": rule_id, "reason": *e.details}))
        }
        Ok(mut doc) => {
            if doc.get("dry_run").is_none() {
                doc["changed"] = json!(true);
                doc["rule_id"] = json!(rule_id);
            }
            Ok(doc)
        }
        err => err,
    }
}

// ---------------------------------------------------------------- apply

async fn apply(ctx: &Ctx, args: ApplyArgs) -> Result<Value, CliError> {
    let model = read_model(&args.file)?;
    let id = match model.get("Id") {
        None | Some(Value::Null) => 0,
        Some(v) => v
            .as_i64()
            .ok_or_else(|| CliError::invalid_input("Id must be an integer"))?,
    };
    if has_params_type(&model) {
        encode_check(&Value::Object(model.clone()))?;
    }
    if id != 0 {
        let mut spec = WriteSpec::new("upsertSmartRules", UPSERT, args.flags, move |s| {
            plan_update(s, id, model)
        });
        spec.variant_verified = false;
        return finish(run_write(ctx, spec).await, id);
    }
    let verified_variant = model["TriggerTypeId"].as_i64() == Some(FORMULA_TYPE)
        && model["IsEnabled"].as_bool() != Some(true);
    let created = Rc::new(Cell::new(None));
    let slot = created.clone();
    let mut spec = WriteSpec::new("upsertSmartRules", UPSERT, args.flags, move |s| {
        plan_create(s, model, slot)
    });
    spec.variant_verified = verified_variant;
    let mut doc = run_write(ctx, spec).await?;
    if doc.get("dry_run").is_none() {
        doc["changed"] = json!(true);
        doc["rule_id"] = json!(created.get());
    }
    Ok(doc)
}

fn plan_create(
    snapshot: &Snapshot,
    mut model: Map<String, Value>,
    created: Rc<Cell<Option<i64>>>,
) -> Result<WritePlan, CliError> {
    let name = match model.get("Name").and_then(Value::as_str) {
        Some(n) if !n.is_empty() => n.to_string(),
        _ => return Err(CliError::invalid_input("Name must be a non-empty string")),
    };
    let type_id = model
        .get("TriggerTypeId")
        .and_then(Value::as_i64)
        .ok_or_else(|| CliError::invalid_input("TriggerTypeId must be an integer"))?;
    // The new rule is found by name afterwards, so a taken name is refused here.
    if let Some(existing) = snapshot.live_rules().find(|r| r.name == name) {
        return Err(CliError::new(
            "already_exist",
            format!("a live rule named \"{name}\" exists (id {}): pick another name or update it with its Id", existing.id),
        )
        .with_exit(EXIT_GATE)
        .with_details(json!({"existing_id": existing.id})));
    }
    let version = match (snapshot.trigger_version(type_id), model.get("Version")) {
        (Some(core), Some(v)) if v.as_i64() != Some(core) => {
            return Err(CliError::invalid_input(format!(
                "Version {v} does not match the Core's version {core} for type {type_id}: drop Version to use the Core's"
            )))
        }
        (Some(core), _) => core,
        (None, Some(v)) if v.as_i64().is_some_and(|n| n > 0) => v.as_i64().unwrap_or(0),
        (None, _) => {
            return Err(CliError::invalid_input(format!(
                "the Core reports no Version for type {type_id}: set Version in the file"
            )))
        }
    };
    require_params_type(snapshot, &model, type_id)?;
    for key in MANAGED {
        model.remove(key);
    }
    model.insert("Id".into(), json!(0));
    model.insert("ChangesetId".into(), json!(0));
    model.insert("Version".into(), json!(version));
    let sent = Value::Object(model);
    encode_check(&sent)?;
    let enabled = sent["IsEnabled"].as_bool().unwrap_or(false);
    let params = sent["TriggerParameters"].clone();
    let diff = vec![diff_entry(
        "add",
        "rule",
        Value::Null,
        "",
        Value::Null,
        json!({"name": name, "type_id": type_id, "type_name": type_name(type_id), "enabled": enabled}),
    )];
    let fields = json!({"TriggerModels": [sent]});
    Ok(WritePlan::new(fields, diff, move |after, _| {
        let found: Vec<&Rule> = after
            .live_rules()
            .filter(|r| r.name == name && r.type_id == type_id)
            .collect();
        let mut checks = vec![Check::new(
            "exactly one live rule has the name and type",
            found.len() == 1,
        )
        .detail(format!("{} found", found.len()))];
        if let [rule] = found.as_slice() {
            created.set(Some(rule.id));
            checks.push(Check::new("IsEnabled as sent", rule.enabled == enabled));
            checks.push(
                Check::new(
                    "TriggerParameters read back equal",
                    covers(&rule.model["TriggerParameters"], &params),
                )
                .detail("compared the keys that were sent"),
            );
        }
        checks
    }))
}

fn plan_update(
    snapshot: &Snapshot,
    id: i64,
    mut file: Map<String, Value>,
) -> Result<WritePlan, CliError> {
    let rule = snapshot.rule(id).ok_or_else(|| {
        CliError::new("not_found", format!("no live rule with id {id}"))
            .with_details(json!({"kind": "rule", "reference": id.to_string()}))
    })?;
    roundtrip_gate(rule)?;
    if let Some(t) = file
        .get("TriggerTypeId")
        .filter(|t| t.as_i64() != Some(rule.type_id))
    {
        return Err(CliError::invalid_input(format!(
            "TriggerTypeId cannot change (rule {id} is type {}, the file says {t})",
            rule.type_id
        )));
    }
    if file.contains_key("TriggerParameters") {
        require_params_type(snapshot, &file, rule.type_id)?;
    }
    file.remove("IsDeleted");
    // The file is applied over the current model: omitted keys (Priorities, $unknown, ...) stay.
    let mut new = rule.model.clone();
    for (key, value) in file {
        new[key] = value;
    }
    let diff = model_diff(id, &rule.model, &new);
    if diff.is_empty() {
        return Err(noop(json!("the file matches the stored rule")));
    }
    encode_check(&new)?;
    let old_changeset = rule.model["ChangesetId"].as_i64().unwrap_or(0);
    let fields = json!({"TriggerModels": [new.clone()]});
    Ok(WritePlan::new(fields, diff, move |after, _| {
        let stored = after.rule(id);
        vec![
            Check::new(
                "stored rule equals the sent one",
                stored.is_some_and(|r| covers(&r.model, &without_managed(&new))),
            )
            .detail("compared the keys that were sent"),
            Check::new(
                "ChangesetId grew",
                stored
                    .is_some_and(|r| r.model["ChangesetId"].as_i64().unwrap_or(0) > old_changeset),
            ),
        ]
    }))
}

// ---------------------------------------------------------------- enable, disable, delete

async fn set_enabled(ctx: &Ctx, args: ToggleArgs, target: bool) -> Result<Value, CliError> {
    let reference = args.rule;
    let id_cell = Rc::new(Cell::new(0));
    let slot = id_cell.clone();
    let mut spec = WriteSpec::new("upsertSmartRules", UPSERT, args.flags, move |snapshot| {
        let rule = snapshot.find_rule(&reference)?;
        slot.set(rule.id);
        if rule.enabled == target {
            return Err(noop(json!({"enabled": target, "name": rule.name})));
        }
        roundtrip_gate(rule)?;
        let (id, old) = (rule.id, rule.model.clone());
        let mut new = old.clone();
        new["IsEnabled"] = json!(target);
        let diff = vec![diff_entry(
            "change",
            "rule",
            json!(id),
            "IsEnabled",
            json!(rule.enabled),
            json!(target),
        )];
        let rest = |m: &Value| {
            let mut m = without_managed(m);
            if let Some(o) = m.as_object_mut() {
                o.remove("IsEnabled");
            }
            m
        };
        Ok(WritePlan::new(
            json!({"TriggerModels": [new]}),
            diff,
            move |after, _| {
                let stored = after.rule(id);
                vec![
                    Check::new(
                        "IsEnabled has the new value",
                        stored.is_some_and(|r| r.enabled == target),
                    ),
                    Check::new(
                        "the rest of the model is unchanged",
                        stored.is_some_and(|r| rest(&r.model) == rest(&old)),
                    )
                    .detail("ChangesetId and IsEnabled excluded"),
                    Check::new(
                        "ChangesetId grew",
                        stored.is_some_and(|r| {
                            r.model["ChangesetId"].as_i64().unwrap_or(0)
                                > old["ChangesetId"].as_i64().unwrap_or(0)
                        }),
                    ),
                ]
            },
        ))
    });
    spec.variant_verified = false;
    let result = run_write(ctx, spec).await;
    finish(result, id_cell.get())
}

async fn delete(ctx: &Ctx, args: ToggleArgs) -> Result<Value, CliError> {
    let reference = args.rule;
    let id_cell = Rc::new(Cell::new(0));
    let slot = id_cell.clone();
    let spec = WriteSpec::new(
        "deleteSmartRules",
        "MessageDeleteTriggerModelsRequest",
        args.flags,
        move |snapshot| {
            let rule = snapshot.find_rule(&reference)?;
            slot.set(rule.id);
            let (id, live_before) = (rule.id, snapshot.live_rules().count());
            let diff = vec![diff_entry(
                "remove",
                "rule",
                json!(id),
                "",
                json!({"name": rule.name, "type_id": rule.type_id, "type_name": type_name(rule.type_id)}),
                Value::Null,
            )];
            Ok(WritePlan::new(
                json!({"TriggerIds": [id]}),
                diff,
                move |after, _| {
                    vec![
                        Check::new("the rule is a tombstone or gone", after.rule(id).is_none()),
                        Check::new(
                            "the live rule count dropped by one",
                            after.live_rules().count() + 1 == live_before,
                        ),
                    ]
                },
            ))
        },
    );
    let result = run_write(ctx, spec).await;
    finish(result, id_cell.get())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn taphome_time_converts() {
        assert_eq!(taphome_time_iso(0), "2016-01-01T00:00:00Z");
        assert_eq!(
            taphome_time_iso(86_400 * 366 + 3_661),
            "2017-01-01T01:01:01Z"
        );
        assert_eq!(
            taphome_time_iso(86_400 * (366 + 365 + 59)),
            "2018-03-01T00:00:00Z"
        );
    }

    #[test]
    fn covers_treats_defaults_as_absent() {
        let stored = json!({"A": 1, "L": [{"X": 2}]});
        assert!(covers(
            &stored,
            &json!({"A": 1, "B": 0, "C": "", "D": [], "L": [{"X": 2, "Y": false}]})
        ));
        assert!(!covers(&stored, &json!({"A": 2})));
        assert!(!covers(&stored, &json!({"B": 1})));
        assert!(!covers(&stored, &json!({"L": []})));
    }
}
