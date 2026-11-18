//! Command author API: everything a command in `src/commands/` needs to talk to the Core.
//!
//! A command is `pub async fn run(ctx: &Ctx, cmd: Cmd) -> Result<Value, CliError>` in its own file. It
//! never touches sockets, files or the environment directly.
//!
//! * [`Ctx`] carries the injected [`Env`], the connection settings, the global flags and the
//!   known secrets. Use `ctx.warn(..)` for a warning in the envelope and `ctx.timeout(..)` for limits.
//! * [`open_session`] returns a connected [`Session`]: it reuses the stored session, handles the
//!   `DbHash` handshake and logs in again on WrongSession when the password is in the environment.
//! * [`Snapshot`] is the configuration from the reconnect response: devices, interfaces, smart rules
//!   (decoded, original bytes kept as hex), counts, and `find_device` / `find_rule` lookups by id or
//!   exact name (`not_found` and `ambiguous` are exit 6).
//! * [`request`] sends one request and turns a failed `MessageGenericResponse` into a [`CliError`].
//! * [`run_write`] is the only way to write. It takes the snapshot, builds the message, applies the
//!   safety gate, honours `--dry-run`, sends, then opens a new session and verifies the result. A
//!   command cannot skip a gate because it never sends a write itself.
//!
//! Read command:
//!
//! ```no_run
//! use serde_json::{json, Value};
//! use taphome_cli::core::{open_session, Ctx, Snapshot};
//! use taphome_cli::errors::CliError;
//!
//! async fn list_devices(ctx: &Ctx) -> Result<Value, CliError> {
//!     let session = open_session(ctx).await?;
//!     let snapshot = Snapshot::from_session(&session)?;
//!     session.close().await;
//!     Ok(json!({"devices": snapshot.devices}))
//! }
//! ```
//!
//! Write command (`rename`): build the message from the snapshot, describe it in `diff`, and say what
//! must be true in a fresh snapshot afterwards.
//!
//! ```no_run
//! use serde_json::{json, Value};
//! use taphome_cli::core::{diff_entry, run_write, Check, Ctx, WriteFlags, WritePlan, WriteSpec};
//! use taphome_cli::errors::CliError;
//!
//! async fn rename(ctx: &Ctx, flags: WriteFlags, device: &str, name: &str) -> Result<Value, CliError> {
//!     let name = name.to_string();
//!     let spec = WriteSpec::new(
//!         "updateMultipleDevicesProperties",
//!         "MessageUpdateMultipleDevicesPropertiesRequest",
//!         flags,
//!         move |snapshot| {
//!             let device = snapshot.find_device(device)?;
//!             let id = device["ID"].as_i64().unwrap_or(0);
//!             let diff = vec![diff_entry("change", "device", json!(id), "Name", device["Name"].clone(), json!(name))];
//!             Ok(WritePlan::new(
//!                 json!({"DeviceIds": [id], "DeviceProperties": {"Name": name}}),
//!                 diff,
//!                 move |after, _response| {
//!                     let renamed = after.device(id).map(|d| d["Name"] == json!(name)).unwrap_or(false);
//!                     vec![Check::new("device has the new name", renamed)]
//!                 },
//!             ))
//!         },
//!     );
//!     // Set `spec.variant_verified = false` for a variant that was never tested on a real Core.
//!     run_write(ctx, spec).await
//! }
//! ```
use crate::client::{self, Options, Session, Timeouts};
use crate::config::{self, Config, Env, StoredSession};
use crate::errors::{check_generic_response, CliError, Result};
use crate::output::{from_hex, hex, Redactor};
use crate::salt::Salt;
use crate::{catalog, codec};
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::Duration;

// ---------------------------------------------------------------- context

#[derive(Clone, Debug, Default)]
pub struct Global {
    pub pretty: bool,
    pub show_secrets: bool,
    /// `--timeout S`, replaces every limit of the app's defaults.
    pub timeout: Option<f64>,
}

pub struct Ctx {
    pub env: Env,
    pub config: Config,
    pub global: Global,
    pub salt: Salt,
    /// How long `run_write` collects broadcasts after the Core answered.
    pub settle: Duration,
    secrets: Mutex<Vec<String>>,
    warnings: Mutex<Vec<String>>,
}

impl Ctx {
    /// `built_in_salt` is `salt::BUILT_IN` in the binary. Tests pass their own.
    pub fn new(env: Env, global: Global, built_in_salt: &[u8]) -> Result<Ctx> {
        let config = Config::load(&env)?;
        let salt = Salt::resolve(&env, built_in_salt);
        let secrets = [
            config.password.clone(),
            config.cloud_password.clone(),
            salt.as_text().map(str::to_string),
        ]
        .into_iter()
        .flatten()
        .collect();
        Ok(Ctx {
            env,
            config,
            global,
            salt,
            settle: Duration::from_millis(400),
            secrets: Mutex::new(secrets),
            warnings: Mutex::new(vec![]),
        })
    }

    pub fn options(&self) -> Options {
        let mut opts = Options::standard();
        if let Some(seconds) = self.global.timeout {
            opts.timeouts = Timeouts::uniform(Duration::from_secs_f64(seconds));
        }
        opts
    }

    /// `--timeout` when given, else `default`.
    pub fn timeout(&self, default: Duration) -> Duration {
        self.global
            .timeout
            .map(Duration::from_secs_f64)
            .unwrap_or(default)
    }

    /// Registers a value that must never appear in the output (session id, hash).
    pub fn add_secret(&self, value: &str) {
        if !value.is_empty() {
            self.secrets
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .push(value.to_string());
        }
    }

    pub fn warn(&self, message: impl Into<String>) {
        self.warnings
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(message.into());
    }

    pub fn take_warnings(&self) -> Vec<String> {
        std::mem::take(&mut *self.warnings.lock().unwrap_or_else(|p| p.into_inner()))
    }

    pub fn redactor(&self) -> Redactor {
        Redactor {
            show_secrets: self.global.show_secrets,
            values: self
                .secrets
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .clone(),
        }
    }
}

// ---------------------------------------------------------------- session

fn missing_config(missing: &[&str]) -> CliError {
    CliError::new(
        "missing_config",
        format!("set {} in the environment", missing.join(", ")),
    )
    .with_details(json!({"missing": missing}))
}

fn host_of(ctx: &Ctx, stored: Option<&StoredSession>) -> Result<String> {
    ctx.config
        .host
        .clone()
        .or_else(|| stored.map(|s| s.host.clone()))
        .ok_or_else(|| missing_config(&["TAPHOME_HOST"]))
}

pub struct LoginOutcome {
    pub session: StoredSession,
    /// `LocationInfo` from `/Identify`.
    pub location: Value,
}

/// Identify, login, store the session. Needs host, user and password, or the TapHome account in cloud mode.
pub async fn login(ctx: &Ctx) -> Result<LoginOutcome> {
    if ctx.config.is_cloud() {
        return cloud_login(ctx).await;
    }
    let (host, user, password) = match (&ctx.config.host, &ctx.config.user, &ctx.config.password) {
        (Some(h), Some(u), Some(p)) => (h.clone(), u.to_lowercase(), p.clone()),
        (h, u, p) => {
            let missing: Vec<&str> = [
                ("TAPHOME_HOST", h.is_none()),
                ("TAPHOME_USER", u.is_none()),
                ("TAPHOME_PASSWORD", p.is_none()),
            ]
            .into_iter()
            .filter_map(|(name, absent)| absent.then_some(name))
            .collect();
            return Err(missing_config(&missing));
        }
    };
    let hash = ctx.salt.hash(&password)?;
    ctx.add_secret(&hash);
    let timeouts = ctx.options().timeouts;
    let location = client::identify(&host, &timeouts).await?;
    let location_id = location["LocationId"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let session = core_login(ctx, host, location_id, user, &hash).await?;
    config::save_session(&ctx.env, &session)?;
    Ok(LoginOutcome { session, location })
}

/// TapHome account: `login2` in the cloud for the location's authorization token, then the Core login
/// through the tunnel with that token in place of the password.
async fn cloud_login(ctx: &Ctx) -> Result<LoginOutcome> {
    let config = &ctx.config;
    let (Some(email), Some(password)) = (&config.cloud_email, &config.cloud_password) else {
        return Err(missing_config(&["TAPHOME_CLOUD_PASSWORD"]));
    };
    let device_id = config::device_id(&ctx.env)?;
    let cloud = crate::cloud::login(
        &config.cloud_api,
        email,
        password,
        &device_id,
        config.location.as_deref(),
        ctx.timeout(crate::cloud::HTTP_TIMEOUT),
    )?;
    ctx.add_secret(&cloud.location.authorization_token);
    let hash = ctx.salt.hash(&cloud.location.authorization_token)?;
    ctx.add_secret(&hash);
    // shortcut: always the first tunnel, the app picks one at random; upgrade if a relay goes down
    let tunnel = cloud.tunnels[0].clone();
    let mut session = core_login(
        ctx,
        tunnel,
        cloud.location.id.clone(),
        email.to_lowercase(),
        &hash,
    )
    .await?;
    session.location_name = Some(cloud.location.name.clone());
    config::save_session(&ctx.env, &session)?;
    let location = json!({"LocationId": cloud.location.id, "Name": cloud.location.name});
    Ok(LoginOutcome { session, location })
}

/// `MessageLoginRequest` on a throwaway socket. The caller stores the session.
async fn core_login(
    ctx: &Ctx,
    host: String,
    location_id: String,
    user: String,
    hash: &str,
) -> Result<StoredSession> {
    let timeouts = ctx.options().timeouts;
    let device_id = config::device_id(&ctx.env)?;
    let response = client::login(&host, &location_id, &user, hash, &device_id, &timeouts).await?;
    let result = response["LoginResultId"].as_i64().unwrap_or(0);
    let response_location = response["LocationInfo"]["LocationId"]
        .as_str()
        .unwrap_or(&location_id);
    if result == 1 && response_location != location_id {
        return Err(
            CliError::new("login_failed", "the Core answered for a different location")
                .with_details(json!({"login_result": "WrongLogin"})),
        );
    }
    if result != 1 {
        let name = catalog::enum_name("LoginResult", result)
            .map(str::to_string)
            .unwrap_or_else(|| result.to_string());
        return Err(CliError::new(
            "login_failed",
            format!("the Core refused the login: {name}"),
        )
        .with_details(json!({"login_result": name})));
    }
    let session_id = response["SessionId"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    if session_id.is_empty() {
        return Err(CliError::protocol(
            "the Core accepted the login but sent no session id",
        ));
    }
    ctx.add_secret(&session_id);
    let session = StoredSession {
        host,
        location_id,
        user,
        session_id,
        db_hash: None,
        location_name: None,
    };
    Ok(session)
}

pub enum Reconnect {
    Ready(Session),
    /// The Core does not know the session (ReconnectResult 2, 3 or 8).
    WrongSession,
}

/// One reconnect with the stored session, with the `DbHash` retry (result 4) and its persistence.
/// The other ReconnectResult values become errors.
pub async fn reconnect(ctx: &Ctx, stored: &StoredSession) -> Result<Reconnect> {
    ctx.add_secret(&stored.session_id);
    let device_id = config::device_id(&ctx.env)?;
    let mut db_hash = stored.db_hash.clone();
    // The relay answers CoreOffline for a few seconds after an earlier tunnel socket of the same
    // session closed (seen live: 3 to 10 s), so this waits instead of failing at once.
    let mut offline_waits = [1u64, 2, 4, 8, 8].into_iter();
    let mut attempt = 0;
    while attempt < 2 {
        let session = Session::connect(
            &stored.host,
            &stored.location_id,
            &stored.user,
            &stored.session_id,
            db_hash.as_deref(),
            &device_id,
            ctx.options(),
        )
        .await?;
        if session.reconnect["$type"] == "MessageGenericResponse" {
            let refusal = session.reconnect.clone();
            session.close().await;
            if refusal["ErrorCode"] != json!(21) {
                check_generic_response(&refusal)?;
                return Err(CliError::protocol("the reconnect got a generic response"));
            }
            match offline_waits.next() {
                Some(seconds) => {
                    tokio::time::sleep(Duration::from_secs(seconds)).await;
                    continue;
                }
                None => {
                    return Err(CliError::new(
                        "core_offline",
                        "the TapHome cloud cannot reach the Core: check that the Core is online, then try again",
                    )
                    .with_core_code(21)
                    .with_exit(crate::errors::EXIT_CONNECTION))
                }
            }
        }
        let result = session.reconnect["ReconnectResultId"].as_i64().unwrap_or(0);
        let offered = session.reconnect["DbHash"]
            .as_str()
            .filter(|h| !h.is_empty())
            .map(str::to_string);
        if result == 1 {
            if offered.is_some() && offered != stored.db_hash {
                // The DbHash cache only saves a round trip: a failed write must not fail the command.
                let cached = StoredSession {
                    db_hash: offered,
                    ..stored.clone()
                };
                if let Err(e) = config::save_session(&ctx.env, &cached) {
                    ctx.warn(format!("cannot store the DbHash: {}", e.message));
                }
            }
            return Ok(Reconnect::Ready(session));
        }
        let failure = match result {
            4 if attempt == 0 && offered.is_some() => {
                db_hash = offered;
                None
            }
            4 => Some(CliError::protocol("the Core keeps rejecting the DbHash")),
            2 | 3 | 8 => return close_then(session, Ok(Reconnect::WrongSession)).await,
            7 => Some(CliError::new(
                "core_not_ready",
                "the Core is still starting, try again in about 10 seconds",
            )),
            5 => Some(
                CliError::new("unauthorized", "the user has no remote access")
                    .with_exit(crate::errors::EXIT_AUTH),
            ),
            6 => Some(CliError::protocol("the Core says this client is too old")),
            other => Some(CliError::protocol(format!(
                "unexpected ReconnectResult {other}"
            ))),
        };
        session.close().await;
        if let Some(e) = failure {
            return Err(e);
        }
        attempt += 1;
    }
    Err(CliError::protocol("the Core keeps rejecting the DbHash"))
}

async fn close_then<T>(session: Session, value: T) -> T {
    session.close().await;
    value
}

/// A stored session counts only for the configured host and user, or in cloud mode for the
/// configured TapHome account and location.
pub fn stored_for_config(ctx: &Ctx) -> Option<StoredSession> {
    config::load_session(&ctx.env).filter(|s| {
        if let Some(email) = &ctx.config.cloud_email {
            let location_ok = ctx.config.location.as_ref().is_none_or(|w| {
                s.location_id.eq_ignore_ascii_case(w)
                    || s.location_name.as_ref().map(|n| n.to_lowercase()) == Some(w.to_lowercase())
            });
            return client::is_tunnel(&s.host) && s.user == email.to_lowercase() && location_ok;
        }
        ctx.config.host.as_ref().is_none_or(|h| *h == s.host)
            && ctx
                .config
                .user
                .as_ref()
                .is_none_or(|u| u.to_lowercase() == s.user)
    })
}

fn can_log_in(ctx: &Ctx) -> bool {
    let c = &ctx.config;
    if c.is_cloud() {
        c.cloud_password.is_some()
    } else {
        c.user.is_some() && c.password.is_some()
    }
}

/// Connected session on a valid login: reuses the stored session, logs in when there is none, and logs
/// in again on WrongSession when the password is configured (else `session_invalid`).
pub async fn open_session(ctx: &Ctx) -> Result<Session> {
    let mut stored = stored_for_config(ctx);
    let mut fresh = false;
    loop {
        let current = match stored.take() {
            Some(s) => s,
            None => {
                fresh = true;
                login(ctx).await?.session
            }
        };
        host_of(ctx, Some(&current))?;
        match reconnect(ctx, &current).await? {
            Reconnect::Ready(session) => return Ok(session),
            Reconnect::WrongSession if fresh => {
                return Err(CliError::new(
                    "session_invalid",
                    "the Core rejected a session it just issued",
                ));
            }
            Reconnect::WrongSession if !can_log_in(ctx) => {
                return Err(CliError::new(
                    "session_invalid",
                    "the stored session is no longer valid: set TAPHOME_USER and TAPHOME_PASSWORD (or TAPHOME_CLOUD_EMAIL and TAPHOME_CLOUD_PASSWORD) to log in again",
                )
                .with_details(json!({"stored_session": true})));
            }
            Reconnect::WrongSession => {}
        }
    }
}

// ---------------------------------------------------------------- snapshot

/// One smart rule from the reconnect response.
#[derive(Clone, Debug)]
pub struct Rule {
    pub id: i64,
    pub name: String,
    pub type_id: i64,
    pub version: i64,
    pub enabled: bool,
    /// Tombstone of a deleted rule. Its parameters are gone.
    pub deleted: bool,
    /// The decoded `TriggerModel`.
    pub model: Value,
    /// The serialized `TriggerModel` as received, lower-case hex.
    pub raw_hex: String,
}

impl Rule {
    pub fn to_json(&self) -> Value {
        json!({"id": self.id, "name": self.name, "type_id": self.type_id, "version": self.version, "enabled": self.enabled,
               "deleted": self.deleted, "model": self.model, "raw_hex": self.raw_hex})
    }
}

/// The configuration the Core sent in the reconnect response.
#[derive(Clone, Debug)]
pub struct Snapshot {
    pub core_version: String,
    pub protocol: i64,
    pub location: Value,
    pub user: Value,
    pub devices: Vec<Value>,
    pub interfaces: Vec<Value>,
    /// Live rules and tombstones. Use `live_rules()` for lookups.
    pub rules: Vec<Rule>,
    pub dashboards: Vec<Value>,
    /// Smart rule schema version per trigger type id.
    pub trigger_versions: BTreeMap<i64, i64>,
    pub core_properties: Vec<Value>,
}

fn array(v: &Value) -> Vec<Value> {
    v.as_array().cloned().unwrap_or_default()
}

fn int(v: &Value, key: &str) -> i64 {
    v[key].as_i64().unwrap_or(0)
}

impl Snapshot {
    pub fn from_session(session: &Session) -> Result<Snapshot> {
        Snapshot::from_reconnect(&session.reconnect)
    }

    pub fn from_reconnect(r: &Value) -> Result<Snapshot> {
        if r["$type"] != "MessageReconnectResponse" {
            return Err(CliError::protocol(
                "no reconnect response to build a snapshot from",
            ));
        }
        let rules = array(&r["SmartRules"])
            .iter()
            .map(|raw| {
                let raw_hex = raw.as_str().unwrap_or_default().to_string();
                let bytes = from_hex(&raw_hex)
                    .ok_or_else(|| CliError::protocol("a smart rule is not valid hex"))?;
                let model = codec::decode("TriggerModel", &bytes)
                    .map_err(|e| CliError::protocol(format!("undecodable smart rule: {e}")))?;
                Ok(Rule {
                    id: int(&model, "Id"),
                    name: model["Name"].as_str().unwrap_or_default().to_string(),
                    type_id: int(&model, "TriggerTypeId"),
                    version: int(&model, "Version"),
                    enabled: model["IsEnabled"].as_bool().unwrap_or(false),
                    deleted: model["IsDeleted"].as_bool().unwrap_or(false),
                    model,
                    raw_hex,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let trigger_versions = r["TriggerVersions"]
            .as_object()
            .map(|m| {
                m.iter()
                    .filter_map(|(k, v)| Some((k.parse().ok()?, v.as_i64()?)))
                    .collect()
            })
            .unwrap_or_default();
        Ok(Snapshot {
            core_version: r["CoreVersionString"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            protocol: int(r, "CurrentProtocolVersion"),
            location: r["LocationInfo"].clone(),
            user: r["UserInfo"].clone(),
            devices: array(&r["Devices"]),
            interfaces: array(&r["Ccus"]),
            rules,
            dashboards: array(&r["Dashboards"]),
            trigger_versions,
            core_properties: array(&r["CoreProperties"]),
        })
    }

    pub fn live_rules(&self) -> impl Iterator<Item = &Rule> {
        self.rules.iter().filter(|r| !r.deleted)
    }

    /// `{"devices", "rules_live", "rules_total", "interfaces", "dashboards"}`.
    pub fn counts(&self) -> Value {
        json!({"devices": self.devices.len(), "rules_live": self.live_rules().count(), "rules_total": self.rules.len(),
               "interfaces": self.interfaces.len(), "dashboards": self.dashboards.len()})
    }

    pub fn device(&self, id: i64) -> Option<&Value> {
        self.devices.iter().find(|d| int(d, "ID") == id)
    }

    pub fn interface(&self, id: i64) -> Option<&Value> {
        self.interfaces.iter().find(|c| int(c, "CcuId") == id)
    }

    /// Live rule by id.
    pub fn rule(&self, id: i64) -> Option<&Rule> {
        self.live_rules().find(|r| r.id == id)
    }

    pub fn trigger_version(&self, type_id: i64) -> Option<i64> {
        self.trigger_versions.get(&type_id).copied()
    }

    /// A device by integer id, or by exact name when exactly one device has it.
    pub fn find_device(&self, reference: &str) -> Result<&Value> {
        lookup(
            "device",
            reference,
            self.devices.iter(),
            |d| int(d, "ID"),
            |d| d["Name"].as_str().unwrap_or_default(),
        )
    }

    /// A live rule by integer id, or by exact name when exactly one live rule has it.
    pub fn find_rule(&self, reference: &str) -> Result<&Rule> {
        lookup(
            "rule",
            reference,
            self.live_rules(),
            |r| r.id,
            |r| r.name.as_str(),
        )
    }

    /// An interface by integer `CcuId`, or by exact name when exactly one has it.
    pub fn find_interface(&self, reference: &str) -> Result<&Value> {
        lookup(
            "interface",
            reference,
            self.interfaces.iter(),
            |c| int(c, "CcuId"),
            |c| c["Name"].as_str().unwrap_or_default(),
        )
    }
}

fn lookup<'a, T: 'a>(
    kind: &str,
    reference: &str,
    items: impl Iterator<Item = &'a T>,
    id: impl Fn(&T) -> i64,
    name: impl Fn(&T) -> &str,
) -> Result<&'a T> {
    let items: Vec<&T> = items.collect();
    if let Ok(wanted) = reference.trim().parse::<i64>() {
        if let Some(found) = items.iter().find(|i| id(i) == wanted) {
            return Ok(found);
        }
    }
    let matches: Vec<&T> = items.into_iter().filter(|i| name(i) == reference).collect();
    match matches.as_slice() {
        [one] => Ok(one),
        [] => Err(
            CliError::new("not_found", format!("no {kind} with id or exact name \"{reference}\""))
                .with_details(json!({"kind": kind, "reference": reference})),
        ),
        many => Err(
            CliError::new("ambiguous", format!("{} {kind}s are named \"{reference}\": use the id", many.len()))
                .with_details(json!({"kind": kind, "reference": reference, "ids": many.iter().map(|i| id(i)).collect::<Vec<_>>()})),
        ),
    }
}

// ---------------------------------------------------------------- requests

/// Write and action classes, and classes the registry does not know, may change the Core.
pub fn is_write_class(class: &str) -> bool {
    catalog::operation(class).is_none_or(|op| matches!(op.kind.as_str(), "write" | "action"))
}

/// Sends one read or subscribe request and returns the decoded response. A `MessageGenericResponse`
/// failure becomes a [`CliError`]. When `expect` is given, any other response class is a
/// `protocol_error`. Write classes are refused here: they go through [`run_write`].
pub async fn request(
    ctx: &Ctx,
    session: &Session,
    class: &str,
    fields: &Value,
    expect: Option<&str>,
) -> Result<Value> {
    if is_write_class(class) {
        return Err(CliError::internal(format!(
            "{class} can change the Core: send it with run_write"
        )));
    }
    send_request(ctx, session, class, fields, expect).await
}

async fn send_request(
    ctx: &Ctx,
    session: &Session,
    class: &str,
    fields: &Value,
    expect: Option<&str>,
) -> Result<Value> {
    let response = session
        .request(class, fields, ctx.options().timeouts.request)
        .await?;
    let got = response["$type"].as_str().unwrap_or_default();
    if got == "MessageGenericResponse" {
        check_generic_response(&response)?;
    }
    match expect {
        Some(want) if want != got => Err(CliError::protocol(format!(
            "expected {want}, the Core answered {got}"
        ))),
        _ => Ok(response),
    }
}

// ---------------------------------------------------------------- write flow

/// `--dry-run` and `--allow-unverified`, shared by every write command with `#[command(flatten)]`.
#[derive(clap::Args, Clone, Copy, Debug, Default)]
pub struct WriteFlags {
    /// Print the message and the diff, send nothing.
    #[arg(long)]
    pub dry_run: bool,
    /// Allow an operation or variant that was not tested against a real Core.
    #[arg(long)]
    pub allow_unverified: bool,
}

#[derive(Clone, Debug)]
pub struct Check {
    pub name: String,
    pub ok: bool,
    pub detail: Option<String>,
}

impl Check {
    pub fn new(name: impl Into<String>, ok: bool) -> Check {
        Check {
            name: name.into(),
            ok,
            detail: None,
        }
    }

    pub fn detail(mut self, detail: impl Into<String>) -> Check {
        self.detail = Some(detail.into());
        self
    }

    pub fn to_json(&self) -> Value {
        json!({"name": self.name, "ok": self.ok, "detail": self.detail})
    }
}

/// `{"op": "add|remove|change", "object": "device|rule|interface", "id", "path", "before", "after"}`.
pub fn diff_entry(
    op: &str,
    object: &str,
    id: Value,
    path: &str,
    before: Value,
    after: Value,
) -> Value {
    json!({"op": op, "object": object, "id": id, "path": path, "before": before, "after": after})
}

type VerifyFn = Box<dyn Fn(&Snapshot, &Value) -> Vec<Check>>;

/// What `build` returns: the request fields, the diff entries and the check that runs against the
/// fresh snapshot (and the Core's response) after the write.
pub struct WritePlan {
    pub fields: Value,
    pub diff: Vec<Value>,
    pub verify: VerifyFn,
}

impl WritePlan {
    pub fn new(
        fields: Value,
        diff: Vec<Value>,
        verify: impl Fn(&Snapshot, &Value) -> Vec<Check> + 'static,
    ) -> WritePlan {
        WritePlan {
            fields,
            diff,
            verify: Box::new(verify),
        }
    }
}

pub struct WriteSpec<'a, B> {
    /// operationId from `openapi/operations/*.yaml`, reported in the output.
    pub operation_id: &'a str,
    /// Request class that is sent.
    pub class: &'a str,
    /// Expected response class, or None to accept any response that is not a failure.
    pub expect: Option<&'a str>,
    /// False when only a sibling variant was tested on a real Core: needs `--allow-unverified`.
    pub variant_verified: bool,
    pub flags: WriteFlags,
    /// How long to collect broadcasts after the answer. None means `ctx.settle`.
    pub settle: Option<Duration>,
    pub build: B,
}

// The bound here lets the compiler infer `|snapshot| ...` closures as taking `&Snapshot` for any lifetime.
impl<'a, B: FnOnce(&Snapshot) -> Result<WritePlan>> WriteSpec<'a, B> {
    /// A verified variant that accepts any non-failure response. Adjust the public fields for anything else.
    pub fn new(operation_id: &'a str, class: &'a str, flags: WriteFlags, build: B) -> Self {
        WriteSpec {
            operation_id,
            class,
            expect: None,
            variant_verified: true,
            flags,
            settle: None,
            build,
        }
    }
}

fn broadcast_classes(session: &Session) -> Vec<String> {
    let mut seen: Vec<String> = Vec::new();
    for b in session.broadcasts() {
        let class = b["$type"].as_str().unwrap_or_default().to_string();
        if !seen.contains(&class) {
            seen.push(class);
        }
    }
    seen
}

/// True when a string value (also inside an embedded JSON document) is the redaction placeholder.
fn contains_placeholder(value: &Value) -> bool {
    match value {
        Value::String(s) => {
            s == crate::output::REDACTED
                || (s.starts_with(['{', '[']))
                    && serde_json::from_str::<Value>(s).is_ok_and(|v| contains_placeholder(&v))
        }
        Value::Array(a) => a.iter().any(contains_placeholder),
        Value::Object(m) => m.values().any(contains_placeholder),
        _ => false,
    }
}

/// The write flow. Result: `{"operation", "response", "broadcasts", "verify": {"ok", "checks"}, "state",
/// "state_before"}`. `--dry-run` returns the dry-run document and sends nothing. A failed check is
/// `verify_failed` (exit 8) with the same document in `details`.
pub async fn run_write<B>(ctx: &Ctx, spec: WriteSpec<'_, B>) -> Result<Value>
where
    B: FnOnce(&Snapshot) -> Result<WritePlan>,
{
    let session = open_session(ctx).await?;
    let before = match Snapshot::from_session(&session) {
        Ok(s) => s,
        Err(e) => {
            session.close().await;
            return Err(e);
        }
    };
    let plan = match (spec.build)(&before) {
        Ok(plan) => plan,
        Err(e) => {
            session.close().await;
            return Err(e);
        }
    };
    // A hidden value read back from a redacted `get` must never be written to the Core.
    if contains_placeholder(&plan.fields) {
        session.close().await;
        return Err(CliError::invalid_input(
            "the request contains a redacted placeholder value; read the object again with --show-secrets",
        ));
    }
    let verified =
        catalog::operation(spec.class).is_some_and(|op| op.verified) && spec.variant_verified;
    if spec.flags.dry_run {
        session.close().await;
        return Ok(
            json!({"dry_run": true, "operation": spec.operation_id, "verified": verified,
                         "message": {"class": spec.class, "fields": plan.fields}, "diff": plan.diff}),
        );
    }
    if !verified && !spec.flags.allow_unverified {
        session.close().await;
        return Err(CliError::new(
            "unverified_operation",
            format!(
                "{} is not verified against a real Core: pass --allow-unverified to send it",
                spec.operation_id
            ),
        )
        .with_details(json!({"operation": spec.operation_id, "class": spec.class})));
    }
    let response = match send_request(ctx, &session, spec.class, &plan.fields, spec.expect).await {
        Ok(r) => r,
        Err(mut e) => {
            session.close().await;
            if matches!(
                e.code.as_str(),
                "timeout" | "connection_failed" | "protocol_error"
            ) {
                // The write may have reached the Core: tell the caller to check before retrying.
                e.details =
                    Box::new(json!({"write_sent": "unknown", "operation": spec.operation_id}));
            }
            return Err(e);
        }
    };
    session.wait(spec.settle.unwrap_or(ctx.settle)).await;
    let broadcasts = broadcast_classes(&session);
    session.close().await;

    // A new session shows what the Core really holds, not what our own cache believes.
    // The write was sent and answered: a failure from here on must not look like "nothing happened".
    let sent = |mut e: CliError| {
        e.details = Box::new(
            json!({"write_sent": true, "operation": spec.operation_id, "response": response, "broadcasts": broadcasts}),
        );
        e
    };
    let fresh = open_session(ctx).await.map_err(sent)?;
    let after = Snapshot::from_session(&fresh);
    fresh.close().await;
    let after = after.map_err(sent)?;
    let checks = (plan.verify)(&after, &response);
    let ok = checks.iter().all(|c| c.ok);
    let doc = json!({"operation": spec.operation_id, "response": response, "broadcasts": broadcasts,
                     "verify": {"ok": ok, "checks": checks.iter().map(Check::to_json).collect::<Vec<_>>()},
                     "state": after.counts(), "state_before": before.counts()});
    if ok {
        Ok(doc)
    } else {
        Err(CliError::new(
            "verify_failed",
            "the Core answered, but the expected state was not found afterwards",
        )
        .with_details(doc))
    }
}

/// Counts from `before` and `after` as signed differences, for commands that report what changed.
pub fn count_delta(before: &Value, after: &Value) -> Value {
    let mut out = Map::new();
    for key in [
        "devices",
        "rules_live",
        "rules_total",
        "interfaces",
        "dashboards",
    ] {
        out.insert(
            key.to_string(),
            json!(after[key].as_i64().unwrap_or(0) - before[key].as_i64().unwrap_or(0)),
        );
    }
    Value::Object(out)
}

/// Hex of a frame, for dry runs that show the bytes.
pub fn frame_hex(class: &str, fields: &Value, requests_id: i64) -> Result<String> {
    codec::encode_message(class, fields, requests_id)
        .map(|b| hex(&b))
        .map_err(|e| CliError::invalid_input(format!("cannot encode {class}: {e}")))
}
