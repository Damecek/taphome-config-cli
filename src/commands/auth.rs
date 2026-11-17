//! `taphome auth login | status | logout`.
use crate::config;
use crate::core::{self, Ctx, Reconnect};
use crate::errors::CliError;
use clap::Subcommand;
use serde_json::{json, Value};

#[derive(Subcommand)]
pub enum Cmd {
    /// Log in with TAPHOME_HOST, TAPHOME_USER and TAPHOME_PASSWORD and store the session
    Login,
    /// Show whether a stored session exists and whether the Core still accepts it
    Status,
    /// Delete the stored session
    Logout,
}

pub async fn run(ctx: &Ctx, cmd: Cmd) -> Result<Value, CliError> {
    match cmd {
        Cmd::Login => {
            let outcome = core::login(ctx).await?;
            Ok(json!({"user": outcome.session.user, "location": outcome.location["Name"], "session_stored": true}))
        }
        Cmd::Status => status(ctx).await,
        Cmd::Logout => Ok(json!({"session_removed": config::delete_session(&ctx.env)?})),
    }
}

async fn status(ctx: &Ctx) -> Result<Value, CliError> {
    let stored = config::load_session(&ctx.env);
    let host = ctx.config.host.clone().or_else(|| stored.as_ref().map(|s| s.host.clone()));
    let mut doc = json!({
        "stored": stored.is_some(), "valid": false, "core_version": null, "protocol": null,
        "user": stored.as_ref().map(|s| s.user.clone()),
        "host_configured": host.is_some(),
        "salt_source": ctx.salt.source.as_str(),
    });
    if ctx.global.show_secrets {
        doc["host"] = json!(host);
    }
    if let Some(stored) = stored {
        if let Reconnect::Ready(session) = core::reconnect(ctx, &stored).await? {
            doc["valid"] = json!(true);
            doc["core_version"] = session.reconnect["CoreVersionString"].clone();
            doc["protocol"] = session.reconnect["CurrentProtocolVersion"].clone();
            session.close().await;
        }
    }
    Ok(doc)
}
