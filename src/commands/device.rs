//! `taphome device ...`: placeholder, replaced as a whole file.
use crate::core::Ctx;
use crate::errors::CliError;
use clap::Subcommand;
use serde_json::Value;

#[derive(Subcommand)]
pub enum Cmd {
    #[command(hide = true)]
    Todo,
}

pub async fn run(_ctx: &Ctx, cmd: Cmd) -> Result<Value, CliError> {
    match cmd {
        Cmd::Todo => Err(CliError::internal("not implemented")),
    }
}
