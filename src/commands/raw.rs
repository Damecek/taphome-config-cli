//! `taphome raw call | ops`: send any request class, behind the safety gates.
use crate::catalog;
use crate::core::{
    count_delta, frame_hex, open_session, request, run_write, Check, Ctx, WriteFlags, WritePlan,
    WriteSpec,
};
use crate::errors::CliError;
use clap::{Args, Subcommand};
use serde_json::{json, Map, Value};
use std::path::PathBuf;
use std::time::Duration;

#[derive(Subcommand)]
pub enum Cmd {
    /// Send one request. Verified read and subscribe classes run by default
    Call(CallArgs),
    /// List the operation registry
    Ops(OpsArgs),
}

#[derive(Args)]
pub struct CallArgs {
    /// Request class, for example MessageGetBasicCorePropertiesRequest
    message: String,
    /// Request fields as a JSON object
    #[arg(long, conflicts_with = "json_file")]
    json: Option<String>,
    /// Request fields from a JSON file
    #[arg(long, value_name = "FILE")]
    json_file: Option<PathBuf>,
    /// Required for write and action classes
    #[arg(long)]
    allow_write: bool,
    /// Required for unverified or unknown classes
    #[arg(long)]
    allow_unverified: bool,
    /// Print the fields and the encoded frame, send nothing
    #[arg(long)]
    dry_run: bool,
    /// Seconds to collect broadcasts after the answer
    #[arg(long, value_name = "S", default_value_t = 0.0)]
    wait_broadcasts: f64,
}

#[derive(Args)]
pub struct OpsArgs {
    /// read, subscribe, write or action
    #[arg(long)]
    kind: Option<String>,
    /// Only operations verified (true) or not verified (false) against a real Core
    #[arg(long, value_name = "true|false")]
    verified: Option<bool>,
}

pub async fn run(ctx: &Ctx, cmd: Cmd) -> Result<Value, CliError> {
    match cmd {
        Cmd::Call(args) => call(ctx, args).await,
        Cmd::Ops(args) => Ok(ops(args)),
    }
}

fn ops(args: OpsArgs) -> Value {
    let list: Vec<Value> = catalog::operations()
        .filter(|(_, op)| args.kind.as_deref().is_none_or(|k| op.kind == k) && args.verified.is_none_or(|v| op.verified == v))
        .map(|(class, op)| {
            json!({"class": class, "ids": op.ids, "kind": op.kind, "verified": op.verified, "response": op.response,
                   "endpoint": op.endpoint, "min_protocol": op.min_protocol})
        })
        .collect();
    json!({"count": list.len(), "operations": list})
}

fn load_fields(args: &CallArgs) -> Result<Value, CliError> {
    let text = match (&args.json, &args.json_file) {
        (Some(t), _) => t.clone(),
        (None, Some(path)) => std::fs::read_to_string(path)
            .map_err(|e| CliError::invalid_input(format!("cannot read --json-file: {e}")))?,
        (None, None) => return Ok(json!({})),
    };
    match serde_json::from_str::<Value>(&text) {
        Ok(v @ Value::Object(_)) => Ok(v),
        Ok(_) => Err(CliError::invalid_input(
            "the request fields must be a JSON object",
        )),
        Err(e) => Err(CliError::invalid_input(format!(
            "the request fields are not valid JSON: {e}"
        ))),
    }
}

async fn call(ctx: &Ctx, args: CallArgs) -> Result<Value, CliError> {
    let class = args.message.as_str();
    if catalog::message_type(class) != Some("Request") {
        return Err(CliError::invalid_input(format!(
            "{class} is not a known request class (see: taphome raw ops)"
        )));
    }
    let fields = load_fields(&args)?;
    let op = catalog::operation(class);
    let is_write = op.is_none_or(|o| matches!(o.kind.as_str(), "write" | "action"));
    let verified = op.is_some_and(|o| o.verified);

    if args.dry_run {
        // The frame must not leak what the output hides, so it is built from the redacted fields.
        let shown = ctx.redactor().redact(&fields);
        return Ok(
            json!({"dry_run": true, "operation": op.and_then(|o| o.ids.first()), "verified": verified,
                         "kind": op.map(|o| o.kind.as_str()),
                         "message": {"class": class, "fields": shown}, "diff": null,
                         "frame_hex": frame_hex(class, &shown, 2)?, "frame_redacted": shown != fields}),
        );
    }

    // a class without a registry entry counts as a write, since nothing says it only reads
    if is_write && !args.allow_write {
        return Err(CliError::new(
            "write_not_allowed",
            format!("{class} changes the Core: pass --allow-write to send it"),
        )
        .with_details(json!({"class": class})));
    }
    if !verified && !args.allow_unverified {
        return Err(CliError::new(
            "unverified_operation",
            format!(
                "{class} is {}: pass --allow-unverified to send it",
                if op.is_some() {
                    "not verified against a real Core"
                } else {
                    "not in the operation registry"
                }
            ),
        )
        .with_details(json!({"class": class})));
    }
    let seconds = if args.wait_broadcasts.is_finite() {
        args.wait_broadcasts.clamp(0.0, 3600.0)
    } else {
        0.0
    };
    let wait = Duration::from_secs_f64(seconds);
    if is_write {
        return raw_write(ctx, class, fields, wait, args.allow_unverified).await;
    }
    let session = open_session(ctx).await?;
    let response = request(ctx, &session, class, &fields, None).await;
    if response.is_ok() {
        session.wait(wait).await;
    }
    let mut counts: Map<String, Value> = Map::new();
    for b in session.broadcasts() {
        let entry = counts
            .entry(b["$type"].as_str().unwrap_or_default().to_string())
            .or_insert(json!(0));
        *entry = json!(entry.as_i64().unwrap_or(0) + 1);
    }
    session.close().await;
    Ok(json!({"class": class, "response": response?, "broadcasts": counts}))
}

async fn raw_write(
    ctx: &Ctx,
    class: &str,
    fields: Value,
    wait: Duration,
    allow_unverified: bool,
) -> Result<Value, CliError> {
    let operation = catalog::operation(class)
        .and_then(|o| o.ids.first().cloned())
        .unwrap_or_else(|| class.to_string());
    let mut spec = WriteSpec::new(
        &operation,
        class,
        WriteFlags {
            dry_run: false,
            allow_unverified,
        },
        move |_snapshot| {
            // No state expectation exists for a raw call: `delta` shows what changed.
            Ok(WritePlan::new(fields, vec![], |_after, _response| {
                vec![Check::new("the Core answered without error", true)
                    .detail("a raw call has no expected state, compare delta")]
            }))
        },
    );
    spec.settle = Some(wait.max(ctx.settle));
    let mut doc = run_write(ctx, spec).await?;
    doc["delta"] = count_delta(&doc["state_before"], &doc["state"]);
    Ok(doc)
}
