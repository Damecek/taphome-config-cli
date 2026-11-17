mod common;
use common::*;
use serde_json::{json, Value};

const UPDATE: &str = "MessageUpdateMultipleDevicesPropertiesRequest";

fn ids(doc: &Value) -> Vec<i64> {
    data(doc)["devices"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["ID"].as_i64().unwrap())
        .collect()
}

fn device_count(core: &FakeCore) -> usize {
    core.with_state(|s| s.devices.len())
}

fn prop(core: &FakeCore, id: i64, key: &str) -> Value {
    core.with_state(|s| {
        s.devices
            .iter()
            .find(|d| d["ID"] == id)
            .map(|d| d["DeviceProperties"][key].clone())
            .unwrap_or(Value::Null)
    })
}

fn name_of(core: &FakeCore, id: i64) -> Value {
    core.with_state(|s| s.devices.iter().find(|d| d["ID"] == id).unwrap()["Name"].clone())
}

#[test]
fn list_returns_compact_rows() {
    let core = FakeCore::start();
    let (code, doc) = run_cli(&core, &["device", "list"]);
    assert_eq!(code, 0, "{doc}");
    assert_eq!(data(&doc)["count"], 5);
    let row = &data(&doc)["devices"][0];
    for key in [
        "ID",
        "Name",
        "Model",
        "Interface",
        "CcuId",
        "HardwareParentId",
        "SerialNumber",
        "DeviceType",
    ] {
        assert!(row.get(key).is_some(), "row lacks {key}: {row}");
    }
    assert!(
        row.get("DeviceProperties").is_none(),
        "rows are compact: {row}"
    );
    assert!(core.writes().is_empty());
}

#[test]
fn list_filters() {
    let core = FakeCore::start();
    let list = |args: &[&str]| {
        let mut argv = vec!["device", "list"];
        argv.extend_from_slice(args);
        let (code, doc) = run_cli(&core, &argv);
        assert_eq!(code, 0, "{doc}");
        ids(&doc)
    };
    assert_eq!(list(&["--model", "Virtual*"]), vec![10, 11, 12]);
    assert_eq!(list(&["--model", "*variable*"]), vec![11, 21]);
    assert_eq!(list(&["--model", "virtualswitch"]), vec![10]);
    assert_eq!(list(&["--name", "PARSER"]), vec![20, 21]);
    assert_eq!(list(&["--parent", "20"]), vec![21]);
    assert_eq!(list(&["--ccu", "2"]), vec![20, 21]);
    assert_eq!(list(&["--ccu", "2", "--model", "*Module*"]), vec![20]);
    assert!(list(&["--name", "nothing like this"]).is_empty());
}

#[test]
fn get_by_id_and_name_with_lookup_errors() {
    let core = FakeCore::start();
    let (code, by_id) = run_cli(&core, &["device", "get", "10"]);
    assert_eq!(code, 0, "{by_id}");
    assert_eq!(data(&by_id)["device"]["Name"], "Test lamp");
    assert_eq!(data(&by_id)["device"]["DeviceProperties"]["IconId"], "1");
    let (_, by_name) = run_cli(&core, &["device", "get", "Test lamp"]);
    assert_eq!(data(&by_name)["device"]["ID"], 10);

    let (code, doc) = run_cli(&core, &["device", "get", "no such device"]);
    assert_eq!((code, error(&doc)["code"].clone()), (6, json!("not_found")));

    core.with_state(|s| {
        let mut twin = s.devices[0].clone();
        twin["ID"] = json!(50);
        s.devices.push(twin);
    });
    let (code, doc) = run_cli(&core, &["device", "get", "Test lamp"]);
    assert_eq!((code, error(&doc)["code"].clone()), (6, json!("ambiguous")));
    assert_eq!(error(&doc)["details"]["ids"], json!([10, 50]));
    assert_eq!(
        run_cli(&core, &["device", "get", "50"]).0,
        0,
        "the id stays usable"
    );
}

#[test]
fn get_redacts_password_properties_unless_asked() {
    let core = FakeCore::start();
    let (_, doc) = run_cli(&core, &["device", "get", "21"]);
    let props = &data(&doc)["device"]["DeviceProperties"];
    assert_eq!(props["Password"], "<redacted>");
    assert_eq!(props["ReadStateScriptPacketParser"], "7");
    let (_, doc) = run_cli(&core, &["--show-secrets", "device", "get", "21"]);
    assert_eq!(
        data(&doc)["device"]["DeviceProperties"]["Password"],
        "child-secret-value"
    );
}

#[test]
fn set_props_changes_and_verifies() {
    let core = FakeCore::start();
    let (code, doc) = run_cli(
        &core,
        &["device", "set-props", "Test lamp", "--prop", "IconId=5"],
    );
    assert_eq!(code, 0, "{doc}");
    assert_eq!(core.writes(), vec![UPDATE]);
    assert_eq!(prop(&core, 10, "IconId"), "5");
    assert_eq!(data(&doc)["verify"]["ok"], true);
    let diff = data(&doc)["diff"].as_array().unwrap();
    assert_eq!(diff.len(), 1);
    assert_eq!(
        (
            diff[0]["path"].clone(),
            diff[0]["before"].clone(),
            diff[0]["after"].clone()
        ),
        (json!("IconId"), json!("1"), json!("5"))
    );
}

#[test]
fn set_props_parses_values_and_merges_the_file() {
    let core = FakeCore::start();
    let file = std::env::temp_dir().join(format!("taphome-props-{}.json", std::process::id()));
    std::fs::write(&file, r#"{"IconId": "7", "Port": "81"}"#).unwrap();
    // 20 has Port. IconId is new there but --allow-new-key covers it. The flag wins on IconId.
    let (code, doc) = run_cli(
        &core,
        &[
            "device",
            "set-props",
            "20",
            "--props-file",
            file.to_str().unwrap(),
            "--prop",
            "IconId=a=b",
            "--prop",
            "IpAddress=",
            "--allow-new-key",
        ],
    );
    std::fs::remove_file(&file).unwrap();
    assert_eq!(code, 0, "{doc}");
    assert_eq!(
        prop(&core, 20, "IconId"),
        "a=b",
        "value split at the first ="
    );
    assert_eq!(prop(&core, 20, "Port"), "81");
    assert_eq!(
        prop(&core, 20, "IpAddress"),
        "",
        "an empty value is allowed"
    );
    assert_eq!(data(&doc)["diff"].as_array().unwrap().len(), 3);
}

#[test]
fn set_props_rejects_bad_input() {
    let core = FakeCore::start();
    for args in [
        vec!["device", "set-props", "10"],
        vec!["device", "set-props", "10", "--prop", "novalue"],
        vec!["device", "set-props", "10", "--prop", "=x"],
    ] {
        let (code, doc) = run_cli(&core, &args);
        assert_eq!(
            (code, error(&doc)["code"].clone()),
            (2, json!("invalid_input")),
            "{args:?}"
        );
    }
    assert!(
        core.requests().is_empty(),
        "input is checked before connecting"
    );
}

#[test]
fn set_props_dry_run_sends_nothing_and_shows_the_diff() {
    let core = FakeCore::start();
    let (code, doc) = run_cli(
        &core,
        &[
            "device",
            "set-props",
            "10",
            "--prop",
            "IconId=5",
            "--dry-run",
        ],
    );
    assert_eq!(code, 0, "{doc}");
    assert!(core.writes().is_empty());
    assert_eq!(prop(&core, 10, "IconId"), "1");
    let d = data(&doc);
    assert_eq!(d["dry_run"], true);
    assert_eq!(d["message"]["class"], UPDATE);
    assert_eq!(
        d["message"]["fields"],
        json!({"DeviceIds": [10], "DeviceProperties": {"IconId": "5"}})
    );
    assert_eq!(d["diff"][0]["op"], "change");
    assert_eq!(d["diff"][0]["after"], "5");
}

#[test]
fn set_props_without_a_difference_sends_nothing() {
    let core = FakeCore::start();
    let (code, doc) = run_cli(&core, &["device", "set-props", "10", "--prop", "IconId=1"]);
    assert_eq!(code, 0, "{doc}");
    assert_eq!(data(&doc), &json!({"changed": false}));
    assert!(core.writes().is_empty());
    // Same value for Name counts too, and a partly current delta drops only the current key.
    let (_, doc) = run_cli(&core, &["device", "rename", "10", "Test lamp"]);
    assert_eq!(data(&doc), &json!({"changed": false}));
    let (code, doc) = run_cli(
        &core,
        &[
            "device",
            "set-props",
            "11",
            "--prop",
            "IsReadOnly=False",
            "--prop",
            "Name=Renamed",
        ],
    );
    assert_eq!(code, 0, "{doc}");
    assert_eq!(data(&doc)["diff"].as_array().unwrap().len(), 1);
    assert!(doc["warnings"][0].as_str().unwrap().contains("IsReadOnly"));
    assert_eq!(core.writes(), vec![UPDATE]);
}

#[test]
fn set_props_typo_guard() {
    let core = FakeCore::start();
    let (code, doc) = run_cli(&core, &["device", "set-props", "10", "--prop", "IconID=5"]);
    assert_eq!(
        (code, error(&doc)["code"].clone()),
        (2, json!("invalid_input"))
    );
    assert_eq!(error(&doc)["details"]["close_keys"], json!(["IconId"]));
    assert!(error(&doc)["message"].as_str().unwrap().contains("IconId"));
    assert!(core.writes().is_empty());
    let (code, doc) = run_cli(
        &core,
        &[
            "device",
            "set-props",
            "10",
            "--prop",
            "IconID=5",
            "--allow-new-key",
        ],
    );
    assert_eq!(code, 0, "{doc}");
    assert_eq!(prop(&core, 10, "IconID"), "5");
    assert_eq!(data(&doc)["diff"][0]["before"], Value::Null);
}

#[test]
fn set_props_masks_secret_values_in_the_diff() {
    let core = FakeCore::start();
    let (code, doc) = run_cli(
        &core,
        &["device", "set-props", "21", "--prop", "Password=new-secret"],
    );
    assert_eq!(code, 0, "{doc}");
    assert_eq!(prop(&core, 21, "Password"), "new-secret");
    let text = doc.to_string();
    assert!(
        !text.contains("new-secret") && !text.contains("child-secret-value"),
        "{text}"
    );
    let (_, doc) = run_cli(
        &core,
        &[
            "--show-secrets",
            "device",
            "set-props",
            "21",
            "--prop",
            "Password=newer",
            "--dry-run",
        ],
    );
    assert_eq!(data(&doc)["diff"][0]["before"], "new-secret");
}

#[test]
fn rename_changes_and_verifies() {
    let core = FakeCore::start();
    let (code, doc) = run_cli(&core, &["device", "rename", "12", "Renamed sensor"]);
    assert_eq!(code, 0, "{doc}");
    assert_eq!(name_of(&core, 12), "Renamed sensor");
    assert_eq!(data(&doc)["verify"]["ok"], true);
    assert_eq!(data(&doc)["diff"][0]["path"], "Name");
    assert_eq!(data(&doc)["diff"][0]["before"], "Test thermometer");

    let (code, doc) = run_cli(&core, &["device", "rename", "Nope", "x"]);
    assert_eq!((code, error(&doc)["code"].clone()), (6, json!("not_found")));
    let (_, doc) = run_cli(&core, &["device", "rename", "10", "Dry", "--dry-run"]);
    assert_eq!(data(&doc)["dry_run"], true);
    assert_eq!(name_of(&core, 10), "Test lamp");
    assert_eq!(core.writes().len(), 1, "only the real rename was sent");
}

#[test]
fn rename_by_ambiguous_name_is_refused() {
    let core = FakeCore::start();
    core.with_state(|s| {
        let mut twin = s.devices[0].clone();
        twin["ID"] = json!(50);
        s.devices.push(twin);
    });
    let (code, doc) = run_cli(&core, &["device", "rename", "Test lamp", "x"]);
    assert_eq!((code, error(&doc)["code"].clone()), (6, json!("ambiguous")));
    assert!(core.writes().is_empty());
}

#[test]
fn core_error_surfaces_with_exit_5() {
    let core = FakeCore::start();
    core.fail_next(6, "no such device");
    let (code, doc) = run_cli(&core, &["device", "rename", "10", "x"]);
    assert_eq!(code, 5, "{doc}");
    assert_eq!(error(&doc)["code"], "does_not_exist");
    assert_eq!(name_of(&core, 10), "Test lamp");
}

#[test]
fn dropped_write_fails_verification_with_exit_8() {
    let core = FakeCore::start();
    core.drop_next_write();
    let (code, doc) = run_cli(&core, &["device", "rename", "10", "x"]);
    assert_eq!(
        (code, error(&doc)["code"].clone()),
        (8, json!("verify_failed"))
    );
    assert_eq!(error(&doc)["details"]["verify"]["ok"], false);
    assert_eq!(name_of(&core, 10), "Test lamp");
}

#[test]
fn delete_a_leaf_device_warns_about_rules() {
    let core = FakeCore::start();
    let before = device_count(&core);
    let (code, doc) = run_cli(&core, &["device", "delete", "Test variable"]);
    assert_eq!(code, 0, "{doc}");
    assert_eq!(device_count(&core), before - 1);
    assert_eq!(data(&doc)["verify"]["ok"], true);
    let diff = &data(&doc)["diff"][0];
    assert_eq!(diff["op"], "remove");
    assert_eq!(diff["before"]["model"], "VirtualVariableGeneral");
    assert_eq!(diff["before"]["serial"], "TEST-11");
    let warning = doc["warnings"][0].as_str().unwrap();
    assert!(
        warning.contains("Test rule A") && warning.contains("Test rule B"),
        "{warning}"
    );
    assert!(!warning.contains("Old rule"), "tombstones are ignored");
    assert_eq!(core.writes(), vec!["MessageDeleteDeviceRequest"]);
}

#[test]
fn delete_without_rule_references_has_no_warning() {
    let core = FakeCore::start();
    let (code, doc) = run_cli(&core, &["device", "delete", "12"]);
    assert_eq!(code, 0, "{doc}");
    assert!(doc["warnings"].as_array().unwrap().is_empty());
}

#[test]
fn delete_dry_run_sends_nothing() {
    let core = FakeCore::start();
    let (code, doc) = run_cli(&core, &["device", "delete", "12", "--dry-run"]);
    assert_eq!(code, 0, "{doc}");
    assert!(core.writes().is_empty());
    assert_eq!(device_count(&core), 5);
    assert_eq!(
        data(&doc)["message"]["fields"],
        json!({"DeviceId": 12, "ForceDelete": false})
    );
    assert_eq!(data(&doc)["diff"][0]["op"], "remove");
}

#[test]
fn delete_refuses_a_module_with_children() {
    let core = FakeCore::start();
    let (code, doc) = run_cli(&core, &["device", "delete", "Parser module"]);
    assert_eq!(
        (code, error(&doc)["code"].clone()),
        (2, json!("invalid_input"))
    );
    assert_eq!(error(&doc)["details"]["children"], json!([21]));
    assert!(core.writes().is_empty());
    // The child first, then the module.
    assert_eq!(run_cli(&core, &["device", "delete", "21"]).0, 0);
    let (code, doc) = run_cli(&core, &["device", "delete", "20"]);
    assert_eq!(code, 0, "{doc}");
    assert_eq!(device_count(&core), 3);
}

#[test]
fn delete_force_needs_allow_unverified() {
    let core = FakeCore::start();
    let (code, doc) = run_cli(&core, &["device", "delete", "20", "--force"]);
    assert_eq!(
        (code, error(&doc)["code"].clone()),
        (7, json!("unverified_operation"))
    );
    assert!(core.writes().is_empty());
    let (_, dry) = run_cli(&core, &["device", "delete", "20", "--force", "--dry-run"]);
    assert_eq!(data(&dry)["verified"], false);
    assert_eq!(data(&dry)["message"]["fields"]["ForceDelete"], true);

    let (code, doc) = run_cli(
        &core,
        &["device", "delete", "20", "--force", "--allow-unverified"],
    );
    assert_eq!(code, 0, "{doc}");
    assert_eq!(
        device_count(&core),
        3,
        "the fake Core takes the child along"
    );
}

#[test]
fn add_virtual_variable_is_the_verified_variant() {
    let core = FakeCore::start();
    let before = device_count(&core);
    let (code, doc) = run_cli(
        &core,
        &[
            "device",
            "add-virtual",
            "--model",
            "VirtualVariableGeneral",
            "--name",
            "TEST var",
        ],
    );
    assert_eq!(code, 0, "{doc}");
    assert_eq!(device_count(&core), before + 1);
    let d = data(&doc);
    assert_eq!(d["verify"]["ok"], true);
    assert_eq!(d["device"]["id"], 100);
    assert!(d["device"]["serial"].as_str().unwrap().starts_with("VIRT-"));
    assert!(d["device"]["property_keys"].is_array());
    assert_eq!(d["diff"][0]["op"], "add");
    assert_eq!(d["operation"], "addVirtualVariable");
    assert_eq!(core.writes(), vec!["MessageAddDeviceRequest"]);
    let (code, doc) = run_cli(&core, &["device", "get", "TEST var"]);
    assert_eq!(code, 0, "{doc}");
    assert_eq!(data(&doc)["device"]["Interface"], "Variable");
}

#[test]
fn add_virtual_sends_the_documented_fields() {
    let core = FakeCore::start();
    let (_, doc) = run_cli(
        &core,
        &[
            "device",
            "add-virtual",
            "--model",
            "VirtualVariableGeneral",
            "--name",
            "TEST var",
            "--dry-run",
        ],
    );
    let d = data(&doc);
    assert_eq!(d["verified"], true);
    assert_eq!(
        d["message"]["fields"],
        json!({"DeviceName": "TEST var", "Icon": 0, "Interface": "Variable",
               "Model": "VirtualVariableGeneral", "DeviceTypeId": 0})
    );
    assert!(core.writes().is_empty());
    assert_eq!(device_count(&core), 5);
}

#[test]
fn add_virtual_other_variants_need_allow_unverified() {
    let core = FakeCore::start();
    let (code, doc) = run_cli(
        &core,
        &[
            "device",
            "add-virtual",
            "--model",
            "VirtualSwitch",
            "--name",
            "TEST switch",
        ],
    );
    assert_eq!(
        (code, error(&doc)["code"].clone()),
        (7, json!("unverified_operation"))
    );
    let (code, _) = run_cli(
        &core,
        &[
            "device",
            "add-virtual",
            "--model",
            "VirtualVariableGeneral",
            "--name",
            "TEST v",
            "--prop",
            "IsReadOnly=True",
        ],
    );
    assert_eq!(code, 7, "a variable with properties is unverified too");
    assert!(core.writes().is_empty());

    let (code, doc) = run_cli(
        &core,
        &[
            "device",
            "add-virtual",
            "--model",
            "VirtualSwitch",
            "--name",
            "TEST switch",
            "--allow-unverified",
        ],
    );
    assert_eq!(code, 0, "{doc}");
    let added = core.with_state(|s| s.devices.last().unwrap().clone());
    assert_eq!(
        added["Interface"], "Switch",
        "default interface for the model"
    );
    assert_eq!(data(&doc)["verify"]["ok"], true);
}

#[test]
fn add_virtual_with_properties_sends_them() {
    let core = FakeCore::start();
    let (code, doc) = run_cli(
        &core,
        &[
            "device",
            "add-virtual",
            "--model",
            "VirtualVariableGeneral",
            "--name",
            "TEST v",
            "--prop",
            "IsReadOnly=True",
            "--allow-unverified",
        ],
    );
    assert_eq!(code, 0, "{doc}");
    assert_eq!(prop(&core, 100, "IsReadOnly"), "True");
    assert_eq!(data(&doc)["device"]["property_keys"], json!(["IsReadOnly"]));
}

#[test]
fn add_virtual_unknown_model_needs_an_interface() {
    let core = FakeCore::start();
    let (code, doc) = run_cli(
        &core,
        &[
            "device",
            "add-virtual",
            "--model",
            "VirtualAlarm",
            "--name",
            "TEST alarm",
            "--allow-unverified",
        ],
    );
    assert_eq!(
        (code, error(&doc)["code"].clone()),
        (2, json!("invalid_input"))
    );
    assert!(core.requests().is_empty());
    let (code, doc) = run_cli(
        &core,
        &[
            "device",
            "add-virtual",
            "--model",
            "VirtualAlarm",
            "--interface",
            "Alarm",
            "--name",
            "TEST alarm",
            "--allow-unverified",
        ],
    );
    assert_eq!(code, 0, "{doc}");
    assert_eq!(
        core.with_state(|s| s.devices.last().unwrap()["Interface"].clone()),
        "Alarm"
    );
}

#[test]
fn add_virtual_warns_about_a_duplicate_name() {
    let core = FakeCore::start();
    let (code, doc) = run_cli(
        &core,
        &[
            "device",
            "add-virtual",
            "--model",
            "VirtualVariableGeneral",
            "--name",
            "Test variable",
        ],
    );
    assert_eq!(code, 0, "{doc}");
    assert!(doc["warnings"][0].as_str().unwrap().contains("ambiguous"));
}

#[test]
fn add_virtual_failures() {
    let core = FakeCore::start();
    core.fail_next(8, "no");
    let (code, doc) = run_cli(
        &core,
        &[
            "device",
            "add-virtual",
            "--model",
            "VirtualVariableGeneral",
            "--name",
            "TEST v",
        ],
    );
    assert_eq!(code, 5, "{doc}");
    assert_eq!(device_count(&core), 5);

    core.drop_next_write();
    let (code, doc) = run_cli(
        &core,
        &[
            "device",
            "add-virtual",
            "--model",
            "VirtualVariableGeneral",
            "--name",
            "TEST v",
        ],
    );
    // The Core answered Ok without a typed response: the CLI refuses the answer.
    assert_ne!(code, 0, "{doc}");
    assert_eq!(device_count(&core), 5);
}
