//! Codec and catalog tests. Byte vectors come from the Python probe codec (scripts/build_codec_vectors.py).

use std::path::Path;
use std::process::Command;

use serde_json::{json, Value};
use taphome_cli::catalog::{self, Catalog};
use taphome_cli::codec::{self, Codec};

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn vectors() -> Vec<Value> {
    let text = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/vectors/codec.json"),
    )
    .unwrap();
    serde_json::from_str::<Value>(&text)
        .unwrap()
        .as_array()
        .unwrap()
        .clone()
}

#[test]
fn python_vectors_decode_and_encode() {
    let vs = vectors();
    assert!(vs.len() >= 30);
    for v in &vs {
        let name = v["name"].as_str().unwrap();
        let ty = v["type"].as_str().unwrap();
        let raw = unhex(v["hex"].as_str().unwrap());
        let decoded = codec::decode(ty, &raw).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(decoded, v["value"], "{name}: decode");
        let encoded = codec::encode(ty, &v["value"]).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(hex(&encoded), v["hex"].as_str().unwrap(), "{name}: encode");
        if let Some(class) = v.get("class").and_then(Value::as_str) {
            let rid = v["requests_id"].as_i64().unwrap();
            let framed = codec::encode_message(class, &v["fields"], rid)
                .unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(
                hex(&framed),
                v["hex"].as_str().unwrap(),
                "{name}: encode_message"
            );
            assert_eq!(
                codec::decode_message(&framed).unwrap(),
                v["value"],
                "{name}: decode_message"
            );
        }
    }
}

#[test]
fn docs_byte_examples() {
    // docs/protocol.md: MessageGetBasicCorePropertiesRequest with RequestsId 5, and the acknowledgement example
    assert_eq!(
        hex(&codec::encode_message("MessageGetBasicCorePropertiesRequest", &json!({}), 5).unwrap()),
        "f208000805"
    );
    let ack = json!({"ReceivedMessagesStamps": [0x2_0000_0007_i64, 0x3_0000_000c_i64]});
    assert_eq!(
        hex(&codec::encode_message("MessageReceiveAcknowledgement", &ack, 0).unwrap()),
        "a2060c088780808020088c80808030"
    );
    let t = |s: Value| {
        let v = json!({"$type": "CountdownTimerTriggerParameters", "Timeout": s});
        let b = codec::encode("TriggerParametersBase", &v).unwrap();
        hex(&b)
    };
    // TimeSpan table: 5 minutes, 1 second, 0, MaxValue (tag 3 inside include 119 of TriggerParametersBase)
    assert!(t(json!(300.0)).ends_with("1a04080a1002"));
    assert!(t(json!(1.0)).ends_with("1a0408021003"));
    assert!(t(json!(0.0)).ends_with("1a00"));
    assert!(t(json!("Infinity")).ends_with("1a040802100f"));
}

#[test]
fn negative_int32_takes_ten_bytes() {
    let v = json!({"$type": "ProtoParameterInt", "GenericValue": -2});
    let b = codec::encode("ProtoParameterBase", &v).unwrap();
    assert_eq!(hex(&b), "aa060b08feffffffffffffffff01");
    assert_eq!(codec::decode("ProtoParameterBase", &b).unwrap(), v);
}

#[test]
fn large_integers_stay_exact() {
    let v = json!({"$type": "ProtoParameterULong", "GenericValue": 18446744073709551615u64});
    let b = codec::encode("ProtoParameterBase", &v).unwrap();
    assert_eq!(codec::decode("ProtoParameterBase", &b).unwrap(), v);
    let v = json!({"$type": "ProtoParameterLong", "GenericValue": -9223372036854775807i64});
    let b = codec::encode("ProtoParameterBase", &v).unwrap();
    assert_eq!(codec::decode("ProtoParameterBase", &b).unwrap(), v);
}

#[test]
fn unknown_fields_round_trip() {
    let mut raw =
        codec::encode_message("MessageGetBasicCorePropertiesRequest", &json!({}), 5).unwrap();
    raw.extend_from_slice(&[0xc8, 0x01, 0x01]); // tag 25 varint 1
    let d = codec::decode_message(&raw).unwrap();
    assert_eq!(
        d["$unknown"][0],
        json!({"tag": 25, "wt": 0, "hex": "01", "in": "MessageBase"})
    );
    assert_eq!(codec::encode("MessageBase", &d).unwrap(), raw);
}

#[test]
fn encode_rejects_unknown_member_names() {
    let e = codec::encode_message(
        "MessageUpdateDeviceNameRequest",
        &json!({"DeviceID": 1, "Nmae": "x"}),
        2,
    );
    let msg = e.unwrap_err().to_string();
    assert!(msg.contains("no member"), "{msg}");
    let e = codec::encode(
        "DeviceBase",
        &json!({"$type": "GenericDevice", "CcuId": 1, "Bogus": 2}),
    );
    assert!(e.is_err());
    assert!(
        codec::encode("DeviceBase", &json!({"$type": "LocationInfo"})).is_err(),
        "not a subtype"
    );
    assert!(
        codec::encode("LocationInfo", &json!({"Name": 5})).is_err(),
        "wrong JSON type"
    );
}

#[test]
fn bad_wire_data_is_an_error_not_a_panic() {
    for raw in [
        &[0x0a][..],
        &[0x0a, 0x05, 0x01],
        &[0xff],
        &[0x0b],
        &[0x09, 0x01],
    ] {
        let _ = codec::decode("LocationInfo", raw);
    }
    assert!(codec::decode("LocationInfo", &[0x0a, 0x05, 0x01]).is_err());
    // a string field with a varint on the wire lands in $errors, the rest decodes
    let d = codec::decode("LocationInfo", &[0x10, 0x01, 0x30, 0x01]).unwrap();
    assert_eq!(d["$errors"][0]["field"], "Name");
    assert_eq!(d["IsInTestMode"], true);
}

#[test]
fn ack_stamp_combines_message_type_and_id() {
    assert_eq!(
        codec::ack_stamp(&json!({"$type": "MessageGenericResponse", "RequestsId": 7})),
        0x2_0000_0007
    );
    assert_eq!(
        codec::ack_stamp(&json!({"$type": "MessageDeviceValueChangedBroadcast", "RequestsId": 12})),
        0x3_0000_000c
    );
    assert_eq!(
        codec::ack_stamp(&json!({"$type": "MessageReconnectRequest", "RequestsId": 1})),
        0x1_0000_0001
    );
}

#[test]
fn collision_guard_rejects_shared_member_names() {
    let cat = |sub_member: &str| {
        json!({"messages": [], "types": [
            {"name": "Base", "kind": "class", "members": [{"tag": 1, "name": "X", "type": "int"}],
             "includes": [{"tag": 100, "type": "Mid"}]},
            {"name": "Mid", "kind": "class", "members": [{"tag": 1, "name": "Y", "type": "int"}],
             "includes": [{"tag": 100, "type": "Leaf"}]},
            {"name": "Leaf", "kind": "class", "members": [{"tag": 1, "name": sub_member, "type": "string"}]}]})
        .to_string()
    };
    let e = Catalog::from_json(&cat("X")).unwrap_err();
    assert!(e.contains("member X"), "{e}");
    let ok = Catalog::from_json(&cat("Z")).unwrap();
    let c = Codec::new(&ok);
    let v = json!({"$type": "Leaf", "X": 1, "Y": 2, "Z": "z"});
    let b = c.encode("Base", &v).unwrap();
    assert_eq!(hex(&b), "a20608a206030a017a08020801");
    assert_eq!(c.decode("Base", &b).unwrap(), v);
}

#[test]
fn embedded_catalog_has_no_collisions_and_resolves_every_member_type() {
    let cat = catalog::catalog();
    let scalars = [
        "int", "short", "long", "sbyte", "uint", "ushort", "ulong", "byte", "char", "bool",
        "double", "float", "string", "byte[]", "Guid", "DateTime", "TimeSpan",
    ];
    fn leaves(t: &catalog::TypeExpr, out: &mut Vec<String>) {
        match t {
            catalog::TypeExpr::Name(n) => out.push(n.clone()),
            catalog::TypeExpr::List(a) | catalog::TypeExpr::Nullable(a) => leaves(a, out),
            catalog::TypeExpr::Map(k, v) => {
                leaves(k, out);
                leaves(v, out)
            }
            catalog::TypeExpr::Tuple(a) => a.iter().for_each(|x| leaves(x, out)),
        }
    }
    for t in cat.types.values() {
        for m in &t.members {
            let mut names = Vec::new();
            leaves(&m.ty, &mut names);
            for n in names {
                assert!(
                    scalars.contains(&n.as_str()) || cat.types.contains_key(&n),
                    "{}.{}: {n}",
                    t.name,
                    m.name
                );
            }
        }
    }
}

#[test]
fn operation_registry() {
    let op = catalog::operation("MessageGetBasicCorePropertiesRequest").unwrap();
    assert_eq!(
        (op.ids[0].as_str(), op.kind.as_str(), op.verified),
        ("getBasicCoreProperties", "read", true)
    );
    assert_eq!(
        op.response.as_deref(),
        Some("MessageGetBasicCorePropertiesResponse")
    );
    assert_eq!((op.endpoint.as_str(), op.min_protocol), ("/Core", Some(0)));
    let op = catalog::operation("MessageUpdateTriggerModelsRequest").unwrap();
    assert_eq!((op.kind.as_str(), op.verified), ("write", true));
    // kind rule: a class shared by a read and a write/action entry is write/action
    assert_eq!(
        catalog::operation("MessageReconnectRequest").unwrap().kind,
        "action"
    );
    let op = catalog::operation("MessageSetDeviceManualOverrideStateRequest").unwrap();
    assert!(op.kind == "write" || op.kind == "action");
    let op = catalog::operation("MessageLongRunningTaskRequest").unwrap();
    assert_eq!((op.ids.len(), op.verified), (2, false));
    assert!(catalog::operation("MessageNoSuchRequest").is_none());
    let all: Vec<&str> = catalog::operations().map(|(k, _)| k).collect();
    assert!(all.len() > 150 && all.windows(2).all(|w| w[0] < w[1]));
    assert_eq!(
        catalog::operations()
            .find(|(_, o)| o.ids.iter().any(|i| i == "identifyCore"))
            .unwrap()
            .1
            .endpoint,
        "/Identify"
    );
}

#[test]
fn enum_tables() {
    assert_eq!(catalog::enum_name("CommonErrorCodes", 1), Some("Ok"));
    assert_eq!(
        catalog::enum_name("CommonErrorCodes", 22),
        Some("RequestNotSupported")
    );
    assert_eq!(
        catalog::enum_name("CommonErrorCodes", 28),
        Some("MissingData")
    );
    assert_eq!(catalog::enum_name("LoginResult", 1), Some("Ok"));
    assert_eq!(
        catalog::enum_name("ReconnectResult", 4),
        Some("FaultyStorageClient")
    );
    assert_eq!(catalog::enum_name("ValueLogType", 1), Some("State"));
    assert_eq!(
        catalog::enum_name("DeviceStatus", 4),
        Some("CommunicationError")
    );
    assert_eq!(
        catalog::enum_name("TriggerType", 12),
        Some("FormulaTrigger")
    );
    assert_eq!(
        catalog::enum_name("TriggerType", 10001),
        Some("LimitTemperatureSafetyTrigger")
    );
    assert_eq!(catalog::enum_name("DeviceStatus", 99), None);
    assert_eq!(
        catalog::enum_value("ReconnectResult", "WrongSession"),
        Some(2)
    );
    assert_eq!(
        catalog::enum_value("ValueLogType", "RealTemperature"),
        Some(5)
    );
    assert_eq!(
        catalog::message_type("MessageReconnectResponse"),
        Some("Response")
    );
    assert_eq!(
        catalog::message_type("MessageDeviceValueChangedBroadcast"),
        Some("Broadcast")
    );
}

#[test]
fn common_error_codes_match_docs_table() {
    let doc =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/protocol.md"))
            .unwrap();
    let start = doc.find("`CommonErrorCodes` (derived).").unwrap();
    let mut n = 0;
    for line in doc[start..]
        .lines()
        .skip(1)
        .take_while(|l| l.is_empty() || l.starts_with('|'))
    {
        let cells: Vec<&str> = line.split('|').map(str::trim).collect();
        for pair in cells
            .get(1..cells.len().saturating_sub(1))
            .unwrap_or_default()
            .chunks(2)
        {
            if let [code, name] = pair {
                if let Ok(code) = code.parse::<i64>() {
                    assert_eq!(
                        catalog::enum_name("CommonErrorCodes", code),
                        Some(*name),
                        "code {code}"
                    );
                    n += 1;
                }
            }
        }
    }
    assert_eq!(n, 28);
}

/// The committed catalog must match a rebuild from the spec. Skips when no Python with PyYAML is available.
#[test]
fn catalog_is_up_to_date_with_the_spec() {
    let root = env!("CARGO_MANIFEST_DIR");
    let script = format!("{root}/scripts/build_cli_catalog.py");
    let attempts: [&[&str]; 2] = [
        &[
            "uv", "run", "--quiet", "--with", "pyyaml", "python", &script, "--check",
        ],
        &["python3", &script, "--check"],
    ];
    for args in attempts {
        let Ok(out) = Command::new(args[0])
            .args(&args[1..])
            .current_dir(root)
            .output()
        else {
            continue;
        };
        let text = String::from_utf8_lossy(&out.stdout);
        if text.contains("check ok") {
            return;
        }
        if text.contains("check failed") {
            panic!("data/catalog.json is stale: run `uv run --with pyyaml scripts/build_cli_catalog.py`");
        }
    }
    eprintln!("skipped: no Python with PyYAML to rebuild the catalog");
}

/// Local-only cross-check against the Python codec on real captures. The oracle file is produced outside the
/// repository from `work/captures` and named by TAPHOME_CODEC_ORACLE; without it the test does nothing.
/// It prints counts only, never captured data.
#[test]
fn local_capture_oracle() {
    let Ok(path) = std::env::var("TAPHOME_CODEC_ORACLE") else {
        return;
    };
    let text = std::fs::read_to_string(path).unwrap();
    let (mut n, mut bad) = (std::collections::BTreeMap::<String, usize>::new(), 0);
    for (i, line) in text.lines().enumerate() {
        let v: Value = serde_json::from_str(line).unwrap();
        let ty = v["type"].as_str().unwrap();
        let raw = unhex(v["hex"].as_str().unwrap());
        let ok = codec::decode(ty, &raw)
            .map(|d| d == v["value"])
            .unwrap_or(false)
            && codec::encode(ty, &v["value"])
                .map(|b| b == raw)
                .unwrap_or(false);
        if !ok {
            bad += 1;
            eprintln!("oracle entry {i} ({}) differs", v["kind"]);
        }
        *n.entry(v["kind"].as_str().unwrap().to_string())
            .or_default() += 1;
    }
    eprintln!("oracle entries checked: {n:?}, mismatches: {bad}");
    assert_eq!(bad, 0);
}
