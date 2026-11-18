//! Cloud mode: TapHome account login through the cloud API and the tunnel to the (fake) Core.
mod common;
use common::*;
use serde_json::Value;

fn run(
    core: &FakeCore,
    cloud: &FakeCloud,
    args: &[&str],
    extra: &[(&str, Option<&str>)],
) -> (i32, Value) {
    let base = cloud_overrides(cloud);
    let mut overrides: Vec<(&str, Option<&str>)> =
        base.iter().map(|(k, v)| (*k, v.as_deref())).collect();
    overrides.extend_from_slice(extra);
    run_cli_with(core, args, &overrides)
}

#[test]
fn taphome_account_logs_in_through_the_tunnel_and_reuses_the_session() {
    let core = FakeCore::start();
    let cloud = FakeCloud::start(&core);
    let (code, doc) = run(&core, &cloud, &["auth", "login"], &[]);
    assert_eq!(code, 0, "{doc}");
    assert_eq!(data(&doc)["location"], "Test home");
    assert_eq!(data(&doc)["user"], CLOUD_EMAIL.to_lowercase());
    let stored = stored_session(&core);
    assert_eq!(stored["host"], format!("ws://{}/MobileApp", core.host()));
    assert_eq!(stored["location_id"], LOCATION_ID);
    assert_eq!(stored["location_name"], "Test home");
    for secret in [CLOUD_PASSWORD, CLOUD_TOKEN, &CLOUD_TOKEN.to_lowercase()] {
        assert!(!doc.to_string().contains(secret));
    }

    let (code, doc) = run(&core, &cloud, &["sync"], &[]);
    assert_eq!(code, 0, "{doc}");
    assert_eq!(
        (cloud.logins(), core.logins()),
        (1, 1),
        "the stored session is reused"
    );
    let flags = core.with_state(|s| s.account_flags.clone());
    assert!(
        flags.len() >= 2 && flags.iter().all(|local| !local),
        "{flags:?}"
    );
}

#[test]
fn relay_core_offline_is_retried() {
    let core = FakeCore::start();
    let cloud = FakeCloud::start(&core);
    core.with_state(|s| s.offline_next = 2);
    let (code, doc) = run(&core, &cloud, &["sync"], &[]);
    assert_eq!(code, 0, "{doc}");
    assert_eq!(core.with_state(|s| s.offline_next), 0);
}

#[test]
fn wrong_cloud_password_is_a_login_failure() {
    let core = FakeCore::start();
    let cloud = FakeCloud::start(&core);
    let (code, doc) = run(
        &core,
        &cloud,
        &["auth", "login"],
        &[("TAPHOME_CLOUD_PASSWORD", Some("nope"))],
    );
    assert_eq!(code, 3, "{doc}");
    assert_eq!(error(&doc)["code"], "login_failed");
    assert!(!doc.to_string().contains("nope"));
    assert_eq!(core.logins(), 0);
}

#[test]
fn unknown_location_is_not_found_and_lan_plus_cloud_is_refused() {
    let core = FakeCore::start();
    let cloud = FakeCloud::start(&core);
    let (code, doc) = run(
        &core,
        &cloud,
        &["auth", "login"],
        &[("TAPHOME_LOCATION", Some("Office"))],
    );
    assert_eq!((code, error(&doc)["code"].as_str()), (6, Some("not_found")));
    let host = core.host();
    let (code, doc) = run(
        &core,
        &cloud,
        &["sync"],
        &[("TAPHOME_HOST", Some(host.as_str()))],
    );
    assert_eq!(
        (code, error(&doc)["code"].as_str()),
        (3, Some("missing_config"))
    );
}

#[test]
fn a_lan_session_is_not_used_in_cloud_mode_and_status_ignores_it() {
    let core = FakeCore::start();
    let cloud = FakeCloud::start(&core);
    assert_eq!(run_cli(&core, &["auth", "login"]).0, 0);
    let (code, doc) = run(&core, &cloud, &["auth", "status"], &[]);
    assert_eq!(code, 0, "{doc}");
    assert_eq!(
        (
            data(&doc)["stored"].as_bool(),
            data(&doc)["valid"].as_bool()
        ),
        (Some(false), Some(false))
    );
    let (code, doc) = run(&core, &cloud, &["sync"], &[]);
    assert_eq!(code, 0, "{doc}");
    assert_eq!(
        cloud.logins(),
        1,
        "cloud mode logs in with the TapHome account"
    );
    assert_eq!(stored_session(&core)["user"], CLOUD_EMAIL.to_lowercase());
}

#[test]
fn location_is_matched_by_name_for_a_stored_session() {
    let core = FakeCore::start();
    let cloud = FakeCloud::start(&core);
    let by_name = [("TAPHOME_LOCATION", Some("test HOME"))];
    assert_eq!(run(&core, &cloud, &["auth", "login"], &by_name).0, 0);
    assert_eq!(run(&core, &cloud, &["sync"], &by_name).0, 0);
    assert_eq!(
        run(
            &core,
            &cloud,
            &["sync"],
            &[("TAPHOME_LOCATION", Some(LOCATION_ID))]
        )
        .0,
        0
    );
    assert_eq!(
        cloud.logins(),
        1,
        "name and id both match the stored session"
    );
}
