//! `taphome io values | errors | log | watch`: read-only checks of inputs, outputs and device errors.
use crate::catalog;
use crate::commands::glob_match;
use crate::core::{open_session, request, Ctx, Snapshot};
use crate::errors::CliError;
use clap::{Args, Subcommand};
use serde_json::{json, Map, Value};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[derive(Subcommand)]
pub enum Cmd {
    /// Current values from the synced configuration, with decoded DeviceStatus
    Values(ValuesArgs),
    /// Errors, warnings and infos of one device (getDeviceErrors)
    Errors(ErrorsArgs),
    /// Value change log of one device (getDeviceLog), newest entries
    Log(LogArgs),
    /// Collect value change broadcasts for some seconds
    Watch(WatchArgs),
}

#[derive(Args)]
pub struct ValuesArgs {
    /// Device ids or exact names. All devices when omitted
    devices: Vec<String>,
    /// Model pattern, `*` matches any text and `?` one character, case-insensitive, for example 'Virtual*'
    #[arg(long)]
    model: Option<String>,
    /// Name substring
    #[arg(long)]
    name: Option<String>,
}

#[derive(Args)]
pub struct ErrorsArgs {
    /// Device id or exact name
    device: String,
    /// 0 device, 1 module, 2 interface, 3 core. Default: 1 for module devices, else 0
    #[arg(long)]
    origin: Option<i64>,
}

#[derive(Args)]
pub struct LogArgs {
    /// Device id or exact name
    device: String,
    /// Keep the newest N entries
    #[arg(long, default_value_t = 50)]
    limit: usize,
}

#[derive(Args)]
pub struct WatchArgs {
    /// How long to collect, 0 to 300
    #[arg(long, default_value_t = 10.0)]
    seconds: f64,
    /// Only these devices (id or exact name), repeatable
    #[arg(long = "device", value_name = "DEV")]
    devices: Vec<String>,
}

pub async fn run(ctx: &Ctx, cmd: Cmd) -> Result<Value, CliError> {
    match cmd {
        Cmd::Values(a) => values(ctx, a).await,
        Cmd::Errors(a) => errors(ctx, a).await,
        Cmd::Log(a) => log(ctx, a).await,
        Cmd::Watch(a) => watch(ctx, a).await,
    }
}

// ---------------------------------------------------------------- time

/// TapHome time counts seconds from 2016-01-01 UTC.
const TAPHOME_EPOCH: i64 = 1_451_606_400;

/// ISO 8601 UTC from Unix seconds (civil-from-days algorithm).
fn iso(unix: i64) -> String {
    let days = unix.div_euclid(86_400);
    let rest = unix.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
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

fn th_iso(taphome_time: i64) -> String {
    iso(taphome_time + TAPHOME_EPOCH)
}

fn now_iso() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    iso(secs)
}

// ---------------------------------------------------------------- decoding

const DEVICE_STATUS: i64 = 7;

fn type_name(id: i64) -> Value {
    catalog::enum_name("ValueLogType", id).map_or(Value::Null, |n| json!(n))
}

/// Plain enum below 256, packed counts from 256 (errors bits 8 to 15, warnings 16 to 23, infos 24 to 31).
fn decode_status(v: i64) -> Value {
    if v < 256 {
        json!({"code": v, "name": catalog::enum_name("DeviceStatus", v), "ok": v == 0})
    } else {
        json!({"code": v, "packed": true, "errors": (v >> 8) & 0xff, "warnings": (v >> 16) & 0xff,
               "infos": (v >> 24) & 0xff, "ok": false})
    }
}

fn value_entry(type_id: i64, value: &Value, changed: Option<i64>) -> Value {
    let mut e = json!({"type": type_id, "name": type_name(type_id), "value": value});
    if type_id == DEVICE_STATUS {
        if let Some(v) = value.as_f64().filter(|f| f.is_finite()) {
            e["status"] = decode_status(v as i64);
        }
    }
    if let Some(t) = changed {
        e["changed_on_th"] = json!(t);
        e["changed_at"] = json!(th_iso(t));
    }
    e
}

fn id_of(d: &Value) -> i64 {
    d["ID"].as_i64().unwrap_or(0)
}

/// An absent `Value` in a decoded `ValueLog` means 0 (protobuf omits defaults).
fn current_values(device: &Value) -> Vec<Value> {
    let mut entries: Vec<(i64, Value)> = device["CurrentValues"]
        .as_object()
        .map(|m| {
            m.iter()
                .map(|(k, v)| {
                    let id = v["Type"].as_i64().or_else(|| k.parse().ok()).unwrap_or(0);
                    let value = v.get("Value").cloned().unwrap_or(json!(0.0));
                    let changed = v["ChangedOnTHUtc"].as_i64().unwrap_or(0);
                    (id, value_entry(id, &value, Some(changed)))
                })
                .collect()
        })
        .unwrap_or_default();
    entries.sort_by_key(|(id, _)| *id);
    entries.into_iter().map(|(_, e)| e).collect()
}

// ---------------------------------------------------------------- commands

async fn values(ctx: &Ctx, args: ValuesArgs) -> Result<Value, CliError> {
    let session = open_session(ctx).await?;
    let snapshot = Snapshot::from_session(&session);
    session.close().await;
    let snapshot = snapshot?;
    let wanted: Vec<i64> = args
        .devices
        .iter()
        .map(|d| snapshot.find_device(d).map(id_of))
        .collect::<Result<_, _>>()?;
    let mut shown = vec![];
    let mut not_ok = vec![];
    for d in &snapshot.devices {
        let (id, name, model) = (
            id_of(d),
            d["Name"].as_str().unwrap_or_default(),
            d["Model"].as_str().unwrap_or_default(),
        );
        if (!wanted.is_empty() && !wanted.contains(&id))
            || args.model.as_ref().is_some_and(|g| !glob_match(g, model))
            || args
                .name
                .as_ref()
                .is_some_and(|s| !name.contains(s.as_str()))
        {
            continue;
        }
        let values = current_values(d);
        if values.iter().any(|v| v["status"]["ok"] == json!(false)) {
            not_ok.push(json!(id));
        }
        shown.push(json!({"id": id, "name": name, "model": model, "values": values}));
    }
    Ok(json!({"summary": {"devices": shown.len(), "status_not_ok": not_ok}, "devices": shown}))
}

fn severity(id: i64) -> Value {
    match id {
        1 => json!("info"),
        2 => json!("warning"),
        3 => json!("error"),
        _ => Value::Null,
    }
}

fn error_entry(e: &Value) -> Value {
    let code = e["ErrorCodeId"].as_i64().unwrap_or(0);
    let mut out = e.clone();
    out["time"] = json!(th_iso(e["TapHomeTime"].as_i64().unwrap_or(0)));
    out["error_code_name"] =
        catalog::enum_name("DeviceErrorCode", code).map_or(Value::Null, |n| json!(n));
    out["severity"] = severity(e["SeverityId"].as_i64().unwrap_or(0));
    out
}

async fn errors(ctx: &Ctx, args: ErrorsArgs) -> Result<Value, CliError> {
    if args.origin.is_some_and(|o| !(0..=3).contains(&o)) {
        return Err(CliError::invalid_input("--origin must be 0, 1, 2 or 3"));
    }
    let session = open_session(ctx).await?;
    let result = async {
        let snapshot = Snapshot::from_session(&session)?;
        let device = snapshot.find_device(&args.device)?;
        let id = id_of(device);
        let origin = args
            .origin
            .unwrap_or_else(|| i64::from(device["Interface"] == "IHardwareParentModule"));
        let fields = json!({"DeviceId": id, "ShouldSubscribe": false, "OriginId": origin});
        let response = request(
            ctx,
            &session,
            "MessageGetDeviceErrorsRequest",
            &fields,
            None,
        )
        .await?;
        let list: Vec<Value> = response["DeviceErrors"]
            .as_array()
            .map(|a| a.iter().map(error_entry).collect())
            .unwrap_or_default();
        Ok(
            json!({"device": {"id": id, "name": device["Name"]}, "origin_id": origin,
                  "count": list.len(), "errors": list}),
        )
    }
    .await;
    session.close().await;
    result
}

fn log_entry(e: &Value) -> Value {
    let time = e["TapHomeTime"].as_i64().unwrap_or(0);
    let values: Vec<Value> = e["Values"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|p| value_entry(p[0].as_i64().unwrap_or(0), &p[1], None))
                .collect()
        })
        .unwrap_or_default();
    let origin = e["ChangeOriginId"].as_i64().unwrap_or(0);
    json!({"time_th": time, "time": th_iso(time), "values": values, "change_origin_id": origin,
           "change_origin": catalog::enum_name("ChangeOrigin", origin), "handling_status": e["HandlingStatus"]})
}

async fn log(ctx: &Ctx, args: LogArgs) -> Result<Value, CliError> {
    let session = open_session(ctx).await?;
    let result = async {
        let snapshot = Snapshot::from_session(&session)?;
        let device = snapshot.find_device(&args.device)?;
        let id = id_of(device);
        let fields = json!({"DeviceId": id, "ShouldSubscribe": false});
        let response = request(ctx, &session, "MessageGetDeviceLogRequest", &fields, None).await?;
        let mut entries: Vec<&Value> = response["ValueChangedLogEntries"]
            .as_array()
            .map(|a| a.iter().collect())
            .unwrap_or_default();
        let total = entries.len();
        entries.sort_by_key(|e| e["TapHomeTime"].as_i64().unwrap_or(0));
        let kept: Vec<Value> = entries[total.saturating_sub(args.limit)..]
            .iter()
            .map(|e| log_entry(e))
            .collect();
        Ok(json!({"device": {"id": id, "name": device["Name"]}, "total": total, "returned": kept.len(),
                  "entries": kept}))
    }
    .await;
    session.close().await;
    result
}

async fn watch(ctx: &Ctx, args: WatchArgs) -> Result<Value, CliError> {
    if !args.seconds.is_finite() || !(0.0..=300.0).contains(&args.seconds) {
        return Err(CliError::invalid_input(
            "--seconds must be between 0 and 300",
        ));
    }
    let session = open_session(ctx).await?;
    let setup = Snapshot::from_session(&session).and_then(|snapshot| {
        let filter = args
            .devices
            .iter()
            .map(|d| snapshot.find_device(d).map(id_of))
            .collect::<Result<Vec<i64>, _>>()?;
        Ok((snapshot, filter))
    });
    let (snapshot, filter) = match setup {
        Ok(s) => s,
        Err(e) => {
            session.close().await;
            return Err(e);
        }
    };
    // Poll in slices so each change gets the time it arrived: broadcasts carry no timestamp of their own.
    let deadline = Instant::now() + Duration::from_secs_f64(args.seconds);
    let mut seen = 0;
    let mut counts: Map<String, Value> = Map::new();
    let mut changes = vec![];
    loop {
        let fresh = session.broadcasts_since(seen);
        seen += fresh.len();
        for b in &fresh {
            let class = b["$type"].as_str().unwrap_or_default();
            let n = counts.get(class).and_then(Value::as_i64).unwrap_or(0);
            counts.insert(class.to_string(), json!(n + 1));
            let id = b["DeviceId"].as_i64().unwrap_or(0);
            if class != "MessageDeviceValueChangedBroadcast"
                || (!filter.is_empty() && !filter.contains(&id))
            {
                continue;
            }
            let value = b.get("Value").cloned().unwrap_or(json!(0.0));
            let mut change = value_entry(b["ValueTypeId"].as_i64().unwrap_or(0), &value, None);
            change["device_id"] = json!(id);
            change["device_name"] = snapshot
                .device(id)
                .map_or(Value::Null, |d| d["Name"].clone());
            change["received_at"] = json!(now_iso());
            changes.push(change);
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        session.wait(left.min(Duration::from_millis(100))).await;
    }
    if session.is_closed() {
        ctx.warn("the connection closed before the watch time ended");
    }
    session.close().await;
    Ok(json!({"seconds": args.seconds, "changes": changes, "broadcasts": counts}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn taphome_time_to_iso() {
        assert_eq!(th_iso(0), "2016-01-01T00:00:00Z");
        assert_eq!(th_iso(343_340_580), "2026-11-17T20:23:00Z");
    }
}
