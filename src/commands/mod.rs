//! One file per command group. A group exposes `Cmd` (clap) and `run(ctx, cmd)`; `cli.rs` only wires them.
pub mod auth;
pub mod device;
pub mod io;
pub mod pp;
pub mod raw;
pub mod rule;
pub mod sync;
