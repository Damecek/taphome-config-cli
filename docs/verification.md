# Verification status

This page is the single place that records what has been tested against a real TapHome Core. All other documents describe behaviour that was read from the Android app (derived from code) unless they say "confirmed". When a statement elsewhere and this page disagree, this page wins.

## How status is tracked

- Per operation, the status is in `openapi/verification.yaml`. The build script `scripts/build_openapi.py` copies it into the generated spec as `x-verified: true|false` on every operation. An operation id that is missing from the file is not verified.
- Per fact outside the operation list (transport behaviour, wire details, discovery), the status is in the table below.
- Everything not listed here is derived from code and untested.

Counts in the generated spec (`openapi/taphome-core.yaml`, 2026-11-17): 54 of 274 operations have `x-verified: true`. That is 47 of 186 WebSocket message paths, 6 of 61 broadcast webhooks and 1 of 27 real HTTP endpoints. One verified path can stand for several manifest operations that share a request class. The `x-verification` note of each path says which variant was exercised.

## Test setup

| Item | Value |
|---|---|
| Date | 2026-11-17 |
| Core software | 2025.2.32396, stable channel |
| Protocol version | 89 (Official line) |
| App version analysed | 2025.2.32375 |
| Tool | `scripts/probe`, read-only by default (see `docs/reverse-engineering.md`). The write tests were sent with the owner's approval. |
| Write tests | Objects named `TEST-zz-agent-test-*`. Every object was removed afterwards and the final configuration compared equal to the baseline. Details are in `work/notes/09-live-write.md` (not committed). |
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
| Device list | The full device and interface list arrives in the reconnect response, after the second reconnect with the `DbHash`. `getDeviceListOnLogin` is therefore a view of the reconnect response. The login response was not checked for devices. | `configuration-model.md` |
| Passive broadcasts | `MessageDeviceValueChangedBroadcast` and `MessageDeviceAggregatedValueChangedBroadcast` (about one every 2 s) arrive on an idle session with no subscription. The aggregated one has no request that enables it. | `protocol.md` |
| TapHomeApi messages | `MessageTapHomeApiLocationInfoRequest`, `...DiscoveryRequest`, `...GetDeviceValueRequest` and `...GetMultipleDeviceValuesRequest` (AllDevices true) got no answer within 60 s on a client session, which stayed usable. They are tunnel messages that the cloud sends. | `architecture.md` |
| Timestamps | `...TUtc` int64 fields are .NET ticks. `ValueLog.ChangedOnTHUtc` and the `TapHomeTime` fields are seconds since 2016-01-01 UTC. The `Current Time` core property is seconds since 2000-01-01 and equals the Core's local wall-clock time. | `protocol.md` |
| Value types | The `ValueLogType` ids in `configuration-model.md` match the declaration order of the catalog (for example 48 SwitchState, 5 RealTemperature, 62 VariableState, 44 ReedContact). | `configuration-model.md` |
| Core properties | `getBasicCoreProperties` returns about 20 named properties, all strings except `Current Time` (uint32). | `configuration-model.md` |
| Write commit | Every write took effect when the Core answered and showed in a fresh sync from a new socket seconds later. There is no commit step and no restart. Each write answered with `MessageGenericResponse` `ErrorCode` 1 (or a typed response) and one matching broadcast. | `protocol.md` |
| Add virtual variable | `MessageAddDeviceRequest` with `DeviceName`, `Icon` 0, `Interface` "Variable", `Model` "VirtualVariableGeneral" and `DeviceTypeId` 0 creates a device. The Core picks the id, the virtual CCU, a `VIRT-<guid>` serial and the default properties. | `configuration-model.md` |
| Rename and edit | `updateMultipleDevicesProperties` with `{"Name": "<new name>"}` renames a device. `{"ReadStateScriptPacketParser": "8"}` replaced a Packet Parser script, which ran at once with no restart. | `configuration-model.md` |
| Smart rule create | `upsertSmartRules` with `Id` 0 and a formula rule (type 12, version 13, disabled) is accepted. The Core fills `TriggerInputs` and `Priorities` and keeps `IsEnabled`. It reuses the id of a soft-deleted rule, so new ids are not max plus 1. | `configuration-model.md` |
| Smart rule delete | `deleteSmartRules` leaves a tombstone: `IsDeleted` true, parameters removed, name, type and version kept, changeset raised. | `configuration-model.md` |
| Device delete | `deleteDevice` with `ForceDelete` false works for a variable, a Packet Parser device and (after its child) a Packet Parser module. Freed device ids are reused. | `configuration-model.md` |
| Packet Parser import | `importPacketParserTemplate` (message `MessageHttpApiConfigRequest`) accepts a hand-built `ConfigJson` with one HTTP module and one variable device. The Core allocates both ids, sets the child's `HardwareParentId`, fills defaults and runs the state script at once. | `configuration-model.md` |
| Script debug | `getScriptLastRunValues` returns the last run of a device script, with `Result` as a `DataType` (type id 4 is a double, the bytes are its IEEE-754 bits as int64) and `RunTimestampUtc`. | `configuration-model.md` |

## Not yet tested

- Writes. Modbus config, TapHome Bus config (terminal mapping), other virtual device models, device actions and outputs on real hardware, users and permissions, zones, dashboards and categories, week plans, MQTT module creation, TCP, UDP and FTP Packet Parser modules, updating or enabling an existing smart rule, other rule types, `ForceDelete` true, several objects in one message, backup and restore.
- Persistence across a Core restart.
- Reads. Permission getters returned empty rows, so the row layout is unseen. Error list responses with entries, the `getValueHistory` container layout and the `getScriptLastRunValues` snapshot list.
- Not observed because nothing changed. `deviceTimersUpdated`, `interfaceUpdated`, `smartRuleErrorStatusChanges` and `moduleUptime`.
- The TapHomeApi HTTP routes, the cloud API, UDP and mDNS discovery, session lifetime and the Next line.

## How to add a result

1. Run the operation against a Core with the probe. Writes need the owner's approval first.
2. Add an entry to `openapi/verification.yaml` with `verified`, `date`, `core`, `protocol` and `note`.
3. Run `uv run --with pyyaml scripts/build_openapi.py` to refresh `x-verified`.
4. If the result is about transport or wire behaviour rather than one operation, add a row to the table above.
