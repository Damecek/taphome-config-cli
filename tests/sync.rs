mod common;
use common::*;
use serde_json::json;

#[test]
fn default_is_a_summary() {
    let core = FakeCore::start();
    let (code, doc) = run_cli(&core, &["sync"]);
    assert_eq!(code, 0, "{doc}");
    assert_eq!(doc["command"], "sync");
    let d = data(&doc);
    assert_eq!(d["core_version"], "2025.2.99999");
    assert_eq!(d["protocol"], 89);
    assert_eq!(
        d["counts"],
        json!({"devices": 5, "rules_live": 2, "rules_total": 3, "interfaces": 2, "dashboards": 1})
    );
    assert_eq!(
        d["interfaces"][0],
        json!({"id": 1, "model": "VirtualCCU", "name": "Virtual"})
    );
    assert_eq!(d["interfaces"][1]["model"], "PacketParserCCU");
    assert!(d.get("devices").is_none());
}

#[test]
fn sections() {
    let core = FakeCore::start();
    let (_, doc) = run_cli(&core, &["sync", "--section", "devices"]);
    let devices = data(&doc)["devices"].as_array().unwrap();
    assert_eq!(devices.len(), 5);
    assert!(devices
        .iter()
        .any(|d| d["Name"] == "Test lamp" && d["ID"] == 10));

    let (_, doc) = run_cli(&core, &["sync", "--section", "rules"]);
    let rules = data(&doc)["rules"].as_array().unwrap();
    assert_eq!(rules.len(), 3);
    let live = rules.iter().find(|r| r["id"] == 1).unwrap();
    assert_eq!(
        (
            live["name"].clone(),
            live["deleted"].clone(),
            live["enabled"].clone()
        ),
        (json!("Test rule A"), json!(false), json!(true))
    );
    assert_eq!(
        live["model"]["TriggerParameters"]["EvaluationFormula"],
        "Va"
    );
    assert!(
        live["raw_hex"].as_str().unwrap().len() > 10,
        "the original bytes are kept"
    );
    let tombstone = rules.iter().find(|r| r["id"] == 3).unwrap();
    assert_eq!(tombstone["deleted"], true);

    let (_, doc) = run_cli(&core, &["sync", "--section", "interfaces"]);
    assert_eq!(data(&doc)["interfaces"].as_array().unwrap().len(), 2);
    let (_, doc) = run_cli(&core, &["sync", "--section", "dashboards"]);
    assert_eq!(data(&doc)["dashboards"][0]["Name"], "Test zone");
    let (_, doc) = run_cli(&core, &["sync", "--section", "core"]);
    assert_eq!(data(&doc)["location"]["Name"], "Test home");
    assert_eq!(data(&doc)["trigger_versions"]["12"], 13);
}

#[test]
fn full_has_every_section() {
    let core = FakeCore::start();
    let (_, doc) = run_cli(&core, &["sync", "--full"]);
    let d = data(&doc);
    for key in [
        "core_version",
        "counts",
        "location",
        "interfaces",
        "devices",
        "rules",
        "dashboards",
    ] {
        assert!(d.get(key).is_some(), "missing {key}");
    }
}

#[test]
fn property_secrets_are_hidden_unless_asked_and_the_session_never_shows() {
    let core = FakeCore::start();
    let (_, hidden) = run_cli(&core, &["sync", "--full"]);
    let text = hidden.to_string();
    assert!(
        !text.contains("child-secret-value") && !text.contains("ccu-token-value"),
        "{text}"
    );
    assert!(text.contains("<redacted>"));
    assert!(
        !text.contains("************abcd"),
        "access token is a secret too"
    );

    let (_, shown) = run_cli(&core, &["sync", "--full", "--show-secrets"]);
    let text = shown.to_string();
    assert!(text.contains("child-secret-value") && text.contains("ccu-token-value"));
    // Never visible, with or without --show-secrets.
    let session_id = stored_session(&core)["session_id"]
        .as_str()
        .unwrap()
        .to_string();
    let hash = taphome_cli::salt::password_hash(PASSWORD, SALT.as_bytes());
    for doc in [&hidden, &shown] {
        let text = doc.to_string();
        for secret in [session_id.as_str(), hash.as_str(), PASSWORD, SALT] {
            assert!(!text.contains(secret), "secret leaked into the output");
        }
    }
}

#[test]
#[cfg(unix)]
fn out_file_is_private_and_redacted() {
    let core = FakeCore::start();
    let dir = std::env::temp_dir().join(format!("taphome-out-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("sync.json");
    let (code, doc) = run_cli(&core, &["sync", "--out", file.to_str().unwrap()]);
    assert_eq!(code, 0, "{doc}");
    assert_eq!(
        data(&doc)["counts"]["devices"],
        5,
        "stdout keeps the summary"
    );
    assert_eq!(data(&doc)["out"], file.to_str().unwrap());
    assert_eq!(mode(&file), 0o600);
    let written: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    assert_eq!(written["devices"].as_array().unwrap().len(), 5);
    assert!(!written.to_string().contains("child-secret-value"));
    // --section with --out writes only that section.
    let (code, _) = run_cli(
        &core,
        &[
            "sync",
            "--section",
            "rules",
            "--out",
            file.to_str().unwrap(),
        ],
    );
    assert_eq!(code, 0);
    let written: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    assert!(written.get("devices").is_none() && written["rules"].as_array().unwrap().len() == 3);
    std::fs::remove_dir_all(&dir).unwrap();
}
