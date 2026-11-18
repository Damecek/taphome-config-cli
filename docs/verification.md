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
| Smart rule update | `upsertSmartRules` with the whole stored model re-sent updates an existing rule. Enabling and disabling (only `IsEnabled` changed) and a changed `EvaluationFormula` of a formula rule were accepted: `ErrorCode` 1, `MessageTriggerModelsUpdatedBroadcast`, `ChangesetId` raised, the rest of the model unchanged, no rule errors. | `configuration-model.md` |
| Smart rule codec | The CLI decoded 87 live rules of 12 trigger types (5, 11, 12, 13, 19, 26, 27, 32, 37, 40, 41, 45). Re-encoding each decoded rule reproduced the Core's bytes, so `rule get` followed by `rule apply --dry-run` reported no change for all of them. | `configuration-model.md` |
| Smart rule delete | `deleteSmartRules` leaves a tombstone: `IsDeleted` true, parameters removed, name, type and version kept, changeset raised. | `configuration-model.md` |
| Device delete | `deleteDevice` with `ForceDelete` false works for a variable, a Packet Parser device and (after its child) a Packet Parser module. Freed device ids are reused. | `configuration-model.md` |
| Packet Parser import | `importPacketParserTemplate` (message `MessageHttpApiConfigRequest`) accepts a `ConfigJson` with one HTTP module and one variable device, hand-built or converted from a template XML by the CLI (ProtocolVersion 66, placeholder ids -1 and -2, plus the template keys `Name`, `Id` and `Revision`). The answer is `MessageHttpApiConfigResponse`. The Core allocates both ids, sets the child's `HardwareParentId`, fills defaults and runs the state script at once. | `configuration-model.md` |
| Script debug | `getScriptLastRunValues` returns the last run of a device script, with `Result` as a `DataType` (type id 4 is a double, the bytes are its IEEE-754 bits as int64) and `RunTimestampUtc`. The per-phase variable snapshot list is decoded: phase 0 held the script variable (NaN before its first value), phase 1 was empty. | `configuration-model.md` |

## CLI verification (step 2)

Setup. The tool was the release build of the Rust CLI `taphome`, run on 2026-11-17 against the same Core (software 2025.2.32396, protocol 89) with the owner's approval. All created objects were named `TEST-zz-agent-test-*` and removed afterwards. On macOS an app without the Local Network permission cannot reach the LAN (os error 65, "No route to host"), which also hit the Rust binary. For this run the traffic went through a transparent loopback TCP relay started by an interpreter that has the permission. The protocol behaviour is therefore verified. A direct LAN connection from the binary on that machine is not.

Reads. Every command exited 0 and decoded with no `$errors` and no `$unknown`.

| Command | Observed |
|---|---|
| `auth login`, `auth status` | Login and stored session worked. |
| `sync` | Summary, every section, `--full` and `--out` (file mode 600). |
| `device list`, `device get` | Filters and single devices decoded. |
| `io values` | All 136 devices, 424 values, every value type id has a name. |
| `io errors` | Plain device (origin 0) and Packet Parser module (origin 1), empty lists. |
| `io log` | Entries with `ChangeOrigin` names such as Startup and ByDeviceOrDirectLink. |
| `io watch` | 30 s gave 4 `MessageDeviceValueChangedBroadcast` and 14 `MessageDeviceAggregatedValueChangedBroadcast`. |
| `rule list`, `rule get`, `rule errors` | All 87 live rules, 12 trigger types, each with `TriggerParameters."$type"`. Round trip `rule get` to `rule apply --dry-run` reported no change for all 87. Error lists empty. |
| `pp script get`, `pp script last-run` | 49 Packet Parser devices. The last run included the decoded per-phase snapshot. |
| `raw ops`, `raw call` | `MessageGetBasicCorePropertiesRequest` and `MessageGetUsersListRequest`. |

Writes. Each ran first as `--dry-run`, then for real. Each was answered Ok and passed the CLI's read-back by a fresh sync.

| Command | Observed |
|---|---|
| `device add-virtual --model VirtualVariableGeneral` | `MessageAddDeviceResponse` with `AddedDevice`, `MessageDeviceListUpdatedBroadcast`. |
| `device rename`, `device set-props` (`Name`) | `ErrorCode` 1, `MessageDeviceUpdatedBroadcast`. |
| `pp import` (XML template) | CLI converted the template to `ConfigJson` (ProtocolVersion 66, keys `Name`, `Id`, `Revision` accepted), one `PacketParserModuleHttp` module and one `PacketParserVariable` child. `MessageHttpApiConfigResponse` listed both, the child got the module id as `HardwareParentId`, 2 devices added, one `MessageDeviceListUpdatedBroadcast`. The module did not keep `StayConnected`, reported as a property difference. |
| `pp script last-run` after the import | `Result` double 7, `RunTimestampUtc` set, snapshot decoded. |
| `pp script set` | `ReadStateScriptPacketParser` from "7" to "8": `ErrorCode` 1, `MessageDeviceUpdatedBroadcast`. Four seconds later `last-run` returned 8 and `io values` showed 8. |
| `rule apply` create | Disabled formula rule (type 12, version 13): `ErrorCode` 1, `MessageTriggerModelsUpdatedBroadcast`. The Core reused the id of a soft-deleted rule. |
| `rule enable`, `rule disable` | Whole stored model re-sent with `IsEnabled` changed: `ErrorCode` 1, broadcast, `ChangesetId` raised, rest unchanged, no rule errors. |
| `rule apply` update | Same `Id`, `EvaluationFormula` from "Va" to "Va + 0": stored model equals the sent one, `ChangesetId` raised. |
| `rule delete` | Tombstone, live count back to 87, `MessageTriggerModelsDeletedBroadcast`. |
| `device delete` | Packet Parser child, then the module, then the variable: `ErrorCode` 1 and `MessageDeviceListUpdatedBroadcast` each. |

Rule writes were also followed by `MessageAffectingTriggersInfoSubscribedBroadcast`, and a `MessageDeviceLogSubscribedBroadcast` was seen once after the enable.

Guards confirmed on real data without sending. `device delete` refused the variable while a rule referenced it and refused the module while it had a child. `rule enable` without `--allow-unverified` was refused with `unverified_operation` (this gate was removed afterwards, because enable and disable are now verified).

Final state equal to the baseline: 136 devices, 87 live rules (88 in total), 4 interfaces, 24 dashboards. A full configuration diff showed only the tombstone rule's `ChangesetId` and live aggregated statistics values.

Not verified on a real Core: `device delete --force` and `--allow-referenced`, `device add-virtual` with other models or with properties, `pp import` with TCP, UDP, FTP or MQTT modules, the devices-only form and templates with several devices or `DeviceVariables` placeholders, creating an enabled rule or a rule of another type, updating a rule of a type other than 12, `rule errors` and `io errors` answers with entries, `raw call` writes, and release binaries for Linux and Windows, the release workflow and `install.sh` (no release exists yet).

## Remote access through the cloud, 2026-11-18

Setup. The owner's TapHome account, the same Core (2025.2.32396, protocol 89), first `scripts/probe` with a patched transport, then the release build of `taphome`. Only logins and reads.

| Area | Fact |
|---|---|
| Endpoints | `GET https://cloudapi.taphome.com/api/user/tunnelendpoints3` needs no auth and returns `wss://tunnel1.taphome.com/MobileApp` and `wss://tunnel2.taphome.com/MobileApp`. The relay is an ASP.NET Kestrel server. |
| Routing | The relay forwards the first message to the Core named by its `LocationId`. Login and reconnect messages are the same as on the LAN. |
| Local account | A local account's session reconnected through the tunnel gets `ReconnectResult` 5 (RemoteAccessDisabled). The app says "Remote access is no longer supported for local users" and lets an admin only switch it off for a local user. |
| Cloud account | `login2` returns the location's `authorizationToken`. A `MessageLoginRequest` with `IsLocalAccount` false, the e-mail and the salted hash of the lower-case token Guid gets `LoginResult` 1. The reconnect then works as on the LAN: result 4 with the `DbHash`, then result 1 with the full configuration (158 kB, 136 devices, 88 rules). |
| Relay refusals | The relay answers a reconnect with `MessageGenericResponse` ErrorCode 21 (CoreOffline, "Couldn't connect to desired core") without a request id when it cannot forward it. This happened when a reconnect followed the close of the previous tunnel socket of the same session within about 3 s, and every time `ServerClientDeviceId` carried the cloud client device id instead of 0. A second reconnect on the same tunnel socket made the relay close the TCP connection. |
| CLI | `auth login`, `auth status`, `sync`, `device list` and `rule list` ran through the tunnel. With a stored session each command took about 1 s. |

## Not yet tested

- Writes. Modbus config, TapHome Bus config (terminal mapping), other virtual device models, device actions and outputs on real hardware, users and permissions, zones, dashboards and categories, week plans, MQTT module creation, TCP, UDP and FTP Packet Parser modules, creating an enabled smart rule, creating or updating a rule of a type other than 12, `ForceDelete` true, several objects in one message, backup and restore.
- Persistence across a Core restart.
- Reads. Permission getters returned empty rows, so the row layout is unseen. Error list responses with entries (`getDeviceErrors`, `getSmartRuleErrors` and the CLI's `io errors` and `rule errors` returned only empty lists) and the `getValueHistory` container layout.
- Not observed because nothing changed. `deviceTimersUpdated`, `interfaceUpdated`, `smartRuleErrorStatusChanges` and `moduleUptime`.
- Writes through the cloud tunnel. They use the same messages as on the LAN, but no write was sent through the tunnel yet.
- The TapHomeApi HTTP routes, the cloud API calls other than `login2`, `tunnelendpoints3` and `getlocationsstatus`, UDP and mDNS discovery, session lifetime and the Next line.

## How to add a result

1. Run the operation against a Core with the probe. Writes need the owner's approval first.
2. Add an entry to `openapi/verification.yaml` with `verified`, `date`, `core`, `protocol` and `note`.
3. Run `uv run --with pyyaml scripts/build_openapi.py` to refresh `x-verified`.
4. If the result is about transport or wire behaviour rather than one operation, add a row to the table above.
