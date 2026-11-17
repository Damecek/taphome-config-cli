mod common;
use common::*;
use serde_json::{json, Value};

fn seed_values(core: &FakeCore) {
    core.with_state(|s| {
        for d in s.devices.iter_mut() {
            let status = match d["ID"].as_i64().unwrap() {
                10 => 0,
                11 => 4,
                12 => (1 << 8) | (2 << 16),
                _ => 0,
            };
            d["CurrentValues"] = json!({
                "7": {"Type": 7, "Value": status, "ChangedOnTHUtc": 343340580},
                "48": {"Type": 48, "Value": 1.0, "ChangedOnTHUtc": 0},
            });
        }
    });
}

fn device(doc: &Value, id: i64) -> &Value {
    data(doc)["devices"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["id"] == id)
        .unwrap()
}

#[test]
fn values_decode_names_status_and_time() {
    let core = FakeCore::start();
    seed_values(&core);
    let (code, doc) = run_cli(&core, &["io", "values"]);
    assert_eq!(code, 0, "{doc}");
    assert_eq!(data(&doc)["summary"]["devices"], 5);
    assert_eq!(data(&doc)["summary"]["status_not_ok"], json!([11, 12]));
    let ok = &device(&doc, 10)["values"];
    assert_eq!(ok[0]["name"], "DeviceStatus");
    assert_eq!(ok[0]["status"]["name"], "OK");
    assert_eq!(ok[0]["changed_at"], "2026-11-17T20:23:00Z");
    assert_eq!(ok[1]["name"], "SwitchState");
    assert_eq!(ok[1]["changed_at"], "2016-01-01T00:00:00Z");
    assert_eq!(
        device(&doc, 11)["values"][0]["status"]["name"],
        "CommunicationError"
    );
    let packed = &device(&doc, 12)["values"][0]["status"];
    assert_eq!(
        (
            packed["errors"].clone(),
            packed["warnings"].clone(),
            packed["infos"].clone()
        ),
        (json!(1), json!(2), json!(0))
    );
}

#[test]
fn values_filters() {
    let core = FakeCore::start();
    seed_values(&core);
    let (_, doc) = run_cli(&core, &["io", "values", "10", "Test thermometer"]);
    assert_eq!(data(&doc)["summary"]["devices"], 2);
    let (_, doc) = run_cli(&core, &["io", "values", "--model", "Virtual*"]);
    assert_eq!(data(&doc)["summary"]["devices"], 3);
    let (_, doc) = run_cli(
        &core,
        &["io", "values", "--name", "Parser", "--model", "*Variable"],
    );
    assert_eq!(data(&doc)["summary"]["devices"], 1);
    let (code, doc) = run_cli(&core, &["io", "values", "999"]);
    assert_eq!((code, error(&doc)["code"].clone()), (6, json!("not_found")));
}

fn entries_handler(core: &FakeCore) {
    core.on("MessageGetDeviceLogRequest", |_, _| {
        let entry =
            |t: i64, v: f64| json!({"TapHomeTime": t, "Values": [[48, v]], "ChangeOriginId": 1});
        Reply::new(
            "MessageGetDeviceLogResponse",
            json!({"ValueChangedLogEntries": [entry(30, 1.0), entry(10, 0.0), entry(20, 1.0)]}),
        )
    });
}

#[test]
fn log_converts_times_and_limits_to_newest() {
    let core = FakeCore::start();
    entries_handler(&core);
    let (code, doc) = run_cli(&core, &["io", "log", "10"]);
    assert_eq!(code, 0, "{doc}");
    let all = data(&doc)["entries"].as_array().unwrap();
    assert_eq!(all.len(), 3);
    assert_eq!(all[0]["time_th"], 10);
    assert_eq!(all[0]["time"], "2016-01-01T00:00:10Z");
    assert_eq!(all[0]["values"][0]["name"], "SwitchState");

    let (_, doc) = run_cli(&core, &["io", "log", "10", "--limit", "2"]);
    let kept = data(&doc)["entries"].as_array().unwrap();
    assert_eq!(
        (
            data(&doc)["total"].clone(),
            kept.len(),
            kept[1]["time_th"].clone()
        ),
        (json!(3), 2, json!(30))
    );
    assert_eq!(kept[0]["time_th"], 20);
}

#[test]
fn errors_decode_and_pick_origin() {
    let core = FakeCore::start();
    core.on("MessageGetDeviceErrorsRequest", |_, req| {
        assert_ne!(req["ShouldSubscribe"], json!(true));
        Reply::new(
            "MessageGetDeviceErrorsResponse",
            json!({"DeviceErrors": [{"TapHomeTime": 60, "ErrorCodeId": 2, "SeverityId": 3, "OriginId": req["OriginId"].clone(), "CustomText": "boom"}]}),
        )
    });
    let (code, doc) = run_cli(&core, &["io", "errors", "10"]);
    assert_eq!(code, 0, "{doc}");
    assert_eq!(data(&doc)["origin_id"], 0);
    let e = &data(&doc)["errors"][0];
    assert_eq!(
        (
            e["time"].clone(),
            e["severity"].clone(),
            e["CustomText"].clone()
        ),
        (json!("2016-01-01T00:01:00Z"), json!("error"), json!("boom"))
    );

    let (_, doc) = run_cli(&core, &["io", "errors", "20"]);
    assert_eq!(data(&doc)["origin_id"], 1);
    let (_, doc) = run_cli(&core, &["io", "errors", "10", "--origin", "3"]);
    assert_eq!(data(&doc)["origin_id"], 3);
    assert_eq!(
        run_cli(&core, &["io", "errors", "10", "--origin", "9"]).0,
        2
    );
}

#[test]
fn unknown_device_and_core_error() {
    let core = FakeCore::start();
    for cmd in ["errors", "log"] {
        let (code, doc) = run_cli(&core, &["io", cmd, "999"]);
        assert_eq!((code, error(&doc)["code"].clone()), (6, json!("not_found")));
    }
    core.fail_next(3, "boom");
    // The first request after reconnect is the read itself.
    let (code, doc) = run_cli(&core, &["io", "log", "10"]);
    assert_eq!(code, 5, "{doc}");
    assert_eq!(error(&doc)["core_error_code"], 3);
}

#[test]
fn watch_collects_filtered_changes_and_counts_classes() {
    let core = FakeCore::start();
    let (code, doc) = std::thread::scope(|scope| {
        scope.spawn(|| {
            std::thread::sleep(std::time::Duration::from_millis(700));
            core.push_broadcast(
                "MessageDeviceValueChangedBroadcast",
                json!({"DeviceId": 10, "ValueTypeId": 48, "Value": 1.0}),
            );
            core.push_broadcast(
                "MessageDeviceValueChangedBroadcast",
                json!({"DeviceId": 11, "ValueTypeId": 62, "Value": 5.0}),
            );
            core.push_broadcast("MessageDeviceAggregatedValueChangedBroadcast", json!({}));
            core.push_broadcast(
                "MessageDeviceValueChangedBroadcast",
                json!({"DeviceId": 10, "ValueTypeId": 48, "Value": 0.0}),
            );
        });
        run_cli(
            &core,
            &["io", "watch", "--seconds", "2", "--device", "Test lamp"],
        )
    });
    assert_eq!(code, 0, "{doc}");
    let changes = data(&doc)["changes"].as_array().unwrap();
    assert_eq!(changes.len(), 2, "{doc}");
    assert_eq!(changes[0]["device_name"], "Test lamp");
    assert_eq!(changes[0]["name"], "SwitchState");
    assert_eq!(
        (changes[0]["value"].clone(), changes[1]["value"].clone()),
        (json!(1.0), json!(0.0))
    );
    assert_eq!(
        data(&doc)["broadcasts"]["MessageDeviceValueChangedBroadcast"],
        3
    );
    assert_eq!(
        data(&doc)["broadcasts"]["MessageDeviceAggregatedValueChangedBroadcast"],
        1
    );

    assert_eq!(run_cli(&core, &["io", "watch", "--seconds", "301"]).0, 2);
    let (code, _) = run_cli(&core, &["io", "watch", "--seconds", "0", "--device", "999"]);
    assert_eq!(code, 6);
}
