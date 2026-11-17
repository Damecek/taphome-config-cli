//! WebSocket transport: `/Identify`, login on a throwaway `/Core` socket, and [`Session`].
//!
//! A port of `scripts/probe/client.py`. The Core speaks plain `ws://` on the LAN, so there is no TLS.
use crate::errors::{CliError, Result};
use crate::output::from_hex;
use crate::{catalog, codec};
use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::sync::{oneshot, Mutex as AsyncMutex};
use tokio::task::JoinHandle;
use tokio::time::{interval_at, timeout, Instant};
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::tungstenite::{Error as WsError, Message};
use tokio_tungstenite::{connect_async_with_config, MaybeTlsStream, WebSocketStream};

pub const PLATFORM: &str = "Android";
pub const SOFTWARE_VERSION: &str = "2025.2.32375";
pub const DEVICE_NAME: &str = "taphome-config-cli";
pub const PROTOCOL_VERSION: i64 = 89;
/// Acknowledgements exist from Core protocol 23.
pub const ACK_MIN_PROTOCOL: i64 = 23;
const MAX_OTHER_LOGIN_MESSAGES: usize = 10;

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;
type Writer = Arc<AsyncMutex<SplitSink<Ws, Message>>>;

/// The app's timeouts from docs/protocol.md.
#[derive(Clone, Copy, Debug)]
pub struct Timeouts {
    pub connect: Duration,
    pub identify: Duration,
    pub login: Duration,
    pub reconnect: Duration,
    pub request: Duration,
}

impl Timeouts {
    pub fn standard() -> Timeouts {
        Timeouts {
            connect: Duration::from_secs(20),
            identify: Duration::from_secs(7),
            login: Duration::from_secs(15),
            reconnect: Duration::from_secs(20),
            request: Duration::from_secs(60),
        }
    }

    /// `--timeout S` replaces every limit.
    pub fn uniform(limit: Duration) -> Timeouts {
        Timeouts {
            connect: limit,
            identify: limit,
            login: limit,
            reconnect: limit,
            request: limit,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Options {
    pub timeouts: Timeouts,
    pub ack_interval: Duration,
    pub ping_interval: Duration,
}

impl Options {
    pub fn standard() -> Options {
        Options {
            timeouts: Timeouts::standard(),
            ack_interval: Duration::from_secs(4),
            ping_interval: Duration::from_secs(6),
        }
    }
}

fn connection_failed(message: impl Into<String>) -> CliError {
    CliError::new("connection_failed", message)
}

fn timed_out(what: &str) -> CliError {
    CliError::new("timeout", format!("{what} timed out"))
}

fn identity(device_id: &str) -> Value {
    json!({"Platform": PLATFORM, "SoftwareVersion": SOFTWARE_VERSION, "DeviceId": device_id,
           "ClientDeviceName": DEVICE_NAME, "ClientProtocolVersion": PROTOCOL_VERSION})
}

fn merge(base: Value, extra: Value) -> Value {
    let mut map = base.as_object().cloned().unwrap_or_default();
    map.extend(extra.as_object().cloned().unwrap_or_default());
    Value::Object(map)
}

async fn connect_ws(host: &str, path: &str, limit: Duration) -> Result<Ws> {
    // The reconnect response is several hundred kB: lift tungstenite's default 64 MiB and 16 MiB limits.
    let config = WebSocketConfig::default()
        .max_message_size(None)
        .max_frame_size(None);
    match timeout(
        limit,
        connect_async_with_config(format!("ws://{host}{path}"), Some(config), false),
    )
    .await
    {
        Err(_) => Err(timed_out("connecting to the Core")),
        Ok(Err(WsError::Url(_))) => Err(CliError::new(
            "missing_config",
            "TAPHOME_HOST is not a valid host or host:port",
        )),
        Ok(Err(e)) => Err(connection_failed(format!(
            "cannot connect to the Core: {e}"
        ))),
        Ok(Ok((ws, _))) => Ok(ws),
    }
}

/// Next binary frame, skipping pings, pongs and text.
async fn next_binary(ws: &mut Ws) -> Result<Vec<u8>> {
    loop {
        match ws.next().await {
            Some(Ok(Message::Binary(data))) => return Ok(data.to_vec()),
            Some(Ok(Message::Close(_))) | None => {
                return Err(connection_failed("the Core closed the connection"))
            }
            Some(Ok(_)) => {}
            Some(Err(e)) => return Err(connection_failed(format!("connection error: {e}"))),
        }
    }
}

/// One frame from `ws://host/Identify`, decoded as a bare `LocationInfo`.
pub async fn identify(host: &str, timeouts: &Timeouts) -> Result<Value> {
    let mut ws = connect_ws(host, "/Identify", timeouts.connect).await?;
    let data = timeout(timeouts.identify, next_binary(&mut ws))
        .await
        .map_err(|_| timed_out("the identify request"))??;
    let _ = ws.close(None).await;
    codec::decode("LocationInfo", &data)
        .map_err(|e| CliError::protocol(format!("undecodable identify frame: {e}")))
}

/// Inner messages of a container, or the message itself.
fn unwrap_container(msg: Value) -> Result<Vec<Value>> {
    if msg["$type"] != "MessageContainer" {
        return Ok(vec![msg]);
    }
    msg["Messages"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .map(|m| {
            let bytes = m
                .as_str()
                .and_then(from_hex)
                .ok_or_else(|| CliError::protocol("bad container element"))?;
            codec::decode_message(&bytes)
                .map_err(|e| CliError::protocol(format!("undecodable container element: {e}")))
        })
        .collect()
}

/// Login on a throwaway `/Core` socket. Returns the decoded `MessageLoginResponse`.
pub async fn login(
    host: &str,
    location_id: &str,
    user: &str,
    password_hash: &str,
    device_id: &str,
    timeouts: &Timeouts,
) -> Result<Value> {
    let fields = merge(
        json!({"LocationId": location_id, "LoginOrEmail": user.to_lowercase(),
               "PasswordHashOrAuthorizationToken": password_hash, "ServerClientDeviceId": 0, "IsLocalAccount": true}),
        identity(device_id),
    );
    let exchange = async {
        let mut ws = connect_ws(host, "/Core", timeouts.connect).await?;
        let frame = codec::encode_message("MessageLoginRequest", &fields, 0)
            .map_err(|e| CliError::internal(e.to_string()))?;
        ws.send(Message::Binary(frame.into()))
            .await
            .map_err(|e| connection_failed(format!("cannot send the login: {e}")))?;
        let mut others = 0;
        loop {
            let data = next_binary(&mut ws).await?;
            let msg = codec::decode_message(&data)
                .map_err(|e| CliError::protocol(format!("undecodable login frame: {e}")))?;
            for m in unwrap_container(msg)? {
                if m["$type"] == "MessageLoginResponse" {
                    let _ = ws.close(None).await;
                    return Ok(m);
                }
                others += 1;
            }
            if others > MAX_OTHER_LOGIN_MESSAGES {
                return Err(CliError::protocol(
                    "no login response after 10 other messages",
                ));
            }
        }
    };
    timeout(timeouts.login, exchange)
        .await
        .map_err(|_| timed_out("the login"))?
}

// ---------------------------------------------------------------- session

#[derive(Default)]
struct State {
    pending: HashMap<i64, oneshot::Sender<Result<Value>>>,
    next_id: i64,
    broadcasts: Vec<Value>,
    unmatched: Vec<Value>,
    to_ack: Vec<i64>,
    protocol: i64,
    closed: Option<CliError>,
}

type Shared = Arc<Mutex<State>>;

fn lock(shared: &Shared) -> std::sync::MutexGuard<'_, State> {
    shared
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Marks the session closed and fails every pending request at once.
fn close_with(shared: &Shared, error: CliError) {
    let mut st = lock(shared);
    st.closed.get_or_insert_with(|| error.clone());
    for (_, tx) in st.pending.drain() {
        let _ = tx.send(Err(error.clone()));
    }
}

fn message_kind(msg: &Value) -> Option<&'static str> {
    msg["$type"].as_str().and_then(catalog::message_type)
}

fn dispatch(shared: &Shared, msg: Value) {
    let mut st = lock(shared);
    match message_kind(&msg) {
        Some(kind @ ("Response" | "Progress")) => {
            let mut rid = msg["RequestsId"].as_i64().unwrap_or(0);
            if msg["$type"] == "MessageReconnectResponse" {
                rid = 1;
            } else if rid == -1 {
                if let Some(min) = st.pending.keys().min() {
                    rid = *min;
                }
            }
            if kind == "Response" {
                if let Some(tx) = st.pending.remove(&rid) {
                    let _ = tx.send(Ok(msg));
                    return;
                }
            }
            st.unmatched.push(msg);
        }
        Some("Broadcast") => st.broadcasts.push(msg),
        _ => st.unmatched.push(msg),
    }
}

async fn read_loop(shared: Shared, writer: Writer, mut stream: SplitStream<Ws>) {
    let error = loop {
        let data = match stream.next().await {
            Some(Ok(Message::Binary(data))) => data,
            Some(Ok(Message::Close(_))) | None => {
                break connection_failed("the Core closed the connection")
            }
            Some(Ok(_)) => continue,
            Some(Err(e)) => break connection_failed(format!("connection error: {e}")),
        };
        let msg = match codec::decode_message(&data) {
            Ok(m) => m,
            Err(e) => break CliError::protocol(format!("undecodable frame from the Core: {e}")),
        };
        // The reconnect response arrives before the protocol is known: its stamp waits for the next tick.
        if !matches!(message_kind(&msg), Some("Acknowledgement" | "Stream")) {
            lock(&shared).to_ack.push(codec::ack_stamp(&msg));
        }
        match unwrap_container(msg) {
            Ok(inner) => inner.into_iter().for_each(|m| dispatch(&shared, m)),
            Err(e) => break e,
        }
    };
    close_with(&shared, error);
    let _ = timeout(Duration::from_secs(1), async {
        writer.lock().await.close().await
    })
    .await;
}

async fn flush_acks(shared: &Shared, writer: &Writer) {
    let stamps = {
        let mut st = lock(shared);
        if st.protocol < ACK_MIN_PROTOCOL || st.to_ack.is_empty() {
            return;
        }
        std::mem::take(&mut st.to_ack)
    };
    if let Ok(frame) = codec::encode_message(
        "MessageReceiveAcknowledgement",
        &json!({"ReceivedMessagesStamps": stamps}),
        0,
    ) {
        let _ = writer
            .lock()
            .await
            .send(Message::Binary(frame.into()))
            .await;
    }
}

async fn keep_alive(shared: Shared, writer: Writer, opts: Options) {
    let start = Instant::now();
    let mut acks = interval_at(start + opts.ack_interval, opts.ack_interval);
    let mut pings = interval_at(start + opts.ping_interval, opts.ping_interval);
    loop {
        tokio::select! {
            _ = acks.tick() => flush_acks(&shared, &writer).await,
            _ = pings.tick() => {
                let _ = writer.lock().await.send(Message::Ping(Vec::new().into())).await;
            }
        }
        if lock(&shared).closed.is_some() {
            return;
        }
    }
}

/// The working `/Core` connection: reconnect handshake, request/response matching by `RequestsId`,
/// acknowledgements every 4 s, WebSocket pings every 6 s and collected broadcasts.
pub struct Session {
    shared: Shared,
    writer: Writer,
    tasks: Vec<JoinHandle<()>>,
    /// The decoded `MessageReconnectResponse`. `ReconnectResultId` 1 means a usable session.
    pub reconnect: Value,
}

impl Session {
    /// Opens a socket and sends the reconnect request (`RequestsId` 1). The caller inspects
    /// `reconnect["ReconnectResultId"]`: 4 (stale `DbHash`) and 2 (wrong session) need a new session or login.
    pub async fn connect(
        host: &str,
        location_id: &str,
        user: &str,
        session_id: &str,
        db_hash: Option<&str>,
        device_id: &str,
        opts: Options,
    ) -> Result<Session> {
        let ws = connect_ws(host, "/Core", opts.timeouts.connect).await?;
        let (sink, stream) = ws.split();
        let writer: Writer = Arc::new(AsyncMutex::new(sink));
        let shared: Shared = Arc::new(Mutex::new(State {
            next_id: 1,
            ..State::default()
        }));
        let tasks = vec![
            tokio::spawn(read_loop(shared.clone(), writer.clone(), stream)),
            tokio::spawn(keep_alive(shared.clone(), writer.clone(), opts)),
        ];
        let mut session = Session {
            shared,
            writer,
            tasks,
            reconnect: Value::Null,
        };
        let mut fields = merge(
            json!({"LocationId": location_id, "Login": user.to_lowercase(), "SessionId": session_id,
                   "IsLocalAccount": true, "ServerClientDeviceId": 0}),
            identity(device_id),
        );
        if let Some(hash) = db_hash.filter(|h| !h.is_empty()) {
            fields["DbHash"] = json!(hash);
        }
        session.reconnect = session
            .request("MessageReconnectRequest", &fields, opts.timeouts.reconnect)
            .await?;
        lock(&session.shared).protocol = session.reconnect["CurrentProtocolVersion"]
            .as_i64()
            .unwrap_or(0);
        Ok(session)
    }

    /// Sends one request and waits for its response. After the session closed (socket closed or
    /// undecodable frame) every call fails at once instead of waiting for the timeout.
    pub async fn request(&self, class: &str, fields: &Value, limit: Duration) -> Result<Value> {
        let (rid, frame, rx) = {
            let mut st = lock(&self.shared);
            if let Some(e) = &st.closed {
                return Err(e.clone());
            }
            let rid = st.next_id;
            let frame = codec::encode_message(class, fields, rid)
                .map_err(|e| CliError::invalid_input(format!("cannot encode {class}: {e}")))?;
            st.next_id += 1;
            let (tx, rx) = oneshot::channel();
            st.pending.insert(rid, tx);
            (rid, frame, rx)
        };
        if let Err(e) = self
            .writer
            .lock()
            .await
            .send(Message::Binary(frame.into()))
            .await
        {
            let error = connection_failed(format!("cannot send the request: {e}"));
            close_with(&self.shared, error.clone());
            return Err(error);
        }
        match timeout(limit, rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(lock(&self.shared)
                .closed
                .clone()
                .unwrap_or_else(|| connection_failed("the session ended"))),
            Err(_) => {
                lock(&self.shared).pending.remove(&rid);
                Err(timed_out(&format!("the {class}")))
            }
        }
    }

    pub fn protocol(&self) -> i64 {
        lock(&self.shared).protocol
    }

    pub fn is_closed(&self) -> bool {
        lock(&self.shared).closed.is_some()
    }

    /// Broadcasts received so far, in arrival order.
    pub fn broadcasts(&self) -> Vec<Value> {
        lock(&self.shared).broadcasts.clone()
    }

    /// Responses nobody waited for (acknowledgements, late answers, progress messages).
    pub fn unmatched(&self) -> Vec<Value> {
        lock(&self.shared).unmatched.clone()
    }

    pub async fn wait(&self, duration: Duration) {
        tokio::time::sleep(duration).await;
    }

    /// Flushes pending acknowledgements, sends a close frame and stops the background tasks.
    pub async fn close(self) {
        flush_acks(&self.shared, &self.writer).await;
        let _ = timeout(Duration::from_secs(1), async {
            self.writer.lock().await.close().await
        })
        .await;
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.tasks.iter().for_each(JoinHandle::abort);
    }
}
