//! Fake Core for command tests: a loopback WebSocket server that speaks the real codec.
//!
//! ```ignore
//! mod common;
//! use common::*;
//!
//! #[test]
//! fn lists_devices() {
//!     let core = FakeCore::start();                      // 127.0.0.1:<free port>, own thread and config dir
//!     let (code, doc) = run_cli(&core, &["device", "list"]);  // args without the program name
//!     assert_eq!(code, 0);
//!     assert_eq!(data(&doc)["devices"].as_array().unwrap().len(), 5);
//! }
//! ```
//!
//! Helpers
//! * `FakeCore::start()`, `core.host()`, `core.config_dir()`, `core.env()` (host, user, password and the
//!   invented salt in an `Env` whose config dir is private to this core), `core.ctx()` for library tests.
//! * `run_cli(&core, args)` and `run_cli_with(&core, args, overrides)` where an override `("NAME", None)`
//!   removes a variable. The config dir persists between calls on one core, so session reuse is testable.
//! * `data(&doc)` asserts `ok` and returns `data`. `error(&doc)` asserts failure and returns `error`.
//! * Constants: `SALT`, `USER` (typed with capitals, the Core knows it lower-case), `PASSWORD`.
//!
//! Hooks (all one-shot unless noted)
//! * `core.on("MessageXRequest", |state, request| Reply)` replaces the handler for a class (persistent).
//! * `core.fail_next(ErrorCode, "text")` answers the next request with a MessageGenericResponse error.
//! * `core.drop_next_write()` answers the next write with Ok and changes nothing (verify must fail).
//! * `core.silent_next_request()` never answers the next request (timeout tests).
//! * `core.garbage_on_next_request()` answers the next request with bytes that do not decode.
//! * `core.push_broadcast(class, fields)` and `core.push_raw(bytes)` go to every connected client.
//! * `core.requests()` lists received request classes, `core.writes()` only those of write or action
//!   kind, `core.clear_requests()`, `core.acks()` the stamps clients acknowledged, `core.logins()`.
//! * `core.set_db_hash(..)` makes clients retry the DbHash handshake, `core.forget_sessions()` makes the
//!   stored session answer WrongSession, `core.with_state(|s| ..)` reads or edits the whole state.
//!
//! Implemented requests: Identify, login, reconnect, GetBasicCoreProperties, AddDevice,
//! UpdateMultipleDevicesProperties, DeleteDevice, HttpApiConfig, UpdateTriggerModels,
//! DeleteTriggerModels, GetScriptLastRunValues, GetDeviceErrors, GetDeviceLog. Anything else gets
//! ErrorCode 22 (RequestNotSupported).
#![allow(dead_code)]

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};
use std::net::TcpListener as StdListener;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use taphome_cli::config::Env;
use taphome_cli::core::{Ctx, Global};
use taphome_cli::output::hex;
use taphome_cli::salt::password_hash;
use taphome_cli::{catalog, codec};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, oneshot};
use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};
use tokio_tungstenite::tungstenite::Message;

/// Invented salt with a double quote and a backslash, like the real one has a quote.
pub const SALT: &str = "test\"salt\\for-tests";
pub const USER: &str = "TestUser";
pub const USER_LOGIN: &str = "testuser";
pub const PASSWORD: &str = "test-password";
pub const LOCATION_ID: &str = "11111111-2222-3333-4444-555555555555";

// ---------------------------------------------------------------- state

pub struct Reply {
    pub class: String,
    pub fields: Value,
    pub broadcasts: Vec<(String, Value)>,
}

impl Reply {
    pub fn new(class: &str, fields: Value) -> Reply {
        Reply { class: class.to_string(), fields, broadcasts: vec![] }
    }

    pub fn with_broadcast(mut self, class: &str, fields: Value) -> Reply {
        self.broadcasts.push((class.to_string(), fields));
        self
    }

    pub fn ok() -> Reply {
        Reply::new("MessageGenericResponse", json!({"ResponseFlagsId": 1, "ErrorMessage": "Ok", "ErrorCode": 1}))
    }

    pub fn error(code: i64, text: &str) -> Reply {
        Reply::new("MessageGenericResponse", json!({"ResponseFlagsId": 2, "ErrorMessage": text, "ErrorCode": code}))
    }
}

pub type Handler = Arc<dyn Fn(&mut CoreState, &Value) -> Reply + Send + Sync>;

pub struct CoreState {
    pub location_name: String,
    pub password_hash: String,
    pub sessions: HashSet<String>,
    pub db_hash: String,
    pub protocol: i64,
    pub ccus: Vec<Value>,
    pub devices: Vec<Value>,
    /// Decoded `TriggerModel` values, tombstones included.
    pub rules: Vec<Value>,
    pub rules_changeset: i64,
    pub requests: Vec<String>,
    pub acks: Vec<i64>,
    pub logins: u32,
    pub handlers: HashMap<String, Handler>,
    pub fail_next: Option<(i64, String)>,
    pub drop_next_write: bool,
    pub silent_next: bool,
    pub garbage_next: bool,
    counter: u64,
}

fn guid(n: u64) -> String {
    format!("{:08x}-0000-4000-8000-{:012x}", 0xabc0_0000u64 + n, n)
}

fn rule_model(id: i64, name: &str, enabled: bool) -> Value {
    let class = "FormulaTriggerParameters";
    json!({"Id": id, "TriggerTypeId": 12, "Name": name, "Version": 13, "IsEnabled": enabled, "ChangesetId": 1,
           "TriggerParameters": {"$type": class,
               "InputVariables": [{"DeviceId": 11, "ValueLogType": 62, "Abbreviation": "Va"}],
               "OutputVariables": [{"DeviceId": 11, "ValueLogType": 62}],
               "EvaluationFormula": "Va"}})
}

fn device(id: i64, name: &str, model: &str, interface: &str, ccu: i64, parent: i64, props: Value) -> Value {
    json!({"ID": id, "Name": name, "Model": model, "Interface": interface, "CcuId": ccu, "HardwareParentId": parent,
           "SerialNumber": format!("TEST-{id}"), "DeviceProperties": props})
}

impl CoreState {
    fn seeded() -> CoreState {
        CoreState {
            location_name: "Test home".into(),
            password_hash: password_hash(PASSWORD, SALT.as_bytes()),
            sessions: HashSet::new(),
            db_hash: guid(900),
            protocol: 89,
            ccus: vec![
                json!({"CcuId": 1, "Name": "Virtual", "Model": "VirtualCCU", "SerialNumber": "VIRT-1", "CcuProperties": {}}),
                json!({"CcuId": 2, "Name": "Packet parser", "Model": "PacketParserCCU", "SerialNumber": "PP-1",
                       "CcuProperties": {"ApiToken": "ccu-token-value"}}),
            ],
            devices: vec![
                device(10, "Test lamp", "VirtualSwitch", "Switch", 1, 0, json!({"IconId": "1"})),
                device(11, "Test variable", "VirtualVariableGeneral", "Variable", 1, 0, json!({"IsReadOnly": "False"})),
                device(12, "Test thermometer", "VirtualTemperatureSensor", "TemperatureSensor", 1, 0, json!({})),
                device(20, "Parser module", "PacketParserModuleHttp", "IHardwareParentModule", 2, 0, json!({"IpAddress": "192.0.2.1", "Port": "80"})),
                device(21, "Parser child", "PacketParserVariable", "Variable", 2, 20,
                       json!({"Password": "child-secret-value", "ReadStateScriptPacketParser": "7"})),
            ],
            rules: vec![
                rule_model(1, "Test rule A", true),
                rule_model(2, "Test rule B", false),
                json!({"Id": 3, "TriggerTypeId": 12, "Name": "Old rule", "Version": 13, "IsDeleted": true, "ChangesetId": 5}),
            ],
            rules_changeset: 10,
            requests: vec![],
            acks: vec![],
            logins: 0,
            handlers: HashMap::new(),
            fail_next: None,
            drop_next_write: false,
            silent_next: false,
            garbage_next: false,
            counter: 0,
        }
    }

    fn next_guid(&mut self) -> String {
        self.counter += 1;
        guid(self.counter)
    }

    fn free_device_id(&self) -> i64 {
        (100..).find(|id| !self.devices.iter().any(|d| d["ID"] == *id)).unwrap()
    }

    fn device_index(&self, id: i64) -> Option<usize> {
        self.devices.iter().position(|d| d["ID"] == id)
    }

    fn device_list_broadcast(&self) -> (String, Value) {
        ("MessageDeviceListUpdatedBroadcast".to_string(), json!({"Ccus": self.ccus, "Devices": self.devices}))
    }

    fn rule_hex(model: &Value) -> String {
        hex(&codec::encode("TriggerModel", model).expect("encode TriggerModel"))
    }

    pub fn live_rule_count(&self) -> usize {
        self.rules.iter().filter(|r| r["IsDeleted"] != true).count()
    }

    fn reconnect_ok(&mut self) -> Value {
        let rules: Vec<String> = self.rules.iter().map(Self::rule_hex).collect();
        json!({
            "ReconnectResultId": 1,
            "UserInfo": {"Id": guid(7), "Login": USER_LOGIN, "FullName": "Test User", "IsLocalAccount": true},
            "LocationInfo": {"LocationId": LOCATION_ID, "Name": self.location_name, "IpOnLocalNetwork": "127.0.0.1",
                             "AccessToken": "************abcd", "AuthorizationToken": guid(8)},
            "CurrentProtocolVersion": self.protocol,
            "CoreVersionString": "2025.2.99999",
            "Ccus": self.ccus, "Devices": self.devices, "SmartRules": rules,
            "TriggerVersions": {"12": 13},
            "Dashboards": [{"DashboardId": guid(9), "Name": "Test zone", "DashboardTypeId": 1}],
            "DbHash": self.db_hash,
        })
    }

    fn handle_core_request(&mut self, class: &str, req: &Value) -> Reply {
        if let Some(h) = self.handlers.get(class).cloned() {
            return h(self, req);
        }
        match class {
            "MessageGetBasicCorePropertiesRequest" => Reply::new(
                "MessageGetBasicCorePropertiesResponse",
                json!({"Properties": [{"Name": "Core Name", "IsReadOnly": true,
                                       "ProtoValue": {"$type": "ProtoParameterString", "GenericValue": "fake core"}}]}),
            ),
            "MessageAddDeviceRequest" => {
                let id = self.free_device_id();
                let ccu = req["CcuId"].as_i64().unwrap_or(1);
                let mut d = device(
                    id,
                    req["DeviceName"].as_str().unwrap_or(""),
                    req["Model"].as_str().unwrap_or(""),
                    req["Interface"].as_str().unwrap_or(""),
                    ccu,
                    0,
                    req["DeviceProperties"].clone(),
                );
                d["SerialNumber"] = json!(format!("VIRT-{}", guid(id as u64)));
                if d["DeviceProperties"].is_null() {
                    d["DeviceProperties"] = json!({});
                }
                self.devices.push(d.clone());
                Reply::new("MessageAddDeviceResponse", json!({"Ccus": self.ccus, "AddedDevice": d}))
                    .with_broadcast("MessageDeviceListUpdatedBroadcast", json!({"Ccus": self.ccus, "Devices": self.devices}))
            }
            "MessageUpdateMultipleDevicesPropertiesRequest" => {
                let ids: Vec<i64> = req["DeviceIds"].as_array().map(|a| a.iter().filter_map(Value::as_i64).collect()).unwrap_or_default();
                if ids.is_empty() || ids.iter().any(|id| self.device_index(*id).is_none()) {
                    return Reply::error(6, "device does not exist");
                }
                let props = req["DeviceProperties"].as_object().cloned().unwrap_or_default();
                let mut reply = Reply::ok();
                for id in ids {
                    let i = self.device_index(id).unwrap();
                    for (k, v) in &props {
                        if k == "Name" {
                            self.devices[i]["Name"] = v.clone();
                        } else {
                            self.devices[i]["DeviceProperties"][k] = v.clone();
                        }
                    }
                    reply = reply.with_broadcast("MessageDeviceUpdatedBroadcast", json!({"Device": self.devices[i]}));
                }
                reply
            }
            "MessageDeleteDeviceRequest" => {
                let id = req["DeviceId"].as_i64().unwrap_or(0);
                let Some(i) = self.device_index(id) else { return Reply::error(6, "device does not exist") };
                let has_children = self.devices.iter().any(|d| d["HardwareParentId"] == id);
                if has_children && req["ForceDelete"] != true {
                    return Reply::error(7, "delete the children first");
                }
                self.devices.remove(i);
                self.devices.retain(|d| d["HardwareParentId"] != id);
                Reply::ok().with_broadcast("MessageDeviceListUpdatedBroadcast", json!({"Ccus": self.ccus, "Devices": self.devices}))
            }
            "MessageHttpApiConfigRequest" => self.http_api_config(req),
            "MessageUpdateTriggerModelsRequest" => {
                let mut changed = vec![];
                for model in req["TriggerModels"].as_array().cloned().unwrap_or_default() {
                    let mut model = model;
                    let id = model["Id"].as_i64().unwrap_or(0);
                    self.rules_changeset += 1;
                    model["ChangesetId"] = json!(self.rules_changeset);
                    let existing = if id == 0 {
                        // The Core reuses a tombstone id before it allocates a new one.
                        let reuse = self.rules.iter().position(|r| r["IsDeleted"] == true);
                        let next = self.rules.iter().filter_map(|r| r["Id"].as_i64()).max().unwrap_or(0) + 1;
                        model["Id"] = json!(reuse.map_or(next, |i| self.rules[i]["Id"].as_i64().unwrap()));
                        reuse
                    } else {
                        match self.rules.iter().position(|r| r["Id"] == id && r["IsDeleted"] != true) {
                            Some(i) => Some(i),
                            None => return Reply::error(6, "rule does not exist"),
                        }
                    };
                    match existing {
                        Some(i) => self.rules[i] = model.clone(),
                        None => self.rules.push(model.clone()),
                    }
                    changed.push(Self::rule_hex(&model));
                }
                Reply::ok().with_broadcast(
                    "MessageTriggerModelsUpdatedBroadcast",
                    json!({"TriggerModels": changed, "SmartRulesChangesetId": self.rules_changeset}),
                )
            }
            "MessageDeleteTriggerModelsRequest" => {
                let ids: Vec<i64> = req["TriggerIds"].as_array().map(|a| a.iter().filter_map(Value::as_i64).collect()).unwrap_or_default();
                for id in &ids {
                    if !self.rules.iter().any(|r| r["Id"] == *id && r["IsDeleted"] != true) {
                        return Reply::error(6, "rule does not exist");
                    }
                }
                for id in &ids {
                    self.rules_changeset += 1;
                    let rule = self.rules.iter_mut().find(|r| r["Id"] == *id).unwrap();
                    rule["IsDeleted"] = json!(true);
                    rule["ChangesetId"] = json!(self.rules_changeset);
                    rule.as_object_mut().unwrap().remove("TriggerParameters");
                }
                Reply::ok().with_broadcast(
                    "MessageTriggerModelsDeletedBroadcast",
                    json!({"TriggerIds": ids, "SmartRulesChangesetId": self.rules_changeset}),
                )
            }
            "MessageGetScriptLastRunValuesRequest" => {
                let id = req["LastRunId"]["ObjectId"].as_i64().unwrap_or(-1);
                if self.device_index(id).is_none() {
                    return Reply::error(6, "device does not exist");
                }
                Reply::new(
                    "MessageGetScriptLastRunValuesReponse",
                    json!({"LastRunId": req["LastRunId"],
                           "Values": {"Result": {"SerializerValueTypeId": 4, "SerializerValueBytes": 7f64.to_bits() as i64}}}),
                )
            }
            "MessageGetDeviceErrorsRequest" | "MessageGetDeviceLogRequest" => {
                let id = req["DeviceId"].as_i64().unwrap_or(-1);
                if self.device_index(id).is_none() {
                    return Reply::error(6, "device does not exist");
                }
                if class == "MessageGetDeviceErrorsRequest" {
                    Reply::new("MessageGetDeviceErrorsResponse", json!({"DeviceErrors": []}))
                } else {
                    Reply::new("MessageGetDeviceLogResponse", json!({"ValueChangedLogEntries": []}))
                }
            }
            _ => Reply::error(22, "request not supported by the fake Core"),
        }
    }

    fn http_api_config(&mut self, req: &Value) -> Reply {
        let ccu = req["CcuId"].as_i64().unwrap_or(0);
        if !self.ccus.iter().any(|c| c["CcuId"] == ccu) {
            return Reply::error(6, "interface does not exist");
        }
        let Ok(config) = serde_json::from_str::<Value>(req["ConfigJson"].as_str().unwrap_or("")) else {
            return Reply::error(15, "ConfigJson does not parse");
        };
        let mut added = vec![];
        for module in config["Modules"].as_array().cloned().unwrap_or_default() {
            let module_id = self.free_device_id();
            let mut m = device(
                module_id,
                module["Name"].as_str().unwrap_or(""),
                module["Model"].as_str().unwrap_or(""),
                "IHardwareParentModule",
                ccu,
                0,
                module["DeviceProperties"].clone(),
            );
            m["SerialNumber"] = json!(format!("PacketParser--{module_id:08x}"));
            self.devices.push(m.clone());
            added.push(m);
            for child in module["Devices"].as_array().cloned().unwrap_or_default() {
                let id = self.free_device_id();
                let mut c = device(
                    id,
                    child["Name"].as_str().unwrap_or(""),
                    child["Model"].as_str().unwrap_or(""),
                    "Variable",
                    ccu,
                    module_id,
                    child["DeviceProperties"].clone(),
                );
                c["SerialNumber"] = json!(format!("PacketParser-Variable-{id:08x}"));
                self.devices.push(c.clone());
                added.push(c);
            }
        }
        Reply::new("MessageHttpApiConfigResponse", json!({"CcuId": ccu, "AddedDevices": added}))
            .with_broadcast("MessageDeviceListUpdatedBroadcast", json!({"Ccus": self.ccus, "Devices": self.devices}))
    }
}

// ---------------------------------------------------------------- server

const GARBAGE: [u8; 12] = [0xff; 12];

fn frame(class: &str, fields: &Value, rid: i64) -> Vec<u8> {
    codec::encode_message(class, fields, rid).unwrap_or_else(|e| panic!("fake Core cannot encode {class}: {e}"))
}

type Shared = Arc<Mutex<CoreState>>;

/// Frames to send for one received frame, in order.
fn handle_frame(state: &Shared, bus: &broadcast::Sender<Vec<u8>>, data: &[u8]) -> Vec<Vec<u8>> {
    let mut st = state.lock().unwrap();
    let Ok(msg) = codec::decode_message(data) else { return vec![] };
    let class = msg["$type"].as_str().unwrap_or_default().to_string();
    let rid = msg["RequestsId"].as_i64().unwrap_or(0);
    if class == "MessageReceiveAcknowledgement" {
        let stamps: Vec<i64> = msg["ReceivedMessagesStamps"].as_array().map(|a| a.iter().filter_map(Value::as_i64).collect()).unwrap_or_default();
        st.acks.extend(stamps);
        return vec![];
    }
    st.requests.push(class.clone());
    let mut out = vec![];
    if rid != 0 {
        // The Core acknowledges each request before it answers.
        let stamp = (1i64 << 32) | rid;
        out.push(frame("MessageReceiveAcknowledgement", &json!({"ReceivedMessagesStamps": [stamp]}), 1000 + rid));
    }
    let handshake = class == "MessageLoginRequest" || class == "MessageReconnectRequest";
    if !handshake {
        if st.silent_next {
            st.silent_next = false;
            return out;
        }
        if st.garbage_next {
            st.garbage_next = false;
            out.push(GARBAGE.to_vec());
            return out;
        }
    }
    let reply = match class.as_str() {
        "MessageLoginRequest" => {
            st.logins += 1;
            let result = if msg["LocationId"] != LOCATION_ID || msg["LoginOrEmail"] != USER_LOGIN {
                3
            } else if msg["PasswordHashOrAuthorizationToken"] != st.password_hash.as_str() {
                2
            } else {
                1
            };
            let mut fields = json!({"LoginResultId": result});
            if result == 1 {
                let session = st.next_guid();
                st.sessions.insert(session.clone());
                fields["SessionId"] = json!(session);
                fields["UserInfo"] = json!({"Id": guid(7), "Login": USER_LOGIN, "IsLocalAccount": true});
                fields["LocationInfo"] = json!({"LocationId": LOCATION_ID, "Name": st.location_name});
            }
            Reply::new("MessageLoginResponse", fields)
        }
        "MessageReconnectRequest" => {
            let known = st.sessions.contains(msg["SessionId"].as_str().unwrap_or_default());
            let offered = msg["DbHash"].as_str().unwrap_or_default();
            let fields = if !known {
                json!({"ReconnectResultId": 2})
            } else if offered != st.db_hash {
                json!({"ReconnectResultId": 4, "DbHash": st.db_hash})
            } else {
                st.reconnect_ok()
            };
            Reply::new("MessageReconnectResponse", fields)
        }
        _ => {
            let is_write = catalog::operation(&class).is_some_and(|o| matches!(o.kind.as_str(), "write" | "action"));
            if let Some((code, text)) = st.fail_next.take() {
                Reply::error(code, &text)
            } else if is_write && st.drop_next_write {
                st.drop_next_write = false;
                Reply::ok()
            } else {
                st.handle_core_request(&class, &msg)
            }
        }
    };
    out.push(frame(&reply.class, &reply.fields, rid));
    for (class, fields) in &reply.broadcasts {
        let _ = bus.send(frame(class, fields, 2000));
    }
    out
}

async fn serve(stream: TcpStream, state: Shared, bus: broadcast::Sender<Vec<u8>>) {
    let path = Arc::new(Mutex::new(String::new()));
    let seen = path.clone();
    let callback = move |req: &Request, resp: Response| {
        *seen.lock().unwrap() = req.uri().path().to_string();
        Ok(resp)
    };
    let Ok(ws) = tokio_tungstenite::accept_hdr_async(stream, callback).await else { return };
    let (mut sink, mut stream) = ws.split();
    let path = path.lock().unwrap().clone();
    if path == "/Identify" {
        let info = {
            let st = state.lock().unwrap();
            json!({"LocationId": LOCATION_ID, "Name": st.location_name, "IpOnLocalNetwork": "127.0.0.1", "AccessToken": "************abcd"})
        };
        let bytes = codec::encode("LocationInfo", &info).expect("encode LocationInfo");
        let _ = sink.send(Message::Binary(bytes.into())).await;
        let _ = sink.close().await;
        return;
    }
    let mut rx = bus.subscribe();
    loop {
        tokio::select! {
            incoming = stream.next() => match incoming {
                Some(Ok(Message::Binary(data))) => {
                    for out in handle_frame(&state, &bus, &data) {
                        if sink.send(Message::Binary(out.into())).await.is_err() {
                            return;
                        }
                    }
                }
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => return,
                Some(Ok(_)) => {}
            },
            pushed = rx.recv() => match pushed {
                Ok(bytes) => {
                    if sink.send(Message::Binary(bytes.into())).await.is_err() {
                        return;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => return,
            },
        }
    }
}

pub struct FakeCore {
    addr: String,
    state: Shared,
    bus: broadcast::Sender<Vec<u8>>,
    root: PathBuf,
    stop: Option<oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

static NEXT_ROOT: AtomicU32 = AtomicU32::new(0);

impl FakeCore {
    pub fn start() -> FakeCore {
        let listener = StdListener::bind("127.0.0.1:0").expect("bind loopback");
        listener.set_nonblocking(true).unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let state: Shared = Arc::new(Mutex::new(CoreState::seeded()));
        let (bus, _) = broadcast::channel(64);
        let (stop, stopped) = oneshot::channel();
        let (st, tx) = (state.clone(), bus.clone());
        let thread = std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
            rt.block_on(async move {
                let listener = TcpListener::from_std(listener).unwrap();
                tokio::pin!(stopped);
                loop {
                    tokio::select! {
                        _ = &mut stopped => return,
                        accepted = listener.accept() => {
                            if let Ok((stream, _)) = accepted {
                                tokio::spawn(serve(stream, st.clone(), tx.clone()));
                            }
                        }
                    }
                }
            });
        });
        let root = std::env::temp_dir().join(format!("taphome-test-{}-{}", std::process::id(), NEXT_ROOT.fetch_add(1, Ordering::SeqCst)));
        FakeCore { addr, state, bus, root, stop: Some(stop), thread: Some(thread) }
    }

    pub fn host(&self) -> String {
        self.addr.clone()
    }

    /// Created by the CLI on first use, removed when the core is dropped.
    pub fn config_dir(&self) -> PathBuf {
        self.root.join("config")
    }

    pub fn env(&self) -> Env {
        Env::new(
            self.config_dir(),
            [
                ("TAPHOME_HOST", self.host().as_str()),
                ("TAPHOME_USER", USER),
                ("TAPHOME_PASSWORD", PASSWORD),
                ("TAPHOME_PASSWORD_SALT", SALT),
            ],
        )
    }

    /// Library context for tests that call `core::run_write` and friends directly. Short settle window.
    pub fn ctx(&self) -> Ctx {
        let mut ctx = Ctx::new(self.env(), Global::default(), b"").unwrap();
        ctx.settle = std::time::Duration::from_millis(100);
        ctx
    }

    pub fn with_state<R>(&self, f: impl FnOnce(&mut CoreState) -> R) -> R {
        f(&mut self.state.lock().unwrap())
    }

    pub fn on(&self, class: &str, handler: impl Fn(&mut CoreState, &Value) -> Reply + Send + Sync + 'static) {
        self.with_state(|s| s.handlers.insert(class.to_string(), Arc::new(handler)));
    }

    pub fn fail_next(&self, code: i64, text: &str) {
        self.with_state(|s| s.fail_next = Some((code, text.to_string())));
    }

    pub fn drop_next_write(&self) {
        self.with_state(|s| s.drop_next_write = true);
    }

    pub fn silent_next_request(&self) {
        self.with_state(|s| s.silent_next = true);
    }

    pub fn garbage_on_next_request(&self) {
        self.with_state(|s| s.garbage_next = true);
    }

    pub fn push_broadcast(&self, class: &str, fields: Value) {
        let _ = self.bus.send(frame(class, &fields, 3000));
    }

    pub fn push_raw(&self, bytes: &[u8]) {
        let _ = self.bus.send(bytes.to_vec());
    }

    pub fn requests(&self) -> Vec<String> {
        self.with_state(|s| s.requests.clone())
    }

    pub fn writes(&self) -> Vec<String> {
        self.requests().into_iter().filter(|c| catalog::operation(c).is_some_and(|o| matches!(o.kind.as_str(), "write" | "action"))).collect()
    }

    pub fn clear_requests(&self) {
        self.with_state(|s| s.requests.clear());
    }

    pub fn acks(&self) -> Vec<i64> {
        self.with_state(|s| s.acks.clone())
    }

    pub fn logins(&self) -> u32 {
        self.with_state(|s| s.logins)
    }

    pub fn set_db_hash(&self, hash: &str) {
        self.with_state(|s| s.db_hash = hash.to_string());
    }

    pub fn forget_sessions(&self) {
        self.with_state(|s| s.sessions.clear());
    }
}

impl Drop for FakeCore {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

// ---------------------------------------------------------------- CLI helpers

pub fn run_cli(core: &FakeCore, args: &[&str]) -> (i32, Value) {
    run_cli_with(core, args, &[])
}

/// An override `("NAME", None)` removes the variable.
pub fn run_cli_with(core: &FakeCore, args: &[&str], overrides: &[(&str, Option<&str>)]) -> (i32, Value) {
    let mut env = core.env();
    for (name, value) in overrides {
        match value {
            Some(v) => env.set(name, v),
            None => env.remove(name),
        }
    }
    taphome_cli::cli::run_with_salt(args, env, b"")
}

pub fn data(doc: &Value) -> &Value {
    assert_eq!(doc["ok"], true, "expected success, got {doc}");
    &doc["data"]
}

pub fn error(doc: &Value) -> &Value {
    assert_eq!(doc["ok"], false, "expected failure, got {doc}");
    &doc["error"]
}

pub fn object(pairs: &[(&str, Value)]) -> Value {
    Value::Object(pairs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect::<Map<_, _>>())
}
