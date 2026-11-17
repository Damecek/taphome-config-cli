mod common;
use common::*;
use serde_json::{json, Value};
use taphome_cli::cli::run_with_salt;

fn count(core: &FakeCore, class: &str) -> usize {
    core.requests().iter().filter(|c| *c == class).count()
}

#[test]
fn login_stores_session_and_never_prints_it() {
    let core = FakeCore::start();
    let (code, doc) = run_cli(&core, &["auth", "login"]);
    assert_eq!(code, 0, "{doc}");
    assert_eq!(doc["command"], "auth login");
    assert_eq!(data(&doc)["session_stored"], true);
    assert_eq!(data(&doc)["location"], "Test home");
    assert_eq!(data(&doc)["user"], USER_LOGIN);
    let stored = stored_session(&core);
    let session_id = stored["session_id"].as_str().unwrap();
    assert!(!doc.to_string().contains(session_id));
    assert_eq!(stored["user"], USER_LOGIN);
    assert_eq!(stored["host"], core.host());
}

#[test]
#[cfg(unix)]
fn config_files_are_private() {
    let core = FakeCore::start();
    assert_eq!(run_cli(&core, &["auth", "login"]).0, 0);
    assert_eq!(mode(&core.config_dir()), 0o700);
    assert_eq!(mode(&core.config_dir().join("session.json")), 0o600);
    let device_id = core.config_dir().join("device-id");
    assert_eq!(mode(&device_id), 0o600);
    let id = std::fs::read_to_string(device_id).unwrap();
    assert_eq!(id.trim().len(), 16);
    assert!(id.trim().chars().all(|c| c.is_ascii_hexdigit()));
}

#[test]
fn second_command_reuses_the_session_and_needs_one_reconnect() {
    let core = FakeCore::start();
    assert_eq!(run_cli(&core, &["sync"]).0, 0);
    // First run: login, then reconnect twice (result 4 with the DbHash, then 1).
    assert_eq!(core.logins(), 1);
    assert_eq!(count(&core, "MessageReconnectRequest"), 2);
    assert_eq!(
        stored_session(&core)["db_hash"],
        core.with_state(|s| s.db_hash.clone())
    );
    assert_eq!(run_cli(&core, &["sync"]).0, 0);
    assert_eq!(core.logins(), 1, "no second login");
    assert_eq!(count(&core, "MessageReconnectRequest"), 3, "one reconnect");
}

#[test]
fn stale_db_hash_is_retried_and_persisted() {
    let core = FakeCore::start();
    assert_eq!(run_cli(&core, &["sync"]).0, 0);
    core.set_db_hash("99999999-0000-4000-8000-000000000000");
    assert_eq!(run_cli(&core, &["sync"]).0, 0);
    assert_eq!(
        stored_session(&core)["db_hash"],
        "99999999-0000-4000-8000-000000000000"
    );
    assert_eq!(core.logins(), 1);
}

#[test]
fn wrong_session_logs_in_again_when_the_password_is_available() {
    let core = FakeCore::start();
    assert_eq!(run_cli(&core, &["sync"]).0, 0);
    let first = stored_session(&core)["session_id"].clone();
    core.forget_sessions();
    let (code, doc) = run_cli(&core, &["sync"]);
    assert_eq!(code, 0, "{doc}");
    assert_eq!(core.logins(), 2);
    assert_ne!(stored_session(&core)["session_id"], first);
}

#[test]
fn wrong_session_without_password_is_session_invalid() {
    let core = FakeCore::start();
    assert_eq!(run_cli(&core, &["sync"]).0, 0);
    core.forget_sessions();
    let (code, doc) = run_cli_with(
        &core,
        &["sync"],
        &[("TAPHOME_USER", None), ("TAPHOME_PASSWORD", None)],
    );
    assert_eq!(code, 3);
    assert_eq!(error(&doc)["code"], "session_invalid");
    assert_eq!(core.logins(), 1);
}

#[test]
fn login_failures() {
    let core = FakeCore::start();
    let (code, doc) = run_cli_with(
        &core,
        &["auth", "login"],
        &[("TAPHOME_PASSWORD", Some("wrong"))],
    );
    assert_eq!(code, 3);
    assert_eq!(error(&doc)["code"], "login_failed");
    assert_eq!(error(&doc)["details"]["login_result"], "WrongPassword");
    let (code, doc) = run_cli_with(
        &core,
        &["auth", "login"],
        &[("TAPHOME_USER", Some("nobody"))],
    );
    assert_eq!(
        (code, error(&doc)["details"]["login_result"].clone()),
        (3, json!("WrongLogin"))
    );
    assert!(!std::path::Path::new(&core.config_dir().join("session.json")).exists());
}

#[test]
fn missing_config_names_the_variables() {
    let core = FakeCore::start();
    let (code, doc) = run_cli_with(
        &core,
        &["auth", "login"],
        &[("TAPHOME_PASSWORD", None), ("TAPHOME_HOST", None)],
    );
    assert_eq!(code, 3);
    assert_eq!(error(&doc)["code"], "missing_config");
    let missing = error(&doc)["details"]["missing"].to_string();
    assert!(
        missing.contains("TAPHOME_PASSWORD") && missing.contains("TAPHOME_HOST"),
        "{missing}"
    );
}

#[test]
fn password_and_user_can_come_from_files() {
    let core = FakeCore::start();
    let dir = std::env::temp_dir().join(format!("taphome-files-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("pw"), format!("{PASSWORD}\n")).unwrap();
    let pw_file = dir.join("pw");
    let overrides = [
        ("TAPHOME_PASSWORD", None),
        ("TAPHOME_PASSWORD_FILE", Some(pw_file.to_str().unwrap())),
    ];
    let (code, doc) = run_cli_with(&core, &["auth", "login"], &overrides);
    std::fs::remove_dir_all(&dir).unwrap();
    assert_eq!(code, 0, "{doc}");
}

#[test]
fn salt_missing_and_salt_sources() {
    let core = FakeCore::start();
    let mut env = core.env();
    env.remove("TAPHOME_PASSWORD_SALT");
    // No env salt and an empty built-in salt: refuses to hash.
    let (code, doc) = run_with_salt(&["auth", "login"], env.clone(), b"");
    assert_eq!(code, 3);
    assert_eq!(error(&doc)["code"], "salt_missing");
    assert!(error(&doc)["message"]
        .as_str()
        .unwrap()
        .contains("TAPHOME_PASSWORD_SALT"));
    assert_eq!(
        data(&run_with_salt(&["auth", "status"], env.clone(), b"").1)["salt_source"],
        "missing"
    );
    // Built-in salt works without any variable.
    let (code, doc) = run_with_salt(&["auth", "login"], env.clone(), SALT.as_bytes());
    assert_eq!(code, 0, "{doc}");
    assert_eq!(
        data(&run_with_salt(&["auth", "status"], env.clone(), SALT.as_bytes()).1)["salt_source"],
        "built-in"
    );
    // The environment overrides a wrong built-in salt.
    core.forget_sessions();
    let (code, doc) = run_cli(&core, &["auth", "login"]);
    assert_eq!(code, 0, "{doc}");
    assert_eq!(
        data(&run_cli(&core, &["auth", "status"]).1)["salt_source"],
        "env"
    );
    // A stored session keeps working without a salt, the password is not needed.
    assert_eq!(run_with_salt(&["sync"], env, b"").0, 0);
}

#[test]
fn status_reports_session_state_without_the_address() {
    let core = FakeCore::start();
    let (_, doc) = run_cli(&core, &["auth", "status"]);
    assert_eq!(data(&doc)["stored"], false);
    assert_eq!(data(&doc)["valid"], false);
    assert_eq!(run_cli(&core, &["auth", "login"]).0, 0);
    let (code, doc) = run_cli(&core, &["auth", "status"]);
    assert_eq!(code, 0);
    let d = data(&doc);
    assert_eq!(
        (d["stored"].clone(), d["valid"].clone()),
        (json!(true), json!(true))
    );
    assert_eq!(d["core_version"], "2025.2.99999");
    assert_eq!(d["protocol"], 89);
    assert_eq!(d["host_configured"], true);
    assert!(d.get("host").is_none());
    assert!(!doc.to_string().contains(&core.host()));
    let (_, shown) = run_cli(&core, &["auth", "status", "--show-secrets"]);
    assert_eq!(data(&shown)["host"], core.host());
    core.forget_sessions();
    assert_eq!(data(&run_cli(&core, &["auth", "status"]).1)["valid"], false);
}

#[test]
fn logout_deletes_the_session() {
    let core = FakeCore::start();
    assert_eq!(run_cli(&core, &["auth", "login"]).0, 0);
    assert_eq!(
        data(&run_cli(&core, &["auth", "logout"]).1)["session_removed"],
        true
    );
    assert_eq!(
        data(&run_cli(&core, &["auth", "logout"]).1)["session_removed"],
        false
    );
    assert_eq!(
        data(&run_cli(&core, &["auth", "status"]).1)["stored"],
        false
    );
}

#[test]
fn usage_errors_are_json_and_help_is_text() {
    let core = FakeCore::start();
    let (code, doc) = run_cli(&core, &["sync", "--section", "nope"]);
    assert_eq!(code, 2);
    assert_eq!(error(&doc)["code"], "usage");
    assert_eq!(run_cli(&core, &["frobnicate"]).0, 2);
    assert_eq!(
        run_cli(&core, &["sync", "--full", "--section", "core"]).0,
        2
    );
    assert_eq!(run_cli(&core, &["--timeout", "0", "sync"]).0, 2);
    let (code, doc) = run_cli(&core, &["--help"]);
    assert_eq!(code, 0);
    assert!(doc.as_str().unwrap().contains("Usage"));
    let (code, doc) = run_cli(&core, &["--version"]);
    assert_eq!(code, 0);
    assert!(doc.as_str().unwrap().starts_with("taphome "));
    assert!(core.requests().is_empty());
}

#[test]
fn placeholder_groups_report_internal_error() {
    let core = FakeCore::start();
    for group in ["device", "pp", "rule", "io"] {
        let (code, doc) = run_cli(&core, &[group, "todo"]);
        assert_eq!(
            (code, error(&doc)["code"].clone()),
            (1, json!("internal")),
            "{group}"
        );
    }
    // The hidden subcommand stays out of the help.
    let (_, help) = run_cli(&core, &["device", "--help"]);
    assert!(!help.as_str().unwrap().contains("todo"));
}

#[test]
fn connection_failure_and_timeout_exit_4() {
    let core = FakeCore::start();
    let (code, doc) = run_cli_with(
        &core,
        &["auth", "login"],
        &[("TAPHOME_HOST", Some("127.0.0.1:1"))],
    );
    assert_eq!(
        (code, error(&doc)["code"].clone()),
        (4, json!("connection_failed"))
    );
    assert!(
        !doc.to_string().contains("127.0.0.1:1"),
        "the address stays out of errors"
    );

    assert_eq!(run_cli(&core, &["auth", "login"]).0, 0);
    core.silent_next_request();
    let (code, doc) = run_cli(
        &core,
        &[
            "--timeout",
            "1",
            "raw",
            "call",
            "MessageGetBasicCorePropertiesRequest",
        ],
    );
    assert_eq!((code, error(&doc)["code"].clone()), (4, json!("timeout")));
}

#[test]
fn pretty_flag_is_accepted_anywhere() {
    let core = FakeCore::start();
    let (code, doc) = run_cli(&core, &["--pretty", "auth", "status"]);
    assert_eq!(code, 0);
    assert!(matches!(doc, Value::Object(_)));
    assert_eq!(run_cli(&core, &["auth", "status", "--pretty"]).0, 0);
}
