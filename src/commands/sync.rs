//! `taphome sync [--full | --section S] [--out FILE]`: read the configuration from the Core.
use crate::config::write_private;
use crate::core::{open_session, Ctx, Snapshot};
use crate::errors::CliError;
use clap::{Args, ValueEnum};
use serde_json::{json, Map, Value};
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum Section {
    Devices,
    Interfaces,
    Rules,
    Dashboards,
    Core,
}

#[derive(Args)]
pub struct Cmd {
    /// Everything: core, interfaces, devices, rules, dashboards
    #[arg(long, conflicts_with = "section")]
    full: bool,
    /// One section only
    #[arg(long, value_enum)]
    section: Option<Section>,
    /// Write the document to FILE (mode 600) and print the summary and the path
    #[arg(long, value_name = "FILE")]
    out: Option<PathBuf>,
}

fn summary(s: &Snapshot) -> Value {
    let interfaces: Vec<Value> = s
        .interfaces
        .iter()
        .map(|c| json!({"id": c["CcuId"], "model": c["Model"], "name": c["Name"]}))
        .collect();
    json!({"core_version": s.core_version, "protocol": s.protocol, "counts": s.counts(), "interfaces": interfaces})
}

fn section(s: &Snapshot, which: Section) -> Value {
    match which {
        Section::Devices => json!({"devices": s.devices}),
        Section::Interfaces => json!({"interfaces": s.interfaces}),
        Section::Rules => json!({"rules": s.rules.iter().map(|r| r.to_json()).collect::<Vec<_>>()}),
        Section::Dashboards => json!({"dashboards": s.dashboards}),
        Section::Core => json!({"core_version": s.core_version, "protocol": s.protocol, "counts": s.counts(), "location": s.location,
                                "user": s.user, "trigger_versions": s.trigger_versions, "core_properties": s.core_properties}),
    }
}

fn full(s: &Snapshot) -> Value {
    let mut doc = Map::new();
    for which in [
        Section::Core,
        Section::Interfaces,
        Section::Devices,
        Section::Rules,
        Section::Dashboards,
    ] {
        if let Value::Object(part) = section(s, which) {
            doc.extend(part);
        }
    }
    Value::Object(doc)
}

pub async fn run(ctx: &Ctx, cmd: Cmd) -> Result<Value, CliError> {
    let session = open_session(ctx).await?;
    let snapshot = Snapshot::from_session(&session);
    session.close().await;
    let snapshot = snapshot?;
    let selected = match (cmd.section, cmd.full) {
        (Some(which), _) => Some(section(&snapshot, which)),
        (None, true) => Some(full(&snapshot)),
        (None, false) => None,
    };
    match cmd.out {
        None => Ok(selected.unwrap_or_else(|| summary(&snapshot))),
        Some(path) => {
            let doc = ctx.redactor().redact(&selected.unwrap_or_else(|| full(&snapshot)));
            let bytes = serde_json::to_vec_pretty(&doc).map_err(|e| CliError::internal(e.to_string()))?;
            write_private(&path, &bytes)?;
            let mut out = summary(&snapshot);
            out["out"] = json!(path.display().to_string());
            out["out_bytes"] = json!(bytes.len());
            Ok(out)
        }
    }
}
