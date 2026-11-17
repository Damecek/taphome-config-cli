//! The clap tree and the entry points. Each command group lives in `commands/<group>.rs`; this file
//! only wires them, so a group can be replaced without touching anything else.
use crate::commands::{auth, device, io, pp, raw, rule, sync};
use crate::config::Env;
use crate::core::{Ctx, Global};
use crate::errors::CliError;
use crate::{output, salt};
use clap::error::ErrorKind;
use clap::{ArgMatches, CommandFactory, FromArgMatches, Parser, Subcommand};
use serde_json::Value;
use std::panic::{catch_unwind, AssertUnwindSafe};

#[derive(Parser)]
#[command(
    name = "taphome",
    version,
    about = "Configure a TapHome Core from the command line. Output is one JSON document per command."
)]
struct Cli {
    /// Indent the JSON output
    #[arg(long, global = true)]
    pretty: bool,
    /// Show property passwords and tokens (session id, password hash and salt stay hidden)
    #[arg(long, global = true)]
    show_secrets: bool,
    /// Limit in seconds for connecting and for every reply
    #[arg(long, global = true, value_name = "S", value_parser = parse_timeout)]
    timeout: Option<f64>,
    #[command(subcommand)]
    command: Command,
}

fn parse_timeout(text: &str) -> Result<f64, String> {
    match text.parse::<f64>() {
        Ok(s) if s.is_finite() && s > 0.0 => Ok(s),
        _ => Err("must be a positive number of seconds".to_string()),
    }
}

#[derive(Subcommand)]
enum Command {
    /// Log in, check or drop the stored session
    Auth {
        #[command(subcommand)]
        cmd: auth::Cmd,
    },
    /// Read the configuration from the Core
    Sync(sync::Cmd),
    /// Send any request class, behind safety gates
    Raw {
        #[command(subcommand)]
        cmd: raw::Cmd,
    },
    /// Devices
    Device {
        #[command(subcommand)]
        cmd: device::Cmd,
    },
    /// Packet Parser modules and scripts
    Pp {
        #[command(subcommand)]
        cmd: pp::Cmd,
    },
    /// Smart rules
    Rule {
        #[command(subcommand)]
        cmd: rule::Cmd,
    },
    /// Device values, errors and logs
    Io {
        #[command(subcommand)]
        cmd: io::Cmd,
    },
}

async fn dispatch(ctx: &Ctx, command: Command) -> Result<Value, CliError> {
    match command {
        Command::Auth { cmd } => auth::run(ctx, cmd).await,
        Command::Sync(cmd) => sync::run(ctx, cmd).await,
        Command::Raw { cmd } => raw::run(ctx, cmd).await,
        Command::Device { cmd } => device::run(ctx, cmd).await,
        Command::Pp { cmd } => pp::run(ctx, cmd).await,
        Command::Rule { cmd } => rule::run(ctx, cmd).await,
        Command::Io { cmd } => io::run(ctx, cmd).await,
    }
}

/// "auth login", "sync", ... from the parsed arguments.
fn command_path(matches: &ArgMatches) -> String {
    let mut names = vec![];
    let mut current = matches;
    while let Some((name, sub)) = current.subcommand() {
        names.push(name);
        current = sub;
    }
    names.join(" ")
}

fn panic_text(payload: &(dyn std::any::Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "panic".to_string())
}

/// Runs one invocation. Returns the exit code, the JSON document, and whether to indent it.
/// For `--help` and `--version` the document is a `Value::String` with the plain text.
fn execute(args: &[String], env: Env, built_in_salt: &[u8]) -> (i32, Value, bool) {
    let debug = env.get("TAPHOME_DEBUG") == Some("1");
    let argv = std::iter::once("taphome".to_string()).chain(args.iter().cloned());
    let matches = match Cli::command().try_get_matches_from(argv) {
        Ok(m) => m,
        Err(e) if matches!(e.kind(), ErrorKind::DisplayHelp | ErrorKind::DisplayVersion) => {
            return (0, Value::String(e.render().to_string()), false);
        }
        Err(e) => {
            let text = e.render().to_string();
            let error = CliError::new("usage", text.trim().trim_start_matches("error: ").to_string());
            return (error.exit_code, output::failure("taphome", &error), false);
        }
    };
    let name = command_path(&matches);
    let cli = match Cli::from_arg_matches(&matches) {
        Ok(c) => c,
        Err(e) => {
            let error = CliError::new("usage", e.to_string());
            return (error.exit_code, output::failure(&name, &error), false);
        }
    };
    let global = Global {
        pretty: cli.pretty,
        show_secrets: cli.show_secrets,
        timeout: cli.timeout,
    };
    let ctx = match Ctx::new(env, global, built_in_salt) {
        Ok(ctx) => ctx,
        Err(error) => return (error.exit_code, output::failure(&name, &error), cli.pretty),
    };
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| CliError::internal(format!("cannot start the async runtime: {e}")))
            .and_then(|rt| rt.block_on(dispatch(&ctx, cli.command)))
    }));
    let result = outcome.unwrap_or_else(|payload| {
        if debug {
            eprintln!("panic: {}", panic_text(&*payload));
        }
        Err(CliError::internal(
            "unexpected internal error (set TAPHOME_DEBUG=1 for details on stderr)",
        ))
    });
    let (code, doc) = match result {
        Ok(data) => (0, output::success(&name, data, &ctx.take_warnings())),
        Err(error) => {
            if debug && error.code == "internal" {
                eprintln!("internal error: {}", error.message);
            }
            (error.exit_code, output::failure(&name, &error))
        }
    };
    (code, ctx.redactor().redact(&doc), ctx.global.pretty)
}

/// In-process entry used by tests. `args` excludes the program name. `built_in_salt` replaces the
/// salt compiled into the binary.
pub fn run_with_salt<S: AsRef<str>>(args: &[S], env: Env, built_in_salt: &[u8]) -> (i32, Value) {
    let args: Vec<String> = args.iter().map(|a| a.as_ref().to_string()).collect();
    let (code, doc, _) = execute(&args, env, built_in_salt);
    (code, doc)
}

/// `run_with_salt` with the salt compiled into this binary.
pub fn run<S: AsRef<str>>(args: &[S], env: Env) -> (i32, Value) {
    run_with_salt(args, env, salt::BUILT_IN)
}

/// Entry of `main.rs`: runs, prints the document and returns the exit code.
pub fn main_from_env() -> i32 {
    let env = Env::from_process();
    if env.get("TAPHOME_DEBUG") != Some("1") {
        std::panic::set_hook(Box::new(|_| {}));
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (code, doc, pretty) = execute(&args, env, salt::BUILT_IN);
    match &doc {
        Value::String(text) => print!("{text}"),
        _ if pretty => println!("{}", serde_json::to_string_pretty(&doc).unwrap_or_default()),
        _ => println!("{doc}"),
    }
    code
}
