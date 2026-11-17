"""Async TapHome Core client (ws://<host>/Core, /Identify). Needs only the `websockets` package."""
from __future__ import annotations

import asyncio
import secrets
import time
import uuid
from pathlib import Path

import websockets

from . import codec

ROOT = Path(__file__).resolve().parents[2]
PLATFORM = "Android"
SOFTWARE_VERSION = "2025.2.32375"
DEVICE_NAME = "taphome-config-cli-probe"
PROTOCOL_VERSION = 89
ACK_INTERVAL = 4.0
ACK_MIN_PROTOCOL = 23


def device_id() -> str:
    """Fixed random-looking id, generated once into work/secrets/device-id.txt."""
    p = ROOT / "work" / "secrets" / "device-id.txt"
    if not p.exists():
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text(secrets.token_hex(8) + "\n")
    return p.read_text().strip()


def _identity() -> dict:
    return {"Platform": PLATFORM, "SoftwareVersion": SOFTWARE_VERSION, "DeviceId": device_id(),
            "ClientDeviceName": DEVICE_NAME, "ClientProtocolVersion": PROTOCOL_VERSION}


def _connect(host: str, path: str):
    # No compression (the app proposes none), unlimited frame size (the reconnect response is large),
    # library-level ping/pong every 6 s like the app.
    return websockets.connect("ws://%s%s" % (host, path), compression=None, max_size=None,
                              ping_interval=6, ping_timeout=10, open_timeout=20)


async def identify(host: str, raw: list = None) -> dict:
    """Open /Identify, read one frame, decode it as LocationInfo. Raw bytes are appended to `raw`."""
    async with _connect(host, "/Identify") as ws:
        data = await asyncio.wait_for(ws.recv(), 7)
    if raw is not None:
        raw.append(data)
    return codec.decode("LocationInfo", data)


def _unwrap(msg: dict) -> list:
    if msg["$type"] == "MessageContainer":
        return [codec.decode_message(bytes.fromhex(m)) for m in msg.get("Messages", [])]
    return [msg]


async def login(host: str, location_id: str, user: str, password_hash: str, raw: list = None) -> dict:
    """Login on a throwaway /Core socket. Returns the decoded MessageLoginResponse."""
    req = {"LocationId": location_id, "LoginOrEmail": user.lower(), "PasswordHashOrAuthorizationToken": password_hash,
           "ServerClientDeviceId": 0, "IsLocalAccount": True, **_identity()}
    async with _connect(host, "/Core") as ws:
        await ws.send(codec.encode_message("MessageLoginRequest", req, 0))
        deadline = time.monotonic() + 15
        others = 0
        while True:
            data = await asyncio.wait_for(ws.recv(), max(0.1, deadline - time.monotonic()))
            if raw is not None:
                raw.append(data)
            for msg in _unwrap(codec.decode_message(data)):
                if msg["$type"] == "MessageLoginResponse":
                    return msg
                others += 1
            if others > 10:
                raise RuntimeError("no MessageLoginResponse after 10 other messages")


class Session:
    """Main /Core connection: reconnect handshake, request/response matching, acks, broadcasts."""

    def __init__(self, host: str, location_id: str, user: str, session_id: str):
        self.host, self.location_id, self.user, self.session_id = host, location_id, user, session_id
        self.reconnect: dict = {}
        self.broadcasts: list = []      # decoded broadcast messages, in arrival order
        self.unmatched: list = []       # responses nobody waited for
        self.raw: list = []             # every received frame
        self._pending: dict = {}
        self._next_id = 1
        self._to_ack: list = []
        self._protocol = 0
        self._tasks: list = []

    async def __aenter__(self):
        self._ws = await _connect(self.host, "/Core").__aenter__()
        self._tasks = [asyncio.ensure_future(self._reader()), asyncio.ensure_future(self._acker())]
        fields = {"LocationId": self.location_id, "Login": self.user, "SessionId": self.session_id,
                  "IsLocalAccount": True, "ServerClientDeviceId": 0, **_identity()}
        self.reconnect = await self.request("MessageReconnectRequest", fields, 20)
        self._protocol = self.reconnect.get("CurrentProtocolVersion", 0)
        return self

    async def __aexit__(self, *exc):
        try:
            await self._flush_acks()
        except Exception:
            pass
        for t in self._tasks:
            t.cancel()
        await self._ws.close()

    async def request(self, class_name: str, fields: dict, timeout: float = 60) -> dict:
        rid = self._next_id
        self._next_id += 1
        fut = asyncio.get_event_loop().create_future()
        self._pending[rid] = fut
        await self._ws.send(codec.encode_message(class_name, fields, rid))
        try:
            return await asyncio.wait_for(fut, timeout)
        finally:
            self._pending.pop(rid, None)

    async def wait(self, seconds: float):
        await asyncio.sleep(seconds)

    async def _reader(self):
        try:
            async for data in self._ws:
                self.raw.append(data)
                msg = codec.decode_message(data)
                if self._needs_ack(msg):
                    self._to_ack.append(codec._codec().stamp(msg))
                for m in _unwrap(msg):
                    self._dispatch(m)
        except Exception as e:  # closed socket or undecodable frame (raw frames stay in self.raw)
            err = ConnectionError("connection ended: %r" % (e,))
            for fut in self._pending.values():
                if not fut.done():
                    fut.set_exception(err)

    def _needs_ack(self, msg: dict) -> bool:
        # The reconnect response arrives before _protocol is known: it is acked at the next tick.
        kind = codec._codec().message_type.get(msg["$type"])
        return kind not in ("Acknowledgement", "Stream")

    def _dispatch(self, m: dict):
        kind = codec._codec().message_type.get(m["$type"])
        if kind in ("Response", "Progress"):
            rid = m.get("RequestsId", 0)
            if m["$type"] == "MessageReconnectResponse":
                rid = 1
            elif rid == -1 and self._pending:
                rid = min(self._pending)
            fut = self._pending.get(rid)
            if fut and not fut.done():
                if kind == "Response":
                    fut.set_result(m)
                    return
            self.unmatched.append(m)
        elif kind == "Broadcast":
            self.broadcasts.append(m)
        else:
            self.unmatched.append(m)

    async def _flush_acks(self):
        if self._to_ack and self._protocol >= ACK_MIN_PROTOCOL:
            stamps, self._to_ack = self._to_ack, []
            await self._ws.send(codec.encode_message("MessageReceiveAcknowledgement",
                                                      {"ReceivedMessagesStamps": stamps}, 0))

    async def _acker(self):
        while True:
            await asyncio.sleep(ACK_INTERVAL)
            await self._flush_acks()


def new_session_id() -> str:
    return str(uuid.uuid4())
