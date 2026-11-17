mod common;
use common::*;
use serde_json::{json, Value};
use std::path::PathBuf;

fn fixture(name: &str) -> String {
    format!("{}/tests/fixtures/rule/{name}", env!("CARGO_MANIFEST_DIR"))
}

/// Writes a JSON file into the core's private config dir and returns its path.
fn temp_json(core: &FakeCore, name: &str, value: &Value) -> String {
    std::fs::create_dir_all(core.config_dir()).unwrap();
    let path: PathBuf = core.config_dir().join(name);
    std::fs::write(&path, value.to_string()).unwrap();
    path.to_string_lossy().into_owned()
}

fn fixture_json(name: &str) -> Value {
    serde_json::from_str(&std::fs::read_to_string(fixture(name)).unwrap()).unwrap()
}

fn rule_state(core: &FakeCore, id: i64) -> Value {
    core.with_state(|s| s.rules.iter().find(|r| r["Id"] == id).cloned().unwrap())
}

fn check_names(doc: &Value) -> Vec<(String, bool)> {
    data(doc)["verify"]["checks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| (c["name"].as_str().unwrap().to_string(), c["ok"] == true))
        .collect()
}

#[test]
fn list_and_filters() {
    let core = FakeCore::start();
    let (code, doc) = run_cli(&core, &["rule", "list"]);
    assert_eq!(code, 0, "{doc}");
    let rules = data(&doc)["rules"].as_array().unwrap();
    assert_eq!(rules.len(), 2);
    assert_eq!(rules[0]["id"], 1);
    assert_eq!(rules[0]["type_name"], "FormulaTrigger");
    assert_eq!(rules[0]["version"], 13);
    assert_eq!(rules[0]["enabled"], true);
    assert_eq!(rules[0]["changeset_id"], 1);
    assert_eq!(rules[1]["enabled"], false);

    let (_, doc) = run_cli(&core, &["rule", "list", "--all"]);
    let all = data(&doc)["rules"].as_array().unwrap();
    assert_eq!(all.len(), 3);
    assert_eq!(all[2]["deleted"], true);

    let (_, doc) = run_cli(&core, &["rule", "list", "--name", "rule B"]);
    assert_eq!(data(&doc)["count"], 1);
    let (_, doc) = run_cli(&core, &["rule", "list", "--type", "11"]);
    assert_eq!(data(&doc)["count"], 0);
    let (_, doc) = run_cli(&core, &["rule", "list", "--type", "12", "--all"]);
    assert_eq!(data(&doc)["count"], 3);
    assert!(core.writes().is_empty());
}

#[test]
fn get_by_id_and_name_and_lookup_errors() {
    let core = FakeCore::start();
    let (code, doc) = run_cli(&core, &["rule", "get", "1"]);
    assert_eq!(code, 0, "{doc}");
    let d = data(&doc);
    assert_eq!(d["type_name"], "FormulaTrigger");
    assert_eq!(d["model"]["Name"], "Test rule A");
    assert_eq!(
        d["model"]["TriggerParameters"]["$type"],
        "FormulaTriggerParameters"
    );
    let (_, by_name) = run_cli(&core, &["rule", "get", "Test rule A"]);
    assert_eq!(data(&by_name)["id"], 1);
    // a tombstone is not a live rule
    let (code, doc) = run_cli(&core, &["rule", "get", "Old rule"]);
    assert_eq!((code, error(&doc)["code"].clone()), (6, json!("not_found")));
    let (code, _) = run_cli(&core, &["rule", "get", "3"]);
    assert_eq!(code, 6);
}

#[test]
fn create_happy_path_finds_the_rule_by_name() {
    let core = FakeCore::start();
    let file = fixture("formula_new.json");
    let (code, doc) = run_cli(&core, &["rule", "apply", &file]);
    assert_eq!(code, 0, "{doc}");
    let d = data(&doc);
    // the fake reuses the tombstone id, as the real Core does
    assert_eq!(d["rule_id"], 3);
    assert_eq!(d["verify"]["ok"], true);
    assert_eq!(d["state"]["rules_live"], 3);
    assert_eq!(core.writes(), vec!["MessageUpdateTriggerModelsRequest"]);
    let stored = rule_state(&core, 3);
    assert_eq!(stored["Name"], "TEST fixture formula");
    assert_eq!(stored["Version"], 13, "defaulted from the Core");
    assert!(stored.get("TriggerInputs").is_none());
}

#[test]
fn create_dry_run_sends_nothing_and_shows_an_add_entry() {
    let core = FakeCore::start();
    let (code, doc) = run_cli(
        &core,
        &["rule", "apply", &fixture("formula_new.json"), "--dry-run"],
    );
    assert_eq!(code, 0, "{doc}");
    let d = data(&doc);
    assert_eq!(d["dry_run"], true);
    assert_eq!(d["operation"], "upsertSmartRules");
    assert_eq!(d["verified"], true);
    assert_eq!(d["diff"][0]["op"], "add");
    assert_eq!(d["diff"][0]["after"]["name"], "TEST fixture formula");
    let sent = &d["message"]["fields"]["TriggerModels"][0];
    assert_eq!(
        (sent["Id"].clone(), sent["ChangesetId"].clone()),
        (json!(0), json!(0))
    );
    assert!(core.writes().is_empty());
}

#[test]
fn create_refuses_a_taken_name() {
    let core = FakeCore::start();
    let mut model = fixture_json("formula_new.json");
    model["Name"] = json!("Test rule A");
    let file = temp_json(&core, "dup.json", &model);
    let (code, doc) = run_cli(&core, &["rule", "apply", &file]);
    assert_eq!(
        (code, error(&doc)["code"].clone()),
        (7, json!("already_exist"))
    );
    assert_eq!(error(&doc)["details"]["existing_id"], 1);
    assert!(core.writes().is_empty());
}

#[test]
fn create_strips_core_managed_fields() {
    let core = FakeCore::start();
    let mut model = fixture_json("formula_new.json");
    model["ChangesetId"] = json!(77);
    model["IsDeleted"] = json!(true);
    model["Priorities"] = json!({"11": 8});
    let file = temp_json(&core, "managed.json", &model);
    let (code, doc) = run_cli(&core, &["rule", "apply", &file, "--dry-run"]);
    assert_eq!(code, 0, "{doc}");
    let sent = &data(&doc)["message"]["fields"]["TriggerModels"][0];
    assert_eq!(sent["ChangesetId"], 0);
    assert!(sent.get("IsDeleted").is_none() && sent.get("Priorities").is_none());
}

#[test]
fn version_defaults_and_mismatch_is_invalid_input() {
    let core = FakeCore::start();
    let mut model = fixture_json("formula_new.json");
    model["Version"] = json!(12);
    let file = temp_json(&core, "v12.json", &model);
    let (code, doc) = run_cli(&core, &["rule", "apply", &file]);
    assert_eq!(
        (code, error(&doc)["code"].clone()),
        (2, json!("invalid_input"))
    );
    let message = error(&doc)["message"].as_str().unwrap();
    assert!(
        message.contains("12") && message.contains("13"),
        "{message}"
    );

    model["Version"] = json!(13);
    let file = temp_json(&core, "v13.json", &model);
    let (code, _) = run_cli(&core, &["rule", "apply", &file, "--dry-run"]);
    assert_eq!(code, 0);
    assert!(core.writes().is_empty());
}

#[test]
fn missing_params_type_names_the_types_seen() {
    let core = FakeCore::start();
    let (code, doc) = run_cli(&core, &["rule", "apply", &fixture("formula_no_type.json")]);
    assert_eq!(
        (code, error(&doc)["code"].clone()),
        (2, json!("invalid_input"))
    );
    let message = error(&doc)["message"].as_str().unwrap();
    assert!(message.contains("rule get"), "{message}");
    assert!(message.contains("FormulaTriggerParameters"), "{message}");
    assert_eq!(
        error(&doc)["details"]["seen_types"],
        json!(["FormulaTriggerParameters"])
    );
    assert!(core.writes().is_empty());
}

#[test]
fn unknown_field_is_rejected_before_connecting() {
    let core = FakeCore::start();
    let (code, doc) = run_cli(
        &core,
        &["rule", "apply", &fixture("formula_bad_field.json")],
    );
    assert_eq!(
        (code, error(&doc)["code"].clone()),
        (2, json!("invalid_input"))
    );
    assert!(error(&doc)["message"]
        .as_str()
        .unwrap()
        .contains("EvaluationFormulaa"));
    assert!(
        core.requests().is_empty(),
        "strict encode runs before any connection"
    );
}

#[test]
fn bad_input_files_are_invalid_input() {
    let core = FakeCore::start();
    let missing = core.config_dir().join("nope.json");
    let (code, _) = run_cli(&core, &["rule", "apply", missing.to_str().unwrap()]);
    assert_eq!(code, 2);
    let file = temp_json(&core, "array.json", &json!([1]));
    let (code, _) = run_cli(&core, &["rule", "apply", &file]);
    assert_eq!(code, 2);
}

#[test]
fn unverified_variants_need_the_switch() {
    let core = FakeCore::start();
    // enabled create
    let mut model = fixture_json("formula_new.json");
    model["IsEnabled"] = json!(true);
    let enabled = temp_json(&core, "enabled.json", &model);
    let (code, doc) = run_cli(&core, &["rule", "apply", &enabled]);
    assert_eq!(
        (code, error(&doc)["code"].clone()),
        (7, json!("unverified_operation"))
    );
    // other type: the Core reports no version for type 11, the file carries it
    let (code, doc) = run_cli(&core, &["rule", "apply", &fixture("evaluation_new.json")]);
    assert_eq!(
        (code, error(&doc)["code"].clone()),
        (7, json!("unverified_operation"))
    );
    // update of a non-formula rule: create one with the switch, then edit it
    let (code, doc) = run_cli(
        &core,
        &[
            "rule",
            "apply",
            &fixture("evaluation_new.json"),
            "--allow-unverified",
        ],
    );
    assert_eq!(code, 0, "{doc}");
    let evaluation_id = data(&doc)["rule_id"].as_i64().unwrap().to_string();
    core.clear_requests();
    let (code, doc) = run_cli(&core, &["rule", "get", &evaluation_id]);
    assert_eq!(code, 0, "{doc}");
    let mut model = doc["data"]["model"].clone();
    model["Name"] = json!("Renamed evaluation");
    let edited = temp_json(&core, "edit.json", &model);
    let (code, doc) = run_cli(&core, &["rule", "apply", &edited]);
    assert_eq!(
        (code, error(&doc)["code"].clone()),
        (7, json!("unverified_operation"))
    );
    assert!(core.writes().is_empty());
    let (code, doc) = run_cli(&core, &["rule", "apply", &edited, "--allow-unverified"]);
    assert_eq!(code, 0, "{doc}");
    core.clear_requests();

    // with the switch they go through
    let (code, doc) = run_cli(&core, &["rule", "apply", &enabled, "--allow-unverified"]);
    assert_eq!(code, 0, "{doc}");
    assert_eq!(
        rule_state(&core, evaluation_id.parse().unwrap())["TriggerTypeId"],
        11
    );
}

#[test]
fn get_edit_apply_round_trip_updates_one_field() {
    let core = FakeCore::start();
    let (_, doc) = run_cli(&core, &["rule", "get", "Test rule A"]);
    let mut model = data(&doc)["model"].clone();
    model["TriggerParameters"]["EvaluationFormula"] = json!("Va + 1");
    let file = temp_json(&core, "round.json", &model);

    let (code, doc) = run_cli(&core, &["rule", "apply", &file, "--dry-run"]);
    assert_eq!(code, 0, "{doc}");
    let diff = data(&doc)["diff"].as_array().unwrap();
    assert_eq!(diff.len(), 1, "{diff:?}");
    assert_eq!(diff[0]["op"], "change");
    assert_eq!(diff[0]["path"], "TriggerParameters.EvaluationFormula");
    assert_eq!(diff[0]["after"], "Va + 1");
    assert_eq!(data(&doc)["verified"], true);
    assert!(core.writes().is_empty());

    let (code, doc) = run_cli(&core, &["rule", "apply", &file]);
    assert_eq!(code, 0, "{doc}");
    assert_eq!(data(&doc)["rule_id"], 1);
    assert!(check_names(&doc).iter().all(|(_, ok)| *ok));
    let stored = rule_state(&core, 1);
    assert_eq!(stored["TriggerParameters"]["EvaluationFormula"], "Va + 1");
    assert!(stored["ChangesetId"].as_i64().unwrap() > 1);

    // applying the same file again changes nothing and sends nothing
    core.clear_requests();
    let (_, doc) = run_cli(&core, &["rule", "get", "1"]);
    let file = temp_json(&core, "same.json", &data(&doc)["model"]);
    let (code, doc) = run_cli(&core, &["rule", "apply", &file]);
    assert_eq!(code, 0, "{doc}");
    assert_eq!(data(&doc)["changed"], false);
    assert!(core.writes().is_empty());
}

#[test]
fn update_gates_and_lookups() {
    let core = FakeCore::start();
    let base = run_cli(&core, &["rule", "get", "1"]).1["data"]["model"].clone();
    let apply = |model: &Value, name: &str| {
        let file = temp_json(&core, name, model);
        run_cli(&core, &["rule", "apply", &file, "--allow-unverified"])
    };
    let mut changed = base.clone();
    changed["Id"] = json!(99);
    let (code, doc) = apply(&changed, "u99.json");
    assert_eq!((code, error(&doc)["code"].clone()), (6, json!("not_found")));
    changed["Id"] = json!(3);
    let (code, _) = apply(&changed, "u3.json");
    assert_eq!(code, 6, "a tombstone is not a live rule");

    let mut changed = base.clone();
    changed["TriggerTypeId"] = json!(11);
    let (code, doc) = apply(&changed, "type.json");
    assert_eq!(
        (code, error(&doc)["code"].clone()),
        (2, json!("invalid_input"))
    );
    assert!(core.writes().is_empty());
}

#[test]
fn round_trip_mismatch_stops_every_update_path() {
    let core = FakeCore::start();
    // An unknown field on a known tag with another wire type: the decoder drops the value,
    // so re-encoding cannot give the bytes the Core sent.
    core.with_state(|s| {
        s.rules[0]["$unknown"] = json!([{"tag": 8, "wt": 2, "hex": "00", "in": "TriggerModel"}]);
    });
    let (_, doc) = run_cli(&core, &["rule", "get", "1"]);
    let mut model = data(&doc)["model"].clone();
    model["Name"] = json!("Renamed");
    let file = temp_json(&core, "mismatch.json", &model);
    let (code, doc) = run_cli(&core, &["rule", "apply", &file]);
    assert_eq!(
        (code, error(&doc)["code"].clone()),
        (7, json!("roundtrip_mismatch")),
        "{doc}"
    );
    let (code, doc) = run_cli(&core, &["rule", "disable", "1"]);
    assert_eq!(
        (code, error(&doc)["code"].clone()),
        (7, json!("roundtrip_mismatch"))
    );
    assert!(core.writes().is_empty());
    // the rule that does round-trip is still editable
    let (code, _) = run_cli(&core, &["rule", "disable", "2"]);
    assert_eq!(code, 0, "already disabled: no-op");
}

#[test]
fn enable_and_disable_flip_the_flag_and_keep_the_rest() {
    let core = FakeCore::start();
    let before = rule_state(&core, 2);
    let (code, doc) = run_cli(&core, &["rule", "enable", "Test rule B"]);
    assert_eq!(code, 0, "{doc}");
    assert_eq!(data(&doc)["changed"], true);
    assert!(check_names(&doc).iter().all(|(_, ok)| *ok), "{doc}");
    let after = rule_state(&core, 2);
    assert_eq!(after["IsEnabled"], true);
    assert_eq!(
        after["TriggerParameters"]["EvaluationFormula"],
        before["TriggerParameters"]["EvaluationFormula"]
    );
    assert!(after["ChangesetId"].as_i64() > before["ChangesetId"].as_i64());

    let (code, doc) = run_cli(&core, &["rule", "disable", "2"]);
    assert_eq!(code, 0, "{doc}");
    assert_ne!(
        rule_state(&core, 2)["IsEnabled"],
        true,
        "false decodes as absent"
    );

    // already in that state: nothing is sent
    core.clear_requests();
    let (code, doc) = run_cli(&core, &["rule", "disable", "2"]);
    assert_eq!(code, 0, "{doc}");
    assert_eq!(data(&doc)["changed"], false);
    assert!(core.writes().is_empty());
}

#[test]
fn enable_dry_run_sends_nothing() {
    let core = FakeCore::start();
    let (code, doc) = run_cli(&core, &["rule", "enable", "2", "--dry-run"]);
    assert_eq!(code, 0, "{doc}");
    let d = data(&doc);
    assert_eq!(d["verified"], true);
    assert_eq!(d["diff"][0]["path"], "IsEnabled");
    assert_eq!(
        d["message"]["fields"]["TriggerModels"][0]["IsEnabled"],
        true
    );
    assert!(core.writes().is_empty());
}

#[test]
fn delete_leaves_a_tombstone_and_verifies() {
    let core = FakeCore::start();
    let (code, doc) = run_cli(&core, &["rule", "delete", "Test rule B", "--dry-run"]);
    assert_eq!(code, 0, "{doc}");
    assert_eq!(data(&doc)["diff"][0]["op"], "remove");
    assert_eq!(data(&doc)["diff"][0]["before"]["name"], "Test rule B");
    assert!(core.writes().is_empty());

    let (code, doc) = run_cli(&core, &["rule", "delete", "Test rule B"]);
    assert_eq!(code, 0, "{doc}");
    assert_eq!(data(&doc)["verify"]["ok"], true);
    assert_eq!(data(&doc)["state"]["rules_live"], 1);
    assert_eq!(core.writes(), vec!["MessageDeleteTriggerModelsRequest"]);
    assert_eq!(rule_state(&core, 2)["IsDeleted"], true);
    let (code, _) = run_cli(&core, &["rule", "delete", "Test rule B"]);
    assert_eq!(code, 6, "a tombstone is not found by name");
}

#[test]
fn core_rejection_exits_5_and_a_dropped_write_exits_8() {
    let core = FakeCore::start();
    core.fail_next(6, "no such rule");
    let (code, doc) = run_cli(&core, &["rule", "delete", "1"]);
    assert_eq!(
        (code, error(&doc)["code"].clone()),
        (5, json!("does_not_exist"))
    );

    core.drop_next_write();
    let (code, doc) = run_cli(&core, &["rule", "apply", &fixture("formula_new.json")]);
    assert_eq!(
        (code, error(&doc)["code"].clone()),
        (8, json!("verify_failed"))
    );
    assert_eq!(error(&doc)["details"]["verify"]["ok"], false);
}

fn seed_errors(core: &FakeCore) {
    core.on("MessageGetTriggerErrorsRequest", |_state, request| {
        assert_eq!(request["TriggerId"], 2);
        assert_ne!(request["ShouldSubscribe"], true);
        Reply::new(
            "MessageGetTriggerErrorsResponse",
            json!({"TriggerErrors": [
                {"TapHomeTime": 86400, "ErrorCodeId": 201, "CustomText": "Line 1, bad token", "SeverityId": 3,
                 "ErrorAreaId": 0, "DeviceIds": [11]},
                {"TapHomeTime": 0, "ErrorCodeId": 203, "CustomText": "", "SeverityId": 2, "ErrorAreaId": 1}
            ]}),
        )
    });
}

#[test]
fn errors_decodes_codes_and_times() {
    let core = FakeCore::start();
    seed_errors(&core);
    let (code, doc) = run_cli(&core, &["rule", "errors", "Test rule B"]);
    assert_eq!(code, 0, "{doc}");
    let d = data(&doc);
    assert_eq!(d["rule"]["id"], 2);
    assert_eq!(d["count"], 2);
    let first = &d["errors"][0];
    assert_eq!(first["ErrorCodeName"], "FormulaParseException");
    assert_eq!(first["SeverityName"], "Error");
    assert_eq!(first["TapHomeTime"], 86400);
    assert_eq!(first["TimeIso"], "2016-01-02T00:00:00Z");
    assert_eq!(first["CustomText"], "Line 1, bad token");
    assert_eq!(d["errors"][1]["ErrorCodeName"], "LiteralArgumentException");
    assert_eq!(d["errors"][1]["TimeIso"], "2016-01-01T00:00:00Z");
    assert!(core.writes().is_empty());

    let (code, _) = run_cli(&core, &["rule", "errors", "Old rule"]);
    assert_eq!(code, 6);
}

#[test]
fn a_redacted_passcode_is_never_applied_back() {
    let core = FakeCore::start();
    core.with_state(|s| {
        s.rules[0]["TriggerParameters"] =
            json!({"$type": "AlarmTriggerParameters", "Passcode": "1234"});
    });
    let (_, doc) = run_cli(&core, &["rule", "get", "1"]);
    let mut model = data(&doc)["model"].clone();
    assert_eq!(model["TriggerParameters"]["Passcode"], "<redacted>");
    model["Name"] = json!("Renamed");
    let file = temp_json(&core, "redacted.json", &model);
    let (code, doc) = run_cli(&core, &["rule", "apply", &file, "--allow-unverified"]);
    assert_eq!(
        (code, error(&doc)["code"].clone()),
        (2, json!("invalid_input"))
    );
    assert!(error(&doc)["message"]
        .as_str()
        .unwrap()
        .contains("redacted placeholder"));
    assert!(core.writes().is_empty());
    // read with --show-secrets, edit, apply: the real value goes through
    let (_, doc) = run_cli(&core, &["rule", "get", "1", "--show-secrets"]);
    let mut model = data(&doc)["model"].clone();
    assert_eq!(model["TriggerParameters"]["Passcode"], "1234");
    model["Name"] = json!("Renamed");
    let file = temp_json(&core, "shown.json", &model);
    let (code, doc) = run_cli(&core, &["rule", "apply", &file, "--allow-unverified"]);
    assert_eq!(code, 0, "{doc}");
    assert_eq!(rule_state(&core, 1)["Name"], "Renamed");
}

#[test]
fn update_keeps_core_managed_keys_and_warns() {
    let core = FakeCore::start();
    let (_, doc) = run_cli(&core, &["rule", "get", "1"]);
    let mut model = data(&doc)["model"].clone();
    model["TriggerParameters"]["EvaluationFormula"] = json!("Va + 2");
    model["ChangesetId"] = json!(999);
    model["IsDeleted"] = json!(true);
    model["TriggerInputs"] = json!({"InstantConnections": {"Connections": {"62": [11]}}});
    let file = temp_json(&core, "managed.json", &model);
    let (code, doc) = run_cli(&core, &["rule", "apply", &file, "--dry-run"]);
    assert_eq!(code, 0, "{doc}");
    let sent = &data(&doc)["message"]["fields"]["TriggerModels"][0];
    assert_eq!(sent["ChangesetId"], 1, "the stored changeset is kept");
    assert!(sent.get("IsDeleted").is_none_or(|v| v == false));
    assert!(sent.get("TriggerInputs").is_none());
    let warnings = doc["warnings"].as_array().unwrap();
    for key in ["ChangesetId", "IsDeleted", "TriggerInputs"] {
        assert!(
            warnings.iter().any(|w| w.as_str().unwrap().contains(key)),
            "{key}: {warnings:?}"
        );
    }
    let paths: Vec<&str> = data(&doc)["diff"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["path"].as_str().unwrap())
        .collect();
    assert_eq!(paths, ["TriggerParameters.EvaluationFormula"]);
}

#[test]
fn update_diffs_and_verifies_priorities() {
    let core = FakeCore::start();
    let (_, doc) = run_cli(&core, &["rule", "get", "1"]);
    let mut model = data(&doc)["model"].clone();
    model["Priorities"] = json!({"5": 2});
    let file = temp_json(&core, "prio.json", &model);
    let (_, dry) = run_cli(&core, &["rule", "apply", &file, "--dry-run"]);
    let diff = data(&dry)["diff"].as_array().unwrap();
    assert_eq!(diff.len(), 1, "{diff:?}");
    assert_eq!(
        (diff[0]["path"].clone(), diff[0]["after"].clone()),
        (json!("Priorities"), json!({"5": 2}))
    );
    let (code, doc) = run_cli(&core, &["rule", "apply", &file, "--allow-unverified"]);
    assert_eq!(code, 0, "{doc}");
    assert!(check_names(&doc).contains(&("Priorities read back equal".to_string(), true)));
    assert_eq!(rule_state(&core, 1)["Priorities"], json!({"5": 2}));
    // the same file again is a no-op
    let (_, doc) = run_cli(&core, &["rule", "get", "1"]);
    let file = temp_json(&core, "prio2.json", &data(&doc)["model"]);
    let (_, doc) = run_cli(&core, &["rule", "apply", &file, "--allow-unverified"]);
    assert_eq!(data(&doc)["changed"], false);
}
