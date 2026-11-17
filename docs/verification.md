# Verification status

This page is the single place that records what has been tested against a real TapHome Core. All other documents describe behaviour that was read from the Android app (derived from code) unless they say "confirmed". When a statement elsewhere and this page disagree, this page wins.

## How status is tracked

- Per operation, the status is in `openapi/verification.yaml`. The build script `scripts/build_openapi.py` copies it into the generated spec as `x-verified: true|false` on every operation. An operation id that is missing from the file is not verified.
- Per fact outside the operation list (transport behaviour, wire details, discovery), the status is in the table below.
- Everything not listed here is derived from code and untested.

## Test setup

| Item | Value |
|---|---|
| Date | 2026-11-17 |
| Core software | 2025.2.32396, stable channel |
| Protocol version | 89 (Official line) |
| App version analysed | 2025.2.32375 |
| Tool | `scripts/probe`, with the read-only allowlist described in `docs/reverse-engineering.md` |
| Account | a local account of the owner |

## Confirmed facts

| Area | Fact | Documented in |
|---|---|---|
| HTTP | `GET /` on port 80 returns an HTML status page. The `Server` header is `websocket-sharp/1.0`, so one server answers both HTTP and WebSocket. | `architecture.md` |
| Identify | `ws://<core>/Identify` sends one binary frame that is a bare `LocationInfo`, with no `MessageBase` wrapper. | `architecture.md`, `protocol.md` |
| Identify | `AccessToken` in that frame is masked (16 characters, only the last 4 are readable). `AuthorizationToken` is absent. | `auth.md` |
| Login | A login with a local account over `ws://<core>/Core` works as derived. A `MessageLoginRequest` is answered by a `MessageLoginResponse` with `LoginResult` 1 (Ok) and a `SessionId`. The Core then closes the login socket. | `auth.md` |
| Version | A reconnect with an unknown `SessionId` returns `ReconnectResult` 2 (WrongSession) and no `CurrentProtocolVersion` or `CoreVersionString`. A client cannot read the Core version before it logs in. | `architecture.md`, `auth.md` |
| Sync | A reconnect from a fresh client (empty `DbHash`) returns `ReconnectResult` 4 (FaultyStorageClient) together with the current `DbHash`. The client reconnects with that `DbHash` and receives `ReconnectResult` 1 and the complete configuration in one message. On the test Core that was 136 devices, 4 interfaces named "CCU", 88 smart rules and 24 dashboards. The message decoded with no unknown fields. | `auth.md`, `protocol.md` |
| Acknowledgement | The Core acknowledges each request with a `MessageReceiveAcknowledgement` before it sends the response. The Core numbers its own messages with a running `RequestsId`, acknowledgements included. | `protocol.md` |
| Correlation | The response to a reconnect request echoes the request's `RequestsId`. | `protocol.md` |
| Hierarchy | Class hierarchies are always serialized from the root base class, even when a field is declared with a derived type. A `GenericDevice` in a list is written as the `DeviceBase` fields at the top level plus the `GenericDevice` fields nested under include tag 100. A client that encodes the derived type directly produces garbage. | `protocol.md` |
| Models | Interface models seen are `VirtualCCU`, `TapHomeBusCCU`, `PacketParserCCU` and `MqttCCU`. Device models seen include `TapHomeBus*`, `PacketParser*` and `Virtual*`. | `configuration-model.md` |
| Environment | On macOS, a Python interpreter managed by uv cannot reach the LAN (Local Network privacy, `OSError 65`). The Homebrew interpreter can. | `reverse-engineering.md` |

## Not yet tested

No write operation has been sent to a real Core. This includes device creation, the TapHome Bus configuration message, Packet Parser and Modbus imports, smart rule updates, user and permission changes, backup and the cloud HTTP API. The TapHomeApi routes, UDP and mDNS discovery, session lifetime and the Next line are also untested.

## How to add a result

1. Run the operation against a Core with the probe. Writes need the owner's approval first.
2. Add an entry to `openapi/verification.yaml` with `verified`, `date`, `core`, `protocol` and `note`.
3. Run `uv run --with pyyaml scripts/build_openapi.py` to refresh `x-verified`.
4. If the result is about transport or wire behaviour rather than one operation, add a row to the table above.
