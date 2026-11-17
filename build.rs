//! Writes the password salt into `$OUT_DIR/salt.bin`.
//!
//! The value never goes through `cargo:rustc-env`, which would land in build logs, and this script
//! never prints it, not even its length. See `docs/auth.md` for the lookup order.
use std::env;
use std::fs;
use std::path::PathBuf;

fn config_dir() -> Option<PathBuf> {
    if let Some(dir) = env::var_os("TAPHOME_CONFIG_DIR").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(dir));
    }
    // Same rule as src/config.rs, evaluated on the machine that builds.
    if cfg!(windows) {
        env::var_os("APPDATA").map(|d| PathBuf::from(d).join("taphome"))
    } else {
        env::var_os("HOME").map(|h| PathBuf::from(h).join(".config").join("taphome"))
    }
}

fn from_file(path: &PathBuf) -> Option<Vec<u8>> {
    let mut bytes = fs::read(path).ok()?;
    if bytes.last() == Some(&b'\n') {
        bytes.pop();
        if bytes.last() == Some(&b'\r') {
            bytes.pop();
        }
    }
    Some(bytes).filter(|b| !b.is_empty())
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=TAPHOME_PASSWORD_SALT");
    println!("cargo:rerun-if-env-changed=TAPHOME_REQUIRE_SALT");
    println!("cargo:rerun-if-env-changed=TAPHOME_CONFIG_DIR");

    let salt_file = config_dir().map(|d| d.join("salt"));
    if let Some(path) = &salt_file {
        println!("cargo:rerun-if-changed={}", path.display());
    }

    let salt = env::var_os("TAPHOME_PASSWORD_SALT")
        .map(|v| v.into_encoded_bytes())
        .filter(|b| !b.is_empty())
        .or_else(|| salt_file.as_ref().and_then(from_file));

    if salt.is_none() {
        if env::var("TAPHOME_REQUIRE_SALT").as_deref() == Ok("1") {
            panic!(
                "no password salt available and TAPHOME_REQUIRE_SALT=1: set the environment variable \
                 TAPHOME_PASSWORD_SALT or create the file <config dir>/salt \
                 (~/.config/taphome/salt, or %APPDATA%\\taphome\\salt on Windows). See docs/auth.md."
            );
        }
        println!(
            "cargo:warning=building without a password salt: login will fail with salt_missing unless \
             TAPHOME_PASSWORD_SALT is set at run time (see docs/auth.md)"
        );
    }

    let out =
        PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR is set by cargo")).join("salt.bin");
    fs::write(out, salt.unwrap_or_default()).expect("cannot write salt.bin");
}
