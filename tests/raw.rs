mod common;
use common::*;
use serde_json::json;

#[test]
fn ops_lists_and_filters_the_registry() {
    let core = FakeCore::start();
    let (code, doc) = run_cli(&core, &["raw", "ops"]);
    assert_eq!(code, 0);
    let all = data(&doc)["count"].as_u64().unwrap();
    assert!(all > 100);
    let add = data(&doc)["operations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["class"] == "MessageAddDeviceRequest")
        .unwrap()
        .clone();
    assert_eq!(
        (add["kind"].clone(), add["verified"].clone()),
        (json!("write"), json!(true))
    );

    let (_, doc) = run_cli(
        &core,
        &["raw", "ops", "--kind", "write", "--verified", "false"],
    );
    let ops = data(&doc)["operations"].as_array().unwrap();
    assert!(
        !ops.is_empty()
            && ops
                .iter()
                .all(|o| o["kind"] == "write" && o["verified"] == false)
    );
    assert_eq!(run_cli(&core, &["raw", "ops", "--verified", "maybe"]).0, 2);
    assert!(core.requests().is_empty(), "ops is local");
}

#[test]
fn verified_read_runs_by_default() {
    let core = FakeCore::start();
    let (code, doc) = run_cli(
        &core,
        &["raw", "call", "MessageGetBasicCorePropertiesRequest"],
    );
    assert_eq!(code, 0, "{doc}");
    assert_eq!(
        data(&doc)["response"]["$type"],
        "MessageGetBasicCorePropertiesResponse"
    );
    assert_eq!(data(&doc)["response"]["Properties"][0]["Name"], "Core Name");
}

#[test]
fn read_with_json_fields_and_core_error() {
    let core = FakeCore::start();
    let (code, doc) = run_cli(
        &core,
        &[
            "raw",
            "call",
            "MessageGetDeviceLogRequest",
            "--json",
            r#"{"DeviceId": 10}"#,
        ],
    );
    assert_eq!(code, 0, "{doc}");
    let (code, doc) = run_cli(
        &core,
        &[
            "raw",
            "call",
            "MessageGetDeviceLogRequest",
            "--json",
            r#"{"DeviceId": 999}"#,
        ],
    );
    assert_eq!(code, 5);
    assert_eq!(error(&doc)["code"], "does_not_exist");
    assert_eq!(error(&doc)["core_error_code"], 6);
}

#[test]
fn json_from_a_file() {
    let core = FakeCore::start();
    let file = std::env::temp_dir().join(format!("taphome-json-{}.json", std::process::id()));
    std::fs::write(&file, r#"{"DeviceId": 10}"#).unwrap();
    let (code, doc) = run_cli(
        &core,
        &[
            "raw",
            "call",
            "MessageGetDeviceErrorsRequest",
            "--json-file",
            file.to_str().unwrap(),
        ],
    );
    std::fs::remove_file(&file).unwrap();
    assert_eq!(code, 0, "{doc}");
    assert!(data(&doc)["response"]["DeviceErrors"]
        .as_array()
        .is_none_or(|a| a.is_empty()));
}

#[test]
fn unverified_and_unknown_classes_need_allow_unverified() {
    let core = FakeCore::start();
    let (code, doc) = run_cli(&core, &["raw", "call", "MessageGetAccessCardsRequest"]);
    assert_eq!(
        (code, error(&doc)["code"].clone()),
        (7, json!("unverified_operation"))
    );
    assert!(core.requests().is_empty());
    // Allowed: the fake Core does not implement it and answers RequestNotSupported (22).
    let (code, doc) = run_cli(
        &core,
        &[
            "raw",
            "call",
            "MessageGetAccessCardsRequest",
            "--allow-unverified",
        ],
    );
    assert_eq!(
        (code, error(&doc)["code"].clone()),
        (5, json!("request_not_supported"))
    );
    // Not a request class at all.
    let (code, doc) = run_cli(&core, &["raw", "call", "NoSuchMessage"]);
    assert_eq!(
        (code, error(&doc)["code"].clone()),
        (2, json!("invalid_input"))
    );
    let (code, _) = run_cli(
        &core,
        &["raw", "call", "MessageGetBasicCorePropertiesResponse"],
    );
    assert_eq!(code, 2, "a response class cannot be sent");
}

#[test]
fn write_classes_need_allow_write() {
    let core = FakeCore::start();
    let fields = r#"{"DeviceName": "TEST raw", "Icon": 0, "Interface": "Variable", "Model": "VirtualVariableGeneral", "DeviceTypeId": 0}"#;
    let (code, doc) = run_cli(
        &core,
        &["raw", "call", "MessageAddDeviceRequest", "--json", fields],
    );
    assert_eq!(
        (code, error(&doc)["code"].clone()),
        (7, json!("write_not_allowed"))
    );
    assert!(core.writes().is_empty());
    assert_eq!(core.with_state(|s| s.devices.len()), 5);

    let (code, doc) = run_cli(
        &core,
        &[
            "raw",
            "call",
            "MessageAddDeviceRequest",
            "--json",
            fields,
            "--allow-write",
        ],
    );
    assert_eq!(code, 0, "{doc}");
    let d = data(&doc);
    assert_eq!(d["response"]["AddedDevice"]["Name"], "TEST raw");
    assert_eq!(
        d["delta"],
        json!({"devices": 1, "rules_live": 0, "rules_total": 0, "interfaces": 0, "dashboards": 0})
    );
    assert_eq!(d["verify"]["ok"], true);
    assert_eq!(
        d["broadcasts"],
        json!(["MessageDeviceListUpdatedBroadcast"])
    );
    assert_eq!(core.writes(), ["MessageAddDeviceRequest"]);
}

#[test]
fn unverified_write_needs_both_switches() {
    let core = FakeCore::start();
    let (code, doc) = run_cli(
        &core,
        &["raw", "call", "MessageAddCcuRequest", "--allow-write"],
    );
    assert_eq!(
        (code, error(&doc)["code"].clone()),
        (7, json!("unverified_operation"))
    );
    let (code, doc) = run_cli(
        &core,
        &[
            "raw",
            "call",
            "MessageAddCcuRequest",
            "--allow-write",
            "--allow-unverified",
        ],
    );
    assert_eq!(
        (code, error(&doc)["code"].clone()),
        (5, json!("request_not_supported"))
    );
}

#[test]
fn dry_run_sends_nothing_and_shows_the_frame() {
    let core = FakeCore::start();
    let (code, doc) = run_cli(
        &core,
        &[
            "raw",
            "call",
            "MessageDeleteDeviceRequest",
            "--json",
            r#"{"DeviceId": 10}"#,
            "--dry-run",
        ],
    );
    assert_eq!(code, 0, "{doc}");
    let d = data(&doc);
    assert_eq!(d["dry_run"], true);
    assert_eq!(d["diff"], json!(null));
    assert_eq!(
        d["message"],
        json!({"class": "MessageDeleteDeviceRequest", "fields": {"DeviceId": 10}})
    );
    let frame = d["frame_hex"].as_str().unwrap();
    assert!(!frame.is_empty() && frame.chars().all(|c| c.is_ascii_hexdigit()));
    assert!(core.requests().is_empty(), "no connection at all");
}

#[test]
fn bad_input_is_a_usage_level_error() {
    let core = FakeCore::start();
    for args in [
        vec![
            "raw",
            "call",
            "MessageGetDeviceLogRequest",
            "--json",
            "{nope",
        ],
        vec!["raw", "call", "MessageGetDeviceLogRequest", "--json", "[1]"],
        vec![
            "raw",
            "call",
            "MessageGetDeviceLogRequest",
            "--json",
            r#"{"NoSuchField": 1}"#,
        ],
        vec![
            "raw",
            "call",
            "MessageGetDeviceLogRequest",
            "--json-file",
            "/nonexistent/taphome.json",
        ],
    ] {
        let (code, doc) = run_cli(&core, &args);
        assert_eq!(
            (code, error(&doc)["code"].clone()),
            (2, json!("invalid_input")),
            "{args:?}"
        );
    }
}

#[test]
fn broadcasts_are_collected_for_the_wait_window() {
    let core = FakeCore::start();
    assert_eq!(run_cli(&core, &["auth", "login"]).0, 0);
    core.on("MessageGetBasicCorePropertiesRequest", |_state, _req| {
        Reply::new(
            "MessageGetBasicCorePropertiesResponse",
            json!({"Properties": []}),
        )
        .with_broadcast(
            "MessageDeviceValueChangedBroadcast",
            json!({"DeviceId": 10, "ValueTypeId": 48, "Value": 1.0}),
        )
    });
    let (code, doc) = run_cli(
        &core,
        &[
            "raw",
            "call",
            "MessageGetBasicCorePropertiesRequest",
            "--wait-broadcasts",
            "0.3",
        ],
    );
    assert_eq!(code, 0, "{doc}");
    assert_eq!(
        data(&doc)["broadcasts"]["MessageDeviceValueChangedBroadcast"],
        1
    );
}

#[test]
fn class_without_registry_entry_needs_both_switches_and_sends_nothing() {
    let core = FakeCore::start();
    let class = "MessageAlexaSetDeviceRequest";
    let (code, doc) = run_cli(&core, &["raw", "call", class]);
    assert_eq!(
        (code, error(&doc)["code"].clone()),
        (7, json!("write_not_allowed"))
    );
    let (code, doc) = run_cli(&core, &["raw", "call", class, "--allow-write"]);
    assert_eq!(
        (code, error(&doc)["code"].clone()),
        (7, json!("unverified_operation"))
    );
    let (code, doc) = run_cli(&core, &["raw", "call", class, "--allow-unverified"]);
    assert_eq!(
        (code, error(&doc)["code"].clone()),
        (7, json!("write_not_allowed"))
    );
    assert!(core.requests().is_empty(), "nothing was sent");
}

#[test]
fn dry_run_redacts_the_message_fields_like_the_frame() {
    let core = FakeCore::start();
    let (code, doc) = run_cli(
        &core,
        &[
            "raw",
            "call",
            "MessageLoginRequest",
            "--json",
            r#"{"LoginOrEmail": "u", "PasswordHashOrAuthorizationToken": "tok-123"}"#,
            "--dry-run",
        ],
    );
    assert_eq!(code, 0, "{doc}");
    let d = data(&doc);
    assert_eq!(
        d["message"]["fields"]["PasswordHashOrAuthorizationToken"],
        "<redacted>"
    );
    assert!(!doc.to_string().contains("tok-123"));
}
