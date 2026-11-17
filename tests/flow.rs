//! The write flow and the lookups, through a small test-only write (rename a device).
mod common;
use common::*;
use serde_json::{json, Value};
use taphome_cli::core::{
    diff_entry, open_session, run_write, Check, Ctx, Snapshot, WriteFlags, WritePlan, WriteSpec,
};
use taphome_cli::errors::CliError;

const CLASS: &str = "MessageUpdateMultipleDevicesPropertiesRequest";

fn rename(
    ctx: &Ctx,
    flags: WriteFlags,
    variant_verified: bool,
    name: &str,
) -> Result<Value, CliError> {
    let name = name.to_string();
    let mut spec = WriteSpec::new(
        "updateMultipleDevicesProperties",
        CLASS,
        flags,
        move |snapshot| {
            let device = snapshot.find_device("Test lamp")?;
            let id = device["ID"].as_i64().unwrap();
            let diff = vec![diff_entry(
                "change",
                "device",
                json!(id),
                "Name",
                device["Name"].clone(),
                json!(name),
            )];
            Ok(WritePlan::new(
                json!({"DeviceIds": [id], "DeviceProperties": {"Name": name}}),
                diff,
                move |after, _response| {
                    let ok = after.device(id).is_some_and(|d| d["Name"] == json!(name));
                    vec![Check::new("device has the new name", ok)
                        .detail("compared with a fresh snapshot")]
                },
            ))
        },
    );
    spec.variant_verified = variant_verified;
    block(run_write(ctx, spec))
}

fn flags(dry_run: bool, allow_unverified: bool) -> WriteFlags {
    WriteFlags {
        dry_run,
        allow_unverified,
    }
}

#[test]
fn dry_run_sends_no_write() {
    let core = FakeCore::start();
    let doc = rename(&core.ctx(), flags(true, false), true, "TEST renamed").unwrap();
    assert_eq!(doc["dry_run"], true);
    assert_eq!(doc["operation"], "updateMultipleDevicesProperties");
    assert_eq!(doc["verified"], true);
    assert_eq!(doc["message"]["class"], CLASS);
    assert_eq!(
        doc["message"]["fields"],
        json!({"DeviceIds": [10], "DeviceProperties": {"Name": "TEST renamed"}})
    );
    assert_eq!(doc["diff"][0]["op"], "change");
    assert_eq!(doc["diff"][0]["before"], "Test lamp");
    assert!(core.writes().is_empty());
    assert_eq!(
        core.with_state(|s| s.devices[0]["Name"].clone()),
        "Test lamp"
    );
}

#[test]
fn successful_write_is_verified_in_a_new_session() {
    let core = FakeCore::start();
    let doc = rename(&core.ctx(), flags(false, false), true, "TEST renamed").unwrap();
    assert_eq!(doc["operation"], "updateMultipleDevicesProperties");
    assert_eq!(doc["response"]["ErrorCode"], 1);
    assert_eq!(doc["broadcasts"], json!(["MessageDeviceUpdatedBroadcast"]));
    assert_eq!(doc["verify"]["ok"], true);
    assert_eq!(
        doc["verify"]["checks"][0]["name"],
        "device has the new name"
    );
    assert_eq!(doc["state"]["devices"], 5);
    assert_eq!(doc["state_before"]["devices"], 5);
    assert_eq!(core.writes(), [CLASS]);
    // One login, then a session for the snapshot and a new one for the verification.
    assert_eq!(core.logins(), 1);
    let reconnects = core
        .requests()
        .iter()
        .filter(|c| *c == "MessageReconnectRequest")
        .count();
    assert_eq!(
        reconnects, 3,
        "stale DbHash retry on the first session, then one reconnect each for the next two"
    );
    assert_eq!(
        core.with_state(|s| s.devices[0]["Name"].clone()),
        "TEST renamed"
    );
}

#[test]
fn failed_verification_exits_8_and_still_reports_the_answer() {
    let core = FakeCore::start();
    core.drop_next_write();
    let err = rename(&core.ctx(), flags(false, false), true, "TEST renamed").unwrap_err();
    assert_eq!((err.code.as_str(), err.exit_code), ("verify_failed", 8));
    assert_eq!(err.details["response"]["ErrorCode"], 1, "the Core said Ok");
    assert_eq!(err.details["verify"]["ok"], false);
    assert_eq!(err.details["verify"]["checks"][0]["ok"], false);
    assert_eq!(err.details["broadcasts"], json!([]));
    assert_eq!(
        core.with_state(|s| s.devices[0]["Name"].clone()),
        "Test lamp"
    );
}

#[test]
fn unverified_operation_is_refused_without_the_switch() {
    let core = FakeCore::start();
    let ctx = core.ctx();
    let attempt = |allow: bool| {
        let spec = WriteSpec::new(
            "addInterface",
            "MessageAddCcuRequest",
            flags(false, allow),
            |_s| Ok(WritePlan::new(json!({}), vec![], |_a, _r| vec![])),
        );
        block(run_write(&ctx, spec))
    };
    let err = attempt(false).unwrap_err();
    assert_eq!(
        (err.code.as_str(), err.exit_code),
        ("unverified_operation", 7)
    );
    assert!(core.writes().is_empty());
    // With the switch it is sent, and the fake Core rejects it: a Core error, exit 5.
    let err = attempt(true).unwrap_err();
    assert_eq!(
        (err.code.as_str(), err.exit_code, err.core_error_code),
        ("request_not_supported", 5, Some(22))
    );
    assert_eq!(core.writes(), ["MessageAddCcuRequest"]);
}

#[test]
fn unverified_variant_of_a_verified_operation_needs_the_switch() {
    let core = FakeCore::start();
    let ctx = core.ctx();
    let err = rename(&ctx, flags(false, false), false, "TEST x").unwrap_err();
    assert_eq!(
        (err.code.as_str(), err.exit_code),
        ("unverified_operation", 7)
    );
    assert!(core.writes().is_empty());
    // A dry run is allowed and reports verified false.
    let doc = rename(&ctx, flags(true, false), false, "TEST x").unwrap();
    assert_eq!(doc["verified"], false);
    // With the switch it goes through.
    assert_eq!(
        rename(&ctx, flags(false, true), false, "TEST x").unwrap()["verify"]["ok"],
        true
    );
}

#[test]
fn core_errors_map_to_named_codes_and_exit_5() {
    let core = FakeCore::start();
    let ctx = core.ctx();
    for (code, name) in [
        (2, "unauthorized"),
        (5, "already_exist"),
        (6, "does_not_exist"),
        (7, "invalid_request"),
        (21, "core_offline"),
        (22, "request_not_supported"),
    ] {
        core.fail_next(code, "refused");
        let err = rename(&ctx, flags(false, false), true, "TEST y").unwrap_err();
        assert_eq!(
            (err.code.as_str(), err.exit_code, err.core_error_code),
            (name, 5, Some(code))
        );
        assert_eq!(err.message, "refused");
    }
    // An unknown code keeps its number.
    core.fail_next(99, "odd");
    let err = rename(&ctx, flags(false, false), true, "TEST y").unwrap_err();
    assert_eq!(
        (err.code.as_str(), err.core_error_code),
        ("core_error", Some(99))
    );
}

#[test]
fn failed_generic_response_with_ok_code_is_not_handled() {
    let core = FakeCore::start();
    core.on(CLASS, |_state, _req| {
        Reply::new(
            "MessageGenericResponse",
            json!({"ResponseFlagsId": 2, "ErrorMessage": "Ok", "ErrorCode": 1}),
        )
    });
    let err = rename(&core.ctx(), flags(false, false), true, "TEST z").unwrap_err();
    assert_eq!((err.code.as_str(), err.exit_code), ("not_handled", 5));
}

#[test]
fn wrong_response_class_is_a_protocol_error() {
    let core = FakeCore::start();
    let ctx = core.ctx();
    let mut spec = WriteSpec::new(
        "updateMultipleDevicesProperties",
        CLASS,
        flags(false, false),
        |_s| {
            Ok(WritePlan::new(
                json!({"DeviceIds": [10], "DeviceProperties": {"Name": "q"}}),
                vec![],
                |_a, _r| vec![],
            ))
        },
    );
    spec.expect = Some("MessageAddDeviceResponse");
    let err = block(run_write(&ctx, spec)).unwrap_err();
    assert_eq!((err.code.as_str(), err.exit_code), ("protocol_error", 4));
}

#[test]
fn lookups_by_id_and_exact_name() {
    let core = FakeCore::start();
    let ctx = core.ctx();
    let snapshot = block(async {
        let session = open_session(&ctx).await.unwrap();
        let snapshot = Snapshot::from_session(&session).unwrap();
        session.close().await;
        snapshot
    });
    assert_eq!(snapshot.find_device("10").unwrap()["Name"], "Test lamp");
    assert_eq!(snapshot.find_device("Test lamp").unwrap()["ID"], 10);
    assert_eq!(
        snapshot.find_device("test lamp").unwrap_err().code,
        "not_found"
    );
    assert_eq!(snapshot.find_device("999").unwrap_err().exit_code, 6);
    assert_eq!(snapshot.find_rule("1").unwrap().name, "Test rule A");
    assert_eq!(snapshot.find_rule("Test rule B").unwrap().id, 2);
    // Tombstones are ignored, by id and by name.
    assert_eq!(snapshot.find_rule("3").unwrap_err().code, "not_found");
    assert_eq!(
        snapshot.find_rule("Old rule").unwrap_err().code,
        "not_found"
    );
    assert_eq!(snapshot.rules.len(), 3);
    assert!(snapshot.rules[2].deleted && !snapshot.rules[2].raw_hex.is_empty());
    assert_eq!(
        snapshot.find_interface("Packet parser").unwrap()["CcuId"],
        2
    );
    assert_eq!(snapshot.trigger_version(12), Some(13));
    assert_eq!(snapshot.counts()["rules_live"], 2);
    assert_eq!(
        (snapshot.core_version.as_str(), snapshot.protocol),
        ("2025.2.99999", 89)
    );
}

#[test]
fn duplicate_names_are_ambiguous_and_the_id_still_works() {
    let core = FakeCore::start();
    core.with_state(|s| {
        let mut twin = s.devices[0].clone();
        twin["ID"] = json!(50);
        s.devices.push(twin);
    });
    let ctx = core.ctx();
    let snapshot = block(async {
        let session = open_session(&ctx).await.unwrap();
        let snapshot = Snapshot::from_session(&session).unwrap();
        session.close().await;
        snapshot
    });
    let err = snapshot.find_device("Test lamp").unwrap_err();
    assert_eq!((err.code.as_str(), err.exit_code), ("ambiguous", 6));
    assert_eq!(err.details["ids"], json!([10, 50]));
    assert_eq!(snapshot.find_device("50").unwrap()["ID"], 50);
}

#[test]
fn request_refuses_write_classes() {
    let core = FakeCore::start();
    let ctx = core.ctx();
    let err = block(async {
        let session = open_session(&ctx).await.unwrap();
        let err = taphome_cli::core::request(&ctx, &session, CLASS, &json!({}), None)
            .await
            .unwrap_err();
        session.close().await;
        err
    });
    assert_eq!(err.code, "internal");
    assert!(core.writes().is_empty());
}

#[test]
fn a_write_that_times_out_is_reported_as_unknown() {
    let core = FakeCore::start();
    let mut ctx = taphome_cli::core::Ctx::new(
        core.env(),
        taphome_cli::core::Global {
            timeout: Some(1.0),
            ..Default::default()
        },
        b"",
    )
    .unwrap();
    ctx.settle = std::time::Duration::from_millis(50);
    // Log in first so only the write itself is silent.
    block(async { open_session(&ctx).await.unwrap().close().await });
    core.silent_next_request();
    let err = rename(&ctx, flags(false, false), true, "TEST silent").unwrap_err();
    assert_eq!((err.code.as_str(), err.exit_code), ("timeout", 4));
    assert_eq!(err.details["write_sent"], "unknown");
}

#[test]
fn a_redacted_placeholder_is_never_written_back() {
    let core = FakeCore::start();
    let doc = rename(&core.ctx(), flags(false, true), true, "<redacted>").unwrap_err();
    assert_eq!(doc.code, "invalid_input");
    assert!(
        doc.message.contains("redacted placeholder"),
        "{}",
        doc.message
    );
    assert!(core.writes().is_empty(), "nothing was sent");
    // also on dry-run, and inside an embedded JSON document
    assert!(rename(&core.ctx(), flags(true, false), true, "<redacted>").is_err());
    assert!(rename(
        &core.ctx(),
        flags(true, false),
        true,
        "{\"Password\":\"<redacted>\"}"
    )
    .is_err());
    assert!(rename(&core.ctx(), flags(true, false), true, "TEST ok").is_ok());
}
