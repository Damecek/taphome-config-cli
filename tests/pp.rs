mod common;
use common::*;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

const EMPTY: &str = "00000000-0000-0000-0000-000000000000";
const SCRIPT_KEY: &str = "ReadStateScriptPacketParser";

fn fixture(name: &str) -> String {
    format!("{}/tests/fixtures/pp/{name}", env!("CARGO_MANIFEST_DIR"))
}

fn temp_file(name: &str, content: &str) -> String {
    let path = std::env::temp_dir().join(format!("taphome-pp-{}-{name}", std::process::id()));
    std::fs::write(&path, content).unwrap();
    path.to_str().unwrap().to_string()
}

/// What `lamp.xml` converts to with `--param ApiKey=k1` and no other parameter.
fn lamp_config() -> Value {
    json!({
        "ProtocolVersion": 66, "Name": "Demo gateway", "Id": "3f2b8c1e-5d4a-4b7e-9a10-0c6d2e8f1a77", "Revision": "1.2",
        "Modules": [{
            "Name": "Demo gateway", "Model": "PacketParserModuleHttp", "Id": -1, "HardwareParentId": 0,
            "CategoryId": EMPTY, "ZoneId": EMPTY,
            "DeviceProperties": {
                "IpAddress": "192.0.2.10", "Port": "8080", "StayConnected": "False", "InternalPollInterval": "10",
                "DeviceVariables": "[{\"DeviceId\":-2,\"ValueLogType\":62,\"Abbreviation\":\"Lv\",\"ValueLogTypeFlavourId\":1}]"},
            "Devices": [
                {"Name": "Demo lamp", "Model": "PacketParserSwitch", "Id": -2, "HardwareParentId": 0,
                 "CategoryId": EMPTY, "ZoneId": EMPTY,
                 "DeviceProperties": {
                     "ReadSwitchStateScriptPacketParser":
                         "VAR r := SENDHTTPREQUEST(\"/status\", \"GET\", \"\", \"X-Key: k1\");\nRETURN(PARSEJSON(r.CONTENT, \"$[0].on\"));",
                     "WriteSwitchStateScriptPacketParser":
                         "SENDHTTPREQUEST(\"/relay?on=\" + TOSTRING(SwitchState, \"0\"));",
                     "InternalPollInterval": "5"}},
                {"Name": "Demo level", "Model": "PacketParserVariable", "Id": -3, "HardwareParentId": 0,
                 "CategoryId": EMPTY, "ZoneId": EMPTY,
                 "DeviceProperties": {"ReadStateScriptPacketParser": "7", "IsReadOnly": "True"}}]}]
    })
}

/// A fake `MessageHttpApiConfigRequest` that also creates top-level devices, can drop property keys
/// and can report removed devices. The default fake handler only knows modules.
fn config_handler(
    drop_keys: &'static [&'static str],
    removed: bool,
) -> impl Fn(&mut CoreState, &Value) -> Reply + Send + Sync + 'static {
    move |state, req| {
        let ccu = req["CcuId"].as_i64().unwrap();
        let config: Value = serde_json::from_str(req["ConfigJson"].as_str().unwrap()).unwrap();
        let mut next = state
            .devices
            .iter()
            .filter_map(|d| d["ID"].as_i64())
            .max()
            .unwrap()
            + 1;
        let mut added = vec![];
        let mut push = |state: &mut CoreState, d: &Value, parent: i64| -> i64 {
            let id = next;
            next += 1;
            let mut props = d["DeviceProperties"]
                .as_object()
                .cloned()
                .unwrap_or_default();
            for k in drop_keys {
                props.remove(*k);
            }
            let device = json!({"ID": id, "Name": d["Name"], "Model": d["Model"], "Interface": "Variable",
                "CcuId": ccu, "HardwareParentId": parent, "SerialNumber": format!("PP-{id}"),
                "DeviceProperties": props});
            state.devices.push(device.clone());
            added.push(device);
            id
        };
        for m in config["Modules"].as_array().cloned().unwrap_or_default() {
            let module_id = push(state, &m, 0);
            for c in m["Devices"].as_array().cloned().unwrap_or_default() {
                push(state, &c, module_id);
            }
        }
        for d in config["Devices"].as_array().cloned().unwrap_or_default() {
            let parent = d["HardwareParentId"].as_i64().unwrap();
            push(state, &d, parent);
        }
        let mut fields = json!({"CcuId": ccu, "AddedDevices": added});
        if removed {
            fields["RemovedDevices"] = json!([state.devices[0].clone()]);
        }
        Reply::new("MessageHttpApiConfigResponse", fields)
    }
}

fn inspect(core: &FakeCore, args: &[&str]) -> (i32, Value) {
    let mut full = vec!["pp", "inspect"];
    full.extend_from_slice(args);
    let out = run_cli_with(core, &full, &[("TAPHOME_HOST", Some("127.0.0.1:1"))]);
    assert!(core.requests().is_empty(), "inspect opens no connection");
    out
}

// ---------------------------------------------------------------- inspect

#[test]
fn xml_template_converts_to_the_confirmed_config_shape() {
    let core = FakeCore::start();
    let (code, doc) = inspect(&core, &[&fixture("lamp.xml"), "--param", "ApiKey=k1"]);
    assert_eq!(code, 0, "{doc}");
    let d = data(&doc);
    assert_eq!(d["config"], lamp_config());
    assert_eq!(d["source"], "xml");
    assert_eq!(d["kind"], "packet_parser");
    assert_eq!(d["unresolved_tokens"], json!([]));
    assert_eq!(d["variant"]["verified"], true);
    let module = &d["modules"][0];
    assert_eq!(module["model"], "PacketParserModuleHttp");
    assert_eq!(
        module["property_keys"],
        json!([
            "IpAddress",
            "Port",
            "StayConnected",
            "InternalPollInterval",
            "DeviceVariables"
        ])
    );
    assert_eq!(module["script_keys"], json!(["DeviceVariables"]));
    let lamp = &module["devices"][0];
    assert_eq!(lamp["name"], "Demo lamp");
    assert_eq!(
        lamp["script_keys"],
        json!([
            "ReadSwitchStateScriptPacketParser",
            "WriteSwitchStateScriptPacketParser"
        ])
    );
}

#[test]
fn parameter_defaults_overrides_and_unresolved_tokens() {
    let core = FakeCore::start();
    let (_, doc) = inspect(&core, &[&fixture("lamp.xml")]);
    let d = data(&doc);
    let params = d["parameters"].as_array().unwrap();
    let by_id = |id: &str| params.iter().find(|p| p["id"] == id).unwrap().clone();
    assert_eq!(by_id("Port")["default"], "8080");
    assert_eq!(by_id("Port")["supplied"], false);
    assert_eq!(by_id("Port")["value"], "8080");
    assert_eq!(by_id("ApiKey")["value"], Value::Null);
    assert_eq!(by_id("DeviceNamePrefix")["value"], "");
    assert_eq!(d["unresolved_tokens"], json!(["ApiKey"]));
    // The token stays in the text and the JSON path `$[0]` is not a token.
    assert!(d["config"]["Modules"][0]["Devices"][0]["DeviceProperties"]
        ["ReadSwitchStateScriptPacketParser"]
        .as_str()
        .unwrap()
        .contains("X-Key: $[ApiKey]"));

    let (_, doc) = inspect(
        &core,
        &[
            &fixture("lamp.xml"),
            "--param",
            "ApiKey=k2",
            "--param",
            "Port=81",
            "--param",
            "IpAddress=192.0.2.99",
            "--param",
            "DeviceNamePrefix=zz-",
        ],
    );
    let d = data(&doc);
    let module = &d["config"]["Modules"][0];
    assert_eq!(module["Name"], "zz-Demo gateway");
    assert_eq!(module["Devices"][0]["Name"], "zz-Demo lamp");
    assert_eq!(module["DeviceProperties"]["IpAddress"], "192.0.2.99");
    assert_eq!(module["DeviceProperties"]["Port"], "81");
    assert_eq!(d["unresolved_tokens"], json!([]));
    let by_id = |id: &str| {
        d["parameters"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["id"] == id)
            .unwrap()
            .clone()
    };
    assert_eq!(
        (
            by_id("Port")["supplied"].clone(),
            by_id("Port")["value"].clone()
        ),
        (json!(true), json!("81"))
    );
}

#[test]
fn unknown_and_malformed_params_are_invalid_input() {
    let core = FakeCore::start();
    let (code, doc) = inspect(&core, &[&fixture("lamp.xml"), "--param", "Nope=1"]);
    assert_eq!(
        (code, error(&doc)["code"].clone()),
        (2, json!("invalid_input"))
    );
    assert_eq!(error(&doc)["details"]["unknown"], json!(["Nope"]));
    let (code, doc) = inspect(&core, &[&fixture("lamp.xml"), "--param", "Port"]);
    assert_eq!(
        (code, error(&doc)["code"].clone()),
        (2, json!("invalid_input"))
    );
    let (code, doc) = inspect(&core, &["/nonexistent/lamp.xml"]);
    assert_eq!(
        (code, error(&doc)["code"].clone()),
        (2, json!("invalid_input"))
    );
    let other = temp_file("lamp.txt", "x");
    let (code, _) = inspect(&core, &[&other]);
    assert_eq!(code, 2);
    let broken = temp_file("broken.xml", "<Templates format=\"2\"><Template>");
    let (code, doc) = inspect(&core, &[&broken]);
    assert_eq!(
        (code, error(&doc)["code"].clone()),
        (2, json!("invalid_input"))
    );
    let old = temp_file("v9.xml", "<Templates format=\"9\"><Template/></Templates>");
    let (code, doc) = inspect(&core, &[&old]);
    assert_eq!(code, 2);
    assert!(error(&doc)["message"].as_str().unwrap().contains("format"));
}

#[test]
fn bare_json_config_is_stringified_and_gets_placeholder_ids() {
    let core = FakeCore::start();
    let (code, doc) = inspect(
        &core,
        &[
            &fixture("config-bare.json"),
            "--param",
            "Host=192.0.2.5",
            "--param",
            "Site=North",
        ],
    );
    assert_eq!(code, 0, "{doc}");
    let d = data(&doc);
    assert_eq!(d["source"], "json");
    assert_eq!(
        d["config"],
        json!({"ProtocolVersion": 66, "Modules": [{
            "Name": "Demo meter North", "Model": "PacketParserModuleHttp", "Id": -1, "HardwareParentId": 0,
            "CategoryId": EMPTY, "ZoneId": EMPTY,
            "DeviceProperties": {"IpAddress": "192.0.2.5", "Port": "80", "UseHttps": "False",
                                 "InternalPollInterval": "3600000"},
            "Devices": [{"Name": "Demo meter value", "Model": "PacketParserVariable", "Id": -2,
                         "HardwareParentId": 0, "CategoryId": EMPTY, "ZoneId": EMPTY,
                         "DeviceProperties": {"ReadStateScriptPacketParser": "7", "IsReadOnly": "True",
                                              "Scale": "1.5"}}]}]})
    );
    // Tokens in a JSON file are known only by use: Host and Site, nothing else.
    let (_, doc) = inspect(&core, &[&fixture("config-bare.json")]);
    assert_eq!(data(&doc)["unresolved_tokens"], json!(["Host", "Site"]));
    let (code, _) = inspect(&core, &[&fixture("config-bare.json"), "--param", "Port=1"]);
    assert_eq!(code, 2);
}

#[test]
fn wrapped_json_keeps_its_ids_and_ccu() {
    let core = FakeCore::start();
    let (code, doc) = inspect(&core, &[&fixture("config-wrapped.json")]);
    assert_eq!(code, 0, "{doc}");
    let d = data(&doc);
    assert_eq!(d["ccu_id"], 2);
    // The explicit -5 stays and the new placeholder skips used ids.
    assert_eq!(d["config"]["Modules"][0]["Id"], -5);
    assert_eq!(d["config"]["Modules"][0]["Devices"][0]["Id"], -1);
}

#[test]
fn invalid_shapes_are_rejected_before_any_connection() {
    let core = FakeCore::start();
    let cases = [
        (r#"{"Modules": [{"Name": "m", "Model": 5}]}"#, "Model"),
        (
            r#"{"Modules": [{"Name": "m", "Model": "X", "DeviceProperties": {"A": [1]}}]}"#,
            "property A",
        ),
        (r#"{"Modules": []}"#, "no Modules"),
        (
            r#"{"Devices": [{"Name": "d", "Model": "PacketParserVariable"}]}"#,
            "HardwareParentId",
        ),
        (
            r#"{"Modules": [{"Name": "m", "Model": "X", "ZoneId": "nope"}]}"#,
            "Guid",
        ),
        (
            r#"{"Modules": [{"Name": "m", "Model": "X", "Id": 7}]}"#,
            "placeholders and must be 0 or negative",
        ),
        (
            r#"{"Modules": [{"Name": "m", "Model": "X", "Devices": [{"Name": "d", "Model": "Y", "Id": 3}]}]}"#,
            "placeholders and must be 0 or negative",
        ),
    ];
    for (i, (text, needle)) in cases.iter().enumerate() {
        let file = temp_file(&format!("bad{i}.json"), text);
        let (code, doc) = inspect(&core, &[&file]);
        assert_eq!(code, 2, "{text}: {doc}");
        assert!(
            error(&doc)["message"].as_str().unwrap().contains(needle),
            "{text}: {doc}"
        );
    }
}

#[test]
fn inspect_flags_modbus() {
    let core = FakeCore::start();
    let (code, doc) = inspect(&core, &[&fixture("config-modbus.json")]);
    assert_eq!(code, 0);
    assert_eq!(data(&doc)["kind"], "modbus");
    assert!(doc["warnings"][0].as_str().unwrap().contains("Modbus"));
}

// ---------------------------------------------------------------- import

fn import_args<'a>(file: &'a str, extra: &[&'a str]) -> Vec<&'a str> {
    let mut args = vec!["pp", "import", file];
    args.extend_from_slice(extra);
    args
}

fn lamp(extra: &[&str]) -> Vec<String> {
    let mut args = vec!["--param".to_string(), "ApiKey=k1".to_string()];
    args.extend(extra.iter().map(|s| s.to_string()));
    args
}

fn run_import(core: &FakeCore, file: &str, extra: &[String]) -> (i32, Value) {
    let extra: Vec<&str> = extra.iter().map(String::as_str).collect();
    run_cli(core, &import_args(file, &extra))
}

#[test]
fn dry_run_sends_nothing_and_shows_the_diff() {
    let core = FakeCore::start();
    let file = fixture("lamp.xml");
    let (code, doc) = run_import(&core, &file, &lamp(&["--dry-run"]));
    assert_eq!(code, 0, "{doc}");
    let d = data(&doc);
    assert_eq!(d["dry_run"], true);
    assert_eq!(d["operation"], "addPacketParserModule");
    assert_eq!(d["verified"], true);
    assert_eq!(d["message"]["class"], "MessageHttpApiConfigRequest");
    assert_eq!(d["message"]["fields"]["CcuId"], 2);
    assert_eq!(
        d["message"]["fields"]["ConfigJson"].as_str().unwrap(),
        lamp_config().to_string(),
        "one JSON string, key order as in the confirmed payload"
    );
    let diff = d["diff"].as_array().unwrap();
    assert_eq!(diff.len(), 3);
    assert!(diff
        .iter()
        .all(|e| e["op"] == "add" && e["object"] == "device"));
    assert_eq!(diff[0]["path"], "Demo gateway");
    assert_eq!(diff[0]["after"]["parent"], Value::Null);
    assert_eq!(diff[1]["after"]["parent"], -1);
    assert_eq!(diff[1]["after"]["model"], "PacketParserSwitch");
    let read = &diff[1]["after"]["scripts"]["ReadSwitchStateScriptPacketParser"];
    assert!(read["length"].as_u64().unwrap() > 40);
    assert!(read["first_line"]
        .as_str()
        .unwrap()
        .starts_with("VAR r := SENDHTTPREQUEST"));
    assert!(
        !diff[1].to_string().contains("RETURN("),
        "scripts are shortened to the first line"
    );
    assert!(core.writes().is_empty());
    assert!(!core
        .requests()
        .iter()
        .any(|c| c == "MessageHttpApiConfigRequest"));
}

#[test]
fn import_creates_the_module_and_verifies() {
    let core = FakeCore::start();
    let before = core.with_state(|s| s.devices.len());
    let (code, doc) = run_import(&core, &fixture("lamp.xml"), &lamp(&[]));
    assert_eq!(code, 0, "{doc}");
    let d = data(&doc);
    assert_eq!(d["verify"]["ok"], true, "{d}");
    assert_eq!(d["operation"], "addPacketParserModule");
    assert_eq!(d["ccu_id"], 2);
    assert_eq!(d["state"]["devices"], before + 3);
    let module = &d["created"]["modules"][0];
    assert_eq!(module["name"], "Demo gateway");
    assert_eq!(module["model"], "PacketParserModuleHttp");
    assert_eq!(module["placeholder"], -1);
    let devices = d["created"]["devices"].as_array().unwrap();
    assert_eq!(devices.len(), 2);
    assert!(devices.iter().all(|c| c["parent_id"] == module["id"]));
    assert_eq!(devices[0]["name"], "Demo lamp");
    assert_eq!(d["property_differences"], json!([]));
    assert!(d.get("removed").is_none());
    assert_eq!(doc["warnings"], json!([]));
    assert_eq!(core.writes(), vec!["MessageHttpApiConfigRequest"]);
    let stored = core.with_state(|s| {
        s.devices
            .iter()
            .filter(|x| x["Name"] == "Demo lamp")
            .count()
    });
    assert_eq!(stored, 1);
}

#[test]
fn import_requires_values_for_tokens_without_default() {
    let core = FakeCore::start();
    let (code, doc) = run_cli(&core, &["pp", "import", &fixture("lamp.xml")]);
    assert_eq!(
        (code, error(&doc)["code"].clone()),
        (2, json!("invalid_input"))
    );
    assert_eq!(error(&doc)["details"]["missing"], json!(["ApiKey"]));
    let (code, doc) = run_cli(
        &core,
        &[
            "pp",
            "import",
            &fixture("lamp.xml"),
            "--param",
            "ApiKey=k",
            "--param",
            "Nope=1",
        ],
    );
    assert_eq!(
        (code, error(&doc)["details"]["unknown"].clone()),
        (2, json!(["Nope"]))
    );
    assert!(
        core.requests().is_empty(),
        "nothing was sent, not even a login"
    );
}

#[test]
fn json_inputs_import_in_both_forms() {
    let core = FakeCore::start();
    let (code, doc) = run_cli(
        &core,
        &[
            "pp",
            "import",
            &fixture("config-bare.json"),
            "--param",
            "Host=192.0.2.5",
            "--param",
            "Site=North",
        ],
    );
    assert_eq!(code, 0, "{doc}");
    assert_eq!(
        data(&doc)["created"]["modules"][0]["name"],
        "Demo meter North"
    );
    let stored: Value = core.with_state(|s| {
        s.devices
            .iter()
            .find(|d| d["Name"] == "Demo meter value")
            .unwrap()["DeviceProperties"]
            .clone()
    });
    assert_eq!(stored["IsReadOnly"], "True");
    assert_eq!(stored["Scale"], "1.5");

    let (code, doc) = run_cli(&core, &["pp", "import", &fixture("config-wrapped.json")]);
    assert_eq!(code, 0, "{doc}");
    assert_eq!(data(&doc)["ccu_id"], 2);
    assert_eq!(data(&doc)["created"]["modules"][0]["name"], "Demo wrapped");
}

#[test]
fn ccu_selection() {
    let core = FakeCore::start();
    let file = fixture("lamp.xml");
    // Several PacketParserCCU interfaces: ambiguous until --ccu says which.
    core.with_state(|s| {
        s.ccus.push(
            json!({"CcuId": 3, "Name": "Second parser", "Model": "PacketParserCCU",
                           "SerialNumber": "PP-2", "CcuProperties": {}}),
        )
    });
    let (code, doc) = run_import(&core, &file, &lamp(&["--dry-run"]));
    assert_eq!((code, error(&doc)["code"].clone()), (6, json!("ambiguous")));
    assert_eq!(error(&doc)["details"]["ccu_ids"], json!([2, 3]));
    let (code, doc) = run_import(&core, &file, &lamp(&["--dry-run", "--ccu", "3"]));
    assert_eq!(code, 0, "{doc}");
    assert_eq!(data(&doc)["message"]["fields"]["CcuId"], 3);
    let (code, doc) = run_import(
        &core,
        &file,
        &lamp(&["--dry-run", "--ccu", "Second parser"]),
    );
    assert_eq!(
        (code, data(&doc)["message"]["fields"]["CcuId"].clone()),
        (0, json!(3))
    );
    let (code, doc) = run_import(&core, &file, &lamp(&["--dry-run", "--ccu", "99"]));
    assert_eq!((code, error(&doc)["code"].clone()), (6, json!("not_found")));
    // A different interface model works but warns.
    let (code, doc) = run_import(&core, &file, &lamp(&["--dry-run", "--ccu", "1"]));
    assert_eq!(code, 0);
    assert!(doc["warnings"][0].as_str().unwrap().contains("VirtualCCU"));

    // None at all: say how to create one.
    core.with_state(|s| s.ccus.retain(|c| c["Model"] != "PacketParserCCU"));
    let (code, doc) = run_import(&core, &file, &lamp(&["--dry-run"]));
    assert_eq!((code, error(&doc)["code"].clone()), (6, json!("not_found")));
    assert!(error(&doc)["message"]
        .as_str()
        .unwrap()
        .contains("MessageAddCcuRequest"));
    assert!(core.writes().is_empty());
}

#[test]
fn refuses_an_existing_device_name_unless_allowed() {
    let core = FakeCore::start();
    let file = temp_file(
        "dup.json",
        r#"{"Modules": [{"Name": "Parser module", "Model": "PacketParserModuleHttp",
            "DeviceProperties": {"IpAddress": "192.0.2.7"}}]}"#,
    );
    let (code, doc) = run_cli(&core, &["pp", "import", &file, "--dry-run"]);
    assert_eq!(code, 7, "{doc}");
    assert_eq!(error(&doc)["code"], "already_exist");
    let (code, doc) = run_cli(
        &core,
        &["pp", "import", &file, "--dry-run", "--allow-duplicate-name"],
    );
    assert_eq!(code, 0, "{doc}");
    let warning = doc["warnings"][0].as_str().unwrap();
    assert!(
        warning.contains("Parser module") && warning.contains("20"),
        "{warning}"
    );
}

#[test]
fn unverified_variants_need_allow_unverified() {
    let core = FakeCore::start();
    let tcp = fixture("config-tcp.json");
    let (code, doc) = run_cli(&core, &["pp", "import", &tcp]);
    assert_eq!(
        (code, error(&doc)["code"].clone()),
        (7, json!("unverified_operation"))
    );
    assert!(core.writes().is_empty());
    let (code, doc) = run_cli(&core, &["pp", "import", &tcp, "--dry-run"]);
    assert_eq!(code, 0);
    assert_eq!(data(&doc)["verified"], false);
    assert!(data(&doc)["variant_notes"][0]
        .as_str()
        .unwrap()
        .contains("PacketParserModuleTcp"));
    let (code, doc) = run_cli(&core, &["pp", "import", &tcp, "--allow-unverified"]);
    assert_eq!(code, 0, "{doc}");
    assert_eq!(
        data(&doc)["created"]["modules"][0]["model"],
        "PacketParserModuleTcp"
    );

    // Devices only, attached to the existing module 20.
    let only = fixture("config-devices-only.json");
    let (code, doc) = run_cli(&core, &["pp", "import", &only]);
    assert_eq!(
        (code, error(&doc)["code"].clone()),
        (7, json!("unverified_operation"))
    );
    core.on("MessageHttpApiConfigRequest", config_handler(&[], false));
    let (code, doc) = run_cli(&core, &["pp", "import", &only, "--allow-unverified"]);
    assert_eq!(code, 0, "{doc}");
    let d = data(&doc);
    assert_eq!(
        d["operation"],
        "addTemplateDevicesToExistingPacketParserModule"
    );
    assert_eq!(d["created"]["modules"], json!([]));
    assert_eq!(d["created"]["devices"][0]["parent_id"], 20);
    assert_eq!(d["verify"]["ok"], true);

    // The parent must exist.
    let missing = temp_file(
        "orphan.json",
        r#"{"Devices": [{"Name": "x", "Model": "PacketParserVariable", "HardwareParentId": 999}]}"#,
    );
    let (code, doc) = run_cli(&core, &["pp", "import", &missing, "--allow-unverified"]);
    assert_eq!((code, error(&doc)["code"].clone()), (6, json!("not_found")));
}

#[test]
fn modbus_is_refused() {
    let core = FakeCore::start();
    let (code, doc) = run_cli(
        &core,
        &[
            "pp",
            "import",
            &fixture("config-modbus.json"),
            "--allow-unverified",
        ],
    );
    assert_eq!(
        (code, error(&doc)["code"].clone()),
        (2, json!("invalid_input"))
    );
    assert!(error(&doc)["message"].as_str().unwrap().contains("Modbus"));
    let by_model = temp_file(
        "modbus-model.json",
        r#"{"Modules": [{"Name": "m", "Model": "ModbusModule"}]}"#,
    );
    let (code, _) = run_cli(&core, &["pp", "import", &by_model]);
    assert_eq!(code, 2);
    assert!(core.requests().is_empty());
}

#[test]
fn core_error_exits_5() {
    let core = FakeCore::start();
    core.fail_next(26, "Core too old for this content");
    let (code, doc) = run_import(&core, &fixture("lamp.xml"), &lamp(&[]));
    assert_eq!(code, 5, "{doc}");
    assert_eq!(error(&doc)["core_error_code"], 26);
    assert_eq!(core.with_state(|s| s.devices.len()), 5);
}

#[test]
fn verification_failure_exits_8_and_reports_what_the_core_claimed() {
    let core = FakeCore::start();
    // The Core says it added a module that a fresh sync does not contain.
    core.on("MessageHttpApiConfigRequest", |_state, _req| {
        let ghost = json!({"ID": 999, "Name": "Demo gateway", "Model": "PacketParserModuleHttp", "CcuId": 2,
                           "HardwareParentId": 0, "DeviceProperties": {}});
        Reply::new("MessageHttpApiConfigResponse", json!({"CcuId": 2, "AddedDevices": [ghost]}))
    });
    let (code, doc) = run_import(&core, &fixture("lamp.xml"), &lamp(&[]));
    assert_eq!(code, 8, "{doc}");
    let e = error(&doc);
    assert_eq!(e["code"], "verify_failed");
    assert_eq!(e["details"]["verify"]["ok"], false);
    assert_eq!(e["details"]["created"]["modules"][0]["id"], 999);
}

#[test]
fn stored_properties_that_differ_are_reported_not_failed() {
    let core = FakeCore::start();
    core.on(
        "MessageHttpApiConfigRequest",
        config_handler(&["StayConnected"], true),
    );
    let file = temp_file(
        "diff.json",
        r#"{"Modules": [{"Name": "Demo keep", "Model": "PacketParserModuleHttp",
            "DeviceProperties": {"IpAddress": "192.0.2.8", "StayConnected": false, "Password": "s3cret-x"},
            "Devices": []}]}"#,
    );
    let (code, doc) = run_cli(&core, &["pp", "import", &file]);
    assert_eq!(code, 0, "{doc}");
    let d = data(&doc);
    assert_eq!(d["verify"]["ok"], true);
    let diffs = d["property_differences"].as_array().unwrap();
    assert_eq!(diffs.len(), 1);
    assert_eq!(diffs[0]["key"], "StayConnected");
    assert_eq!(diffs[0]["sent"], "False");
    assert_eq!(diffs[0]["stored"], Value::Null);
    assert!(doc["warnings"][0].as_str().unwrap().contains("differently"));
    assert_eq!(d["removed"][0]["id"], 10);
    assert!(
        !doc.to_string().contains("s3cret-x"),
        "password never printed"
    );
}

// ---------------------------------------------------------------- script

#[test]
fn script_get_lists_and_prints() {
    let core = FakeCore::start();
    let multi = "line one\nline two";
    core.with_state(|s| {
        let i = s.devices.iter().position(|d| d["ID"] == 21).unwrap();
        s.devices[i]["DeviceProperties"]["CustomVariables"] = json!("[]");
        s.devices[i]["DeviceProperties"][SCRIPT_KEY] = json!(multi);
    });
    let (code, doc) = run_cli(&core, &["pp", "script", "get", "Parser child"]);
    assert_eq!(code, 0, "{doc}");
    let d = data(&doc);
    assert_eq!(d["device"]["id"], 21);
    let scripts = d["scripts"].as_array().unwrap();
    let keys: Vec<&str> = scripts.iter().map(|s| s["key"].as_str().unwrap()).collect();
    assert_eq!(keys, vec![SCRIPT_KEY, "CustomVariables"]);
    assert_eq!(scripts[0]["length"], multi.len());
    assert_eq!(scripts[0]["first_line"], "line one");

    let (code, doc) = run_cli(&core, &["pp", "script", "get", "21", "--key", SCRIPT_KEY]);
    assert_eq!(code, 0);
    assert_eq!(data(&doc)["text"], multi);

    let (code, doc) = run_cli(&core, &["pp", "script", "get", "21", "--key", "Nope"]);
    assert_eq!(
        (code, error(&doc)["code"].clone()),
        (2, json!("invalid_input"))
    );
    assert_eq!(
        error(&doc)["details"]["script_keys"],
        json!([SCRIPT_KEY, "CustomVariables"])
    );
    let (code, doc) = run_cli(&core, &["pp", "script", "get", "999"]);
    assert_eq!((code, error(&doc)["code"].clone()), (6, json!("not_found")));
}

fn stored_script(core: &FakeCore, key: &str) -> Value {
    core.with_state(|s| {
        s.devices.iter().find(|d| d["ID"] == 21).unwrap()["DeviceProperties"][key].clone()
    })
}

#[test]
fn script_set_changes_verifies_and_is_a_noop_when_equal() {
    let core = FakeCore::start();
    let (code, doc) = run_cli(
        &core,
        &[
            "pp",
            "script",
            "set",
            "21",
            "--key",
            SCRIPT_KEY,
            "--text",
            "8",
            "--dry-run",
        ],
    );
    assert_eq!(code, 0, "{doc}");
    let d = data(&doc);
    assert_eq!(d["operation"], "updateMultipleDevicesProperties");
    assert_eq!(
        d["message"]["fields"],
        json!({"DeviceIds": [21], "DeviceProperties": {SCRIPT_KEY: "8"}})
    );
    assert_eq!(d["diff"][0]["op"], "change");
    assert_eq!(d["diff"][0]["before"]["length"], 1);
    assert_eq!(d["diff"][0]["before"]["first_differing_line"]["text"], "7");
    assert_eq!(d["diff"][0]["after"]["first_differing_line"]["text"], "8");
    assert!(core.writes().is_empty());

    let (code, doc) = run_cli(
        &core,
        &[
            "pp",
            "script",
            "set",
            "Parser child",
            "--key",
            SCRIPT_KEY,
            "--text",
            "8",
        ],
    );
    assert_eq!(code, 0, "{doc}");
    assert_eq!(data(&doc)["verify"]["ok"], true);
    assert_eq!(data(&doc)["changed"], true);
    assert_eq!(stored_script(&core, SCRIPT_KEY), "8");

    core.clear_requests();
    let (code, doc) = run_cli(
        &core,
        &[
            "pp", "script", "set", "21", "--key", SCRIPT_KEY, "--text", "8",
        ],
    );
    assert_eq!(code, 0, "{doc}");
    assert_eq!(data(&doc)["changed"], false);
    assert!(core.writes().is_empty(), "equal text sends nothing");
}

#[test]
fn script_set_from_a_file_with_a_long_text_summarises_the_diff() {
    let core = FakeCore::start();
    let text = "VAR a := 1;\nVAR b := 2;\nRETURN(a + b);\n";
    let file = temp_file("script.txt", text);
    core.with_state(|s| {
        let i = s.devices.iter().position(|d| d["ID"] == 21).unwrap();
        s.devices[i]["DeviceProperties"][SCRIPT_KEY] =
            json!("VAR a := 1;\nVAR b := 3;\nRETURN(a + b);\n");
    });
    let (code, doc) = run_cli(
        &core,
        &[
            "pp",
            "script",
            "set",
            "21",
            "--key",
            SCRIPT_KEY,
            "--file",
            &file,
            "--dry-run",
        ],
    );
    assert_eq!(code, 0, "{doc}");
    let d = &data(&doc)["diff"][0];
    assert_eq!(
        d["before"]["first_differing_line"],
        json!({"number": 2, "text": "VAR b := 3;"})
    );
    assert_eq!(
        d["after"]["first_differing_line"],
        json!({"number": 2, "text": "VAR b := 2;"})
    );
    let (code, _) = run_cli(
        &core,
        &[
            "pp", "script", "set", "21", "--key", SCRIPT_KEY, "--file", &file,
        ],
    );
    assert_eq!(code, 0);
    assert_eq!(stored_script(&core, SCRIPT_KEY), text);
}

#[test]
fn script_set_rejects_unknown_keys_unless_allowed() {
    let core = FakeCore::start();
    let (code, doc) = run_cli(
        &core,
        &[
            "pp",
            "script",
            "set",
            "21",
            "--key",
            "WriteStateScriptPacketParser",
            "--text",
            "1",
        ],
    );
    assert_eq!(
        (code, error(&doc)["code"].clone()),
        (2, json!("invalid_input"))
    );
    assert_eq!(error(&doc)["details"]["script_keys"], json!([SCRIPT_KEY]));
    assert!(core.writes().is_empty());
    let (code, doc) = run_cli(
        &core,
        &[
            "pp",
            "script",
            "set",
            "21",
            "--key",
            "WriteStateScriptPacketParser",
            "--text",
            "1",
            "--allow-new-key",
        ],
    );
    assert_eq!(code, 0, "{doc}");
    assert_eq!(stored_script(&core, "WriteStateScriptPacketParser"), "1");
    let file = temp_file("both.txt", "x");
    let (code, doc) = run_cli(
        &core,
        &[
            "pp", "script", "set", "21", "--key", SCRIPT_KEY, "--text", "1", "--file", &file,
        ],
    );
    assert_eq!((code, error(&doc)["code"].clone()), (2, json!("usage")));
    let (code, _) = run_cli(&core, &["pp", "script", "set", "21", "--key", SCRIPT_KEY]);
    assert_eq!(code, 2);
}

#[test]
fn script_set_reports_a_failed_verification() {
    let core = FakeCore::start();
    core.drop_next_write();
    let (code, doc) = run_cli(
        &core,
        &[
            "pp", "script", "set", "21", "--key", SCRIPT_KEY, "--text", "9",
        ],
    );
    assert_eq!(code, 8, "{doc}");
    assert_eq!(error(&doc)["code"], "verify_failed");
}

// ---------------------------------------------------------------- last run

#[test]
fn last_run_decodes_a_double() {
    let core = FakeCore::start();
    let (code, doc) = run_cli(
        &core,
        &[
            "pp",
            "script",
            "last-run",
            "Parser child",
            "--key",
            SCRIPT_KEY,
        ],
    );
    assert_eq!(code, 0, "{doc}");
    let d = data(&doc);
    assert_eq!(d["has_run"], true);
    assert_eq!(
        d["object"],
        json!({"kind": "device", "type_id": 0, "id": 21, "name": "Parser child"})
    );
    assert_eq!(d["result"]["type"], "double");
    assert_eq!(d["result"]["value"], 7.0);
    assert_eq!(d["result"]["raw"], 7f64.to_bits() as i64);
    assert_eq!(d["key"], SCRIPT_KEY);
}

#[test]
fn last_run_converts_timestamp_and_keeps_unknown_types_raw() {
    let core = FakeCore::start();
    core.on("MessageGetScriptLastRunValuesRequest", |_s, req| {
        Reply::new(
            "MessageGetScriptLastRunValuesReponse",
            json!({"LastRunId": req["LastRunId"],
                   "Values": {"Result": {"SerializerValueTypeId": 7, "SerializerValueBytes": 42, "ProtoString": "on"},
                              "RunTimestampUtc": "2025-01-02T03:04:05.1234567",
                              "RunDuration": 0.25,
                              "ValuesParameter": {"0": {"Lv": {"SerializerValueTypeId": 4,
                                  "SerializerValueBytes": 1.5f64.to_bits() as i64}}}}}),
        )
    });
    let (code, doc) = run_cli(
        &core,
        &["pp", "script", "last-run", "21", "--key", SCRIPT_KEY],
    );
    assert_eq!(code, 0, "{doc}");
    let d = data(&doc);
    assert_eq!(d["result"]["type_id"], 7);
    assert!(
        d["result"].get("value").is_none(),
        "no guessing for other type ids"
    );
    assert_eq!(d["result"]["raw"]["SerializerValueBytes"], 42);
    assert_eq!(d["run_timestamp_utc"], "2025-01-02T03:04:05.1234567Z");
    assert_eq!(d["run_timestamp_ticks"], 638713838451234567i64);
    assert_eq!(d["run_duration_seconds"], 0.25);
    assert_eq!(d["variables_by_phase"]["0"]["Lv"]["value"], 1.5);
}

#[test]
fn last_run_before_the_script_ran_and_other_object_kinds() {
    let core = FakeCore::start();
    let seen = Arc::new(Mutex::new(vec![]));
    let log = seen.clone();
    core.on("MessageGetScriptLastRunValuesRequest", move |_s, req| {
        log.lock().unwrap().push(req["LastRunId"].clone());
        Reply::new(
            "MessageGetScriptLastRunValuesReponse",
            json!({"LastRunId": req["LastRunId"]}),
        )
    });
    let (code, doc) = run_cli(
        &core,
        &["pp", "script", "last-run", "21", "--key", SCRIPT_KEY],
    );
    assert_eq!(code, 0, "{doc}");
    assert_eq!(data(&doc)["has_run"], false);
    assert!(data(&doc).get("result").is_none());

    let (code, doc) = run_cli(
        &core,
        &[
            "pp",
            "script",
            "last-run",
            "2",
            "--key",
            "CcuScript",
            "--object",
            "ccu",
        ],
    );
    assert_eq!(code, 0, "{doc}");
    assert_eq!(data(&doc)["object"]["kind"], "ccu");
    let (code, doc) = run_cli(
        &core,
        &["pp", "script", "last-run", "3", "--object", "rule"],
    );
    assert_eq!(code, 0, "{doc}");
    let ids = seen.lock().unwrap().clone();
    let fields = |v: &Value| {
        (
            v["ObjectIdType"].clone(),
            v["ObjectId"].clone(),
            v["ScriptKey"].clone(),
        )
    };
    assert_eq!(fields(&ids[1]), (json!(1), json!(2), json!("CcuScript")));
    assert_eq!(fields(&ids[2]), (json!(2), json!(3), json!("")));

    let (code, _) = run_cli(&core, &["pp", "script", "last-run", "21"]);
    assert_eq!(code, 2, "a device script needs --key");
    let (code, _) = run_cli(
        &core,
        &[
            "pp", "script", "last-run", "x", "--key", "k", "--object", "ccu",
        ],
    );
    assert_eq!(code, 2);
}

#[test]
fn script_get_and_set_mask_secret_keys_unless_shown() {
    let core = FakeCore::start();
    core.with_state(|s| {
        let i = s.devices.iter().position(|d| d["ID"] == 21).unwrap();
        s.devices[i]["DeviceProperties"]["PasswordScript"] = json!("login(hunter2)");
    });
    let (code, doc) = run_cli(
        &core,
        &["pp", "script", "get", "21", "--key", "PasswordScript"],
    );
    assert_eq!(code, 0, "{doc}");
    assert_eq!(data(&doc)["text"], "<redacted>");
    assert_eq!(data(&doc)["length"], 14);
    let (_, doc) = run_cli(
        &core,
        &[
            "pp",
            "script",
            "get",
            "21",
            "--key",
            "PasswordScript",
            "--show-secrets",
        ],
    );
    assert_eq!(data(&doc)["text"], "login(hunter2)");

    let (code, doc) = run_cli(
        &core,
        &[
            "pp",
            "script",
            "set",
            "21",
            "--key",
            "PasswordScript",
            "--text",
            "login(other)",
            "--dry-run",
        ],
    );
    assert_eq!(code, 0, "{doc}");
    let diff = &data(&doc)["diff"][0];
    assert_eq!(
        (diff["before"].clone(), diff["after"].clone()),
        (json!("<redacted>"), json!("<redacted>"))
    );
    assert!(!doc.to_string().contains("hunter2"));
    assert!(!doc.to_string().contains("login(other)"));
}

#[test]
fn last_run_error_names_the_real_flag() {
    let core = FakeCore::start();
    let (code, doc) = run_cli(
        &core,
        &[
            "pp", "script", "last-run", "x", "--key", "k", "--object", "ccu",
        ],
    );
    assert_eq!(code, 2);
    let message = error(&doc)["message"].as_str().unwrap().to_string();
    assert!(
        message.contains("--object ccu") && !message.contains("--object-kind"),
        "{message}"
    );
}

#[test]
fn import_twice_is_refused_by_name() {
    let core = FakeCore::start();
    let (code, doc) = run_import(&core, &fixture("lamp.xml"), &lamp(&[]));
    assert_eq!(code, 0, "{doc}");
    core.clear_requests();
    // the same template again, e.g. a retry after `write_sent: "unknown"`
    let (code, doc) = run_import(&core, &fixture("lamp.xml"), &lamp(&[]));
    assert_eq!(code, 7, "{doc}");
    assert_eq!(error(&doc)["code"], "already_exist");
    assert!(core.writes().is_empty(), "{:?}", core.writes());
    let (code, doc) = run_import(
        &core,
        &fixture("lamp.xml"),
        &lamp(&["--allow-duplicate-name"]),
    );
    assert_eq!(code, 0, "{doc}");
}

#[test]
fn duplicate_placeholder_ids_are_refused_offline() {
    let core = FakeCore::start();
    let file = temp_file(
        "dup-ids.json",
        r#"{"Modules": [{"Id": -1, "Name": "A", "Model": "PacketParserModuleHttp",
              "Devices": [{"Id": -1, "Name": "A var", "Model": "PacketParserVariable"}]}]}"#,
    );
    let (code, doc) = run_cli(&core, &["pp", "inspect", &file]);
    assert_ne!(code, 0, "{doc}");
    assert!(
        error(&doc)["message"].as_str().unwrap().contains("Id -1"),
        "{doc}"
    );
    assert!(core.requests().is_empty());
}
