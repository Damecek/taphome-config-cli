# taphome CLI reference

`taphome` configures a TapHome Core over its WebSocket protocol. The README has install steps and a short tour. This page lists every command and the rules behind them. The source of truth is the code in `src/` and the output of `taphome <command> --help`.

## Contents

- [Conventions](#conventions)
- [Output envelope, errors and exit codes](#output-envelope-errors-and-exit-codes)
- [Redaction](#redaction)
- [Commands](#commands)
- [The write flow](#the-write-flow)
- [Variants gated as unverified](#variants-gated-as-unverified)
- [Packet Parser import formats](#packet-parser-import-formats)
- [Rule file format](#rule-file-format)
- [Configuration](#configuration)
- [Verified against a real Core](#verified-against-a-real-core)

## Conventions

Global flags work on every command and may appear anywhere.

| Flag | Meaning |
|---|---|
| `--pretty` | Indent the JSON output. |
| `--show-secrets` | Show property passwords and tokens. The session id, the password hash and the salt stay hidden. |
| `--timeout <S>` | Limit in seconds (above 0, up to 86400) for connecting and for every reply. It replaces all built-in limits. The built-in limits are 20 s to connect, 7 s for `/Identify`, 15 s for login, 20 s for reconnect and 60 s for a request. |

Every write command also takes `--dry-run` and `--allow-unverified`. `--help` and `--version` print plain text and exit with 0. Everything else prints exactly one JSON document on stdout.

Devices and rules are addressed by integer id or by exact name. A name that matches no object fails with `not_found`, and a name that matches more than one fails with `ambiguous`. Only live rules count, so deleted rules (tombstones) never match. Interfaces (`--ccu`) work the same way with their `CcuId`.

Every command that talks to the Core first opens a session: it reuses the stored session, or logs in when there is none, and it starts with the reconnect that delivers the whole configuration (the "sync"). Reads that say "from the sync" use that data and send nothing else.

## Output envelope, errors and exit codes

Success:

```json
{"ok": true, "command": "device list", "data": {}, "warnings": []}
```

Failure:

```json
{"ok": false, "command": "rule apply", "error": {"code": "invalid_input", "message": "...", "core_error_code": null, "details": {}}}
```

`command` is the subcommand path, for example `pp script set`. A usage error (unknown flag, missing argument) is a failure with `error.code` equal to `usage` and exit code 2. The `warnings` list carries notes that do not stop the command, for example that a device name is already in use.

When the Core answered with an error, `core_error_code` is its numeric `ErrorCode`. `details` holds extra facts, such as the ids of children that block a delete. A failure after a write was sent carries `details.write_sent`. It is `"unknown"` after a timeout or a dropped connection (the Core may have applied the change, so read before retrying) and `true` when the Core answered but the follow-up check could not finish.

### Exit codes

| Exit | Meaning | Error codes |
|---|---|---|
| 0 | Success | |
| 1 | Internal error | `internal` |
| 2 | Usage or invalid input | `usage`, `invalid_input` |
| 3 | Authentication or configuration | `missing_config`, `login_failed`, `session_invalid`, `salt_missing` |
| 4 | Connection or timeout | `connection_failed`, `timeout`, `core_not_ready`, `protocol_error` |
| 5 | The Core rejected the request | the Core error names below, `not_handled` |
| 6 | Object not found or ambiguous (local lookup) | `not_found`, `ambiguous` |
| 7 | Refused by a CLI safety gate | `unverified_operation`, `write_not_allowed`, `roundtrip_mismatch`, and `already_exist` from `rule apply`, `device add-virtual` and `pp import` |
| 8 | Post-write verification failed | `verify_failed` |

Three cases deviate from the table. `core_offline` from the cloud relay during the connect exits with 4. `unauthorized` with the message "the user has no remote access" exits with 3. `already_exist` from `rule apply` (a live rule with that name exists), `device add-virtual` and `pp import` (a device with that name exists) exits with 7, while the same code from the Core exits with 5.

### Error codes

CLI codes:

| Code | When |
|---|---|
| `usage` | The command line does not parse. |
| `invalid_input` | A file, id, key or value is wrong. The message says which. |
| `missing_config` | A required `TAPHOME_*` variable is missing, a `_FILE` cannot be read, `TAPHOME_HOST` is not a valid host, both `TAPHOME_HOST` and `TAPHOME_CLOUD_EMAIL` are set, or no config directory can be found (no `HOME` and no `TAPHOME_CONFIG_DIR`). |
| `login_failed` | The Core refused the login. `details.login_result` names the reason, for example `WrongPassword`. In cloud mode also when `cloudapi.taphome.com` refuses the TapHome account (`details.http_status`). |
| `session_invalid` | The stored session was rejected and no password is available to log in again. |
| `salt_missing` | Login needs the salt and the binary has none (see [Salt lookup](#salt-lookup)). |
| `connection_failed` | The socket could not be opened or broke. On macOS, "No route to host" usually means the Local Network permission is missing. |
| `timeout` | A step exceeded its limit. |
| `core_not_ready` | The Core is still starting. Try again in about 10 seconds. |
| `core_offline` | Cloud mode. The relay kept answering CoreOffline (ErrorCode 21) for about 20 seconds after the CLI retried. The Core is probably not connected to the cloud. Exits with 4. The same name from a Core answer to a request exits with 5. |
| `protocol_error` | An unexpected answer, for example a wrong response class. |
| `not_found`, `ambiguous` | A local lookup of a device, rule or interface. |
| `unverified_operation` | The operation or variant was never tested against a real Core and `--allow-unverified` is missing. |
| `write_not_allowed` | `raw call` needs `--allow-write`. |
| `not_handled` | The Core answered without error but did not handle the request. |
| `roundtrip_mismatch` | A stored rule does not survive decode and encode unchanged, so an update would lose data. Nothing was sent. |
| `verify_failed` | The Core answered, but the expected state was not found afterwards. `details` holds the full result. |
| `internal` | A bug. Set `TAPHOME_DEBUG=1` to print details on stderr. |

Core error codes are the names of the protocol's `CommonErrorCodes` in snake case: `unauthorized`, `exception`, `parameter_too_short_or_long`, `already_exist`, `does_not_exist`, `invalid_request`, `missing_email`, `wrong_email_format`, `missing_password`, `password_too_short`, `wrong_email_or_password`, `missing_parameter`, `already_in_progress`, `deserialization_exception`, `incompatible_core_smart_rule_version`, `update_started`, `update_finished`, `update_failed`, `at_least_one_admin`, `core_offline`, `request_not_supported`, `password_does_not_meet_requirements`, `user_is_placeholder`, `cancelled`, `requires_core_update`, `not_enough_space` and `missing_data`. A number outside this list becomes `core_error`. They all exit with 5. `error.message` is the Core's own text when it sent one.

## Redaction

All output goes through one redactor, including `--dry-run`, `sync --out` and error details.

- The keys `PasswordHashOrAuthorizationToken`, `SessionId`, `NewPasswordHash`, `OldPasswordHash`, `PinCodeHash`, `CardCodeHash` and any key that ends with `PasswordHash` are always replaced with `<redacted>`.
- The keys `AccessToken`, `AuthorizationToken`, `Token` and `Passcode` are replaced unless `--show-secrets` is set.
- Inside any map whose key ends in `Properties` or `Parameters` (for example `DeviceProperties`), keys that contain "password" or "token" in any case are replaced unless `--show-secrets` is set.
- A `ConfigJson` string (a Packet Parser configuration) is parsed, redacted the same way and written back as text.
- Every secret the process knows (the password, the password hash, the salt, the session id) is replaced wherever it appears as a whole string value.
- Empty values are left as they are, so an empty password stays visible as empty.
- A write never sends the placeholder. If any string in the request fields (also inside an embedded JSON document) is `<redacted>`, the command fails with `invalid_input` and the message "the request contains a redacted placeholder value; read the object again with --show-secrets". This protects `rule get` followed by `rule apply` and `device get` followed by `device set-props`.

Diffs of `device set-props` and `pp import` mask password and token values in the same way.

## Commands

"Read" means the command changes nothing on the Core. Message classes and operation ids come from the operation registry, which `taphome raw ops` prints.

### auth

| Command | What it does | `data` |
|---|---|---|
| `auth login` | LAN: needs `TAPHOME_HOST`, `TAPHOME_USER` and `TAPHOME_PASSWORD`. Reads the location from `/Identify`, sends `MessageLoginRequest` (`login`) on a throwaway socket and stores the session. Cloud: needs `TAPHOME_CLOUD_EMAIL` and `TAPHOME_CLOUD_PASSWORD` (and `TAPHOME_LOCATION` when the account has several locations). Calls `login2` and `tunnelendpoints3` on `cloudapi.taphome.com`, then logs in through the tunnel with the location's authorization token. | `user`, `location` (the location name), `session_stored` |
| `auth status` | Local read of the stored session. If one exists, it also reconnects to ask whether the Core still accepts it. | `stored`, `valid`, `core_version`, `protocol`, `user`, `host_configured`, `salt_source` (`env`, `built-in` or `missing`). `host` appears only with `--show-secrets`. |
| `auth logout` | Deletes `session.json`. Does not contact the Core. | `session_removed` |

### sync

`taphome sync [--full | --section devices|interfaces|rules|dashboards|core] [--out FILE]` is a read. It sends the reconnect request (`MessageReconnectRequest`, which also carries `getDeviceListOnLogin` and `listSmartRules`).

Without flags it prints a summary: `core_version`, `protocol`, `counts` and `interfaces`. `counts` has `devices`, `rules_live`, `rules_total`, `interfaces` and `dashboards`. Each interface entry has `id`, `model` and `name`. Check `counts` before and after a write.

`--section` prints one part: `devices`, `interfaces`, `rules` (every rule with its decoded `model`, tombstones included), `dashboards` or `core` (`core_version`, `protocol`, `counts`, `location`, `user`, `trigger_versions`, `core_properties`). `--full` prints all five. `--full` and `--section` cannot be combined. `--out FILE` writes the (redacted) document with mode 600, defaults to the full document when no section is chosen, and prints the summary plus `out` and `out_bytes`.

### device

| Command | Kind | Sends | `data` |
|---|---|---|---|
| `device list [--model GLOB] [--name SUBSTR] [--parent ID] [--ccu ID]` | read, from the sync | nothing | `count`, `devices`: rows with `ID`, `Name`, `Model`, `Interface`, `CcuId`, `HardwareParentId`, `SerialNumber`, `DeviceType` |
| `device get <DEVICE>` | read, from the sync | nothing | `device`: the full decoded device with properties, current values and capabilities |
| `device set-props <DEVICE> --prop K=V... [--props-file F] [--allow-new-key]` | write | `updateMultipleDevicesProperties`, `MessageUpdateMultipleDevicesPropertiesRequest` | write result, `diff`, `changed: false` when nothing differs |
| `device rename <DEVICE> <NAME>` | write | the same message, with `Name` only | as above |
| `device delete <DEVICE> [--force] [--allow-referenced]` | write | `deleteDevice`, `MessageDeleteDeviceRequest` | write result, `diff` |
| `device add-virtual --model M --name N [--interface I] [--prop K=V]... [--allow-duplicate-name]` | write | `addVirtualVariable` (model `VirtualVariableGeneral`) or `addVirtualDevice`, `MessageAddDeviceRequest` | write result, `diff`, `device` with `id`, `serial` and `property_keys` |

Details:

- `--model GLOB` in `device list` and `io values` is case-insensitive, `*` matches any text and `?` matches one character. `--name` is a case-insensitive substring. `--parent` matches `HardwareParentId`.
- `set-props` takes `K=V` split at the first `=`. `--props-file` is a JSON object of string values, and `--prop` wins over the file. `Name` is a top-level field and every other key lives in `DeviceProperties`. A key that the device does not have is refused as a typo guard, with close key names in the message, unless `--allow-new-key` is given. Values that equal the current ones are skipped with a warning.
- `delete` refuses a device that has children (other devices with it as `HardwareParentId`) and tells you the ids. Delete children first. `--force` sends `ForceDelete` and is an unverified variant. Live smart rules that mention the device block the delete. A rule mentions it when its id appears under any key ending in `Id` or `Ids` (`ThermostatId`, `PushButtonIds`, `BlindsIds`, `MastersIds`, `InputSensorsIds`, ...; the rule's own id, types, changesets, enum values and user groups excluded) or anywhere in `TriggerInputs`. Such rules block the delete with `invalid_input` and `details.rules` (ids and names) unless you pass `--allow-referenced`, which keeps the delete and adds a warning that lists them. The Core reuses ids, so the check compares the serial number to decide that the device is gone.
- `add-virtual` knows the interface for these models: `VirtualVariableGeneral` (Variable), `VirtualSwitch` (Switch), `VirtualThermostatDummy` (Thermostat), `VirtualAnalogOutput` (Dimmer), `VirtualPushButton` (PushButton), `VirtualReedContact` (ReedContact) and `VirtualMultiValueSwitch` (MultiValueSwitch). Other models need `--interface`. A name that another device already has is refused with `already_exist` (exit 7, `details.existing_id`), so a retry after `write_sent: "unknown"` cannot create a second device. `--allow-duplicate-name` creates it anyway, with a warning. `pp import` follows the same rule for every module and device name in the file, and refuses a config that uses one `Id` twice.

### pp

| Command | Kind | Sends | `data` |
|---|---|---|---|
| `pp inspect <FILE> [--param K=V]...` | offline | nothing | `source` (`xml` or `json`), `template`, `kind` (`packet_parser` or `modbus`), `ccu_id`, `parameters`, `modules`, `devices`, `unresolved_tokens`, `variant` (`verified`, `notes`), `config` (the canonical configuration that would be sent) |
| `pp import <FILE> [--param K=V]... [--ccu ID]` | write | `addPacketParserModule` (new module) or `addTemplateDevicesToExistingPacketParserModule` (devices only), `MessageHttpApiConfigRequest` with `CcuId` and `ConfigJson`, answered by `MessageHttpApiConfigResponse` | write result, `diff` (one `add` per module and device, with script lengths and first lines instead of whole scripts), `ccu_id`, `created` (`modules` and `devices` with real ids), `removed`, `property_differences`. A dry run adds `variant_notes`. |
| `pp script get <DEVICE> [--key K]` | read, from the sync | nothing | `device`, then `scripts` (per script key: `key`, `length`, `first_line`) or, with `--key`, `key`, `length` and the full `text` (masked as `<redacted>` when the key contains "password" or "token", unless `--show-secrets`) |
| `pp script set <DEVICE> --key K (--file F \| --text T) [--allow-new-key]` | write | `updateMultipleDevicesProperties`, `MessageUpdateMultipleDevicesPropertiesRequest` with one property | write result, `changed` |
| `pp script last-run <DEV> [--key K] [--object device\|ccu\|rule]` | read | `getScriptLastRunValues`, `MessageGetScriptLastRunValuesRequest` | `object`, `key`, `has_run`, and when the script ran: `result`, `run_timestamp_utc`, `run_timestamp_ticks`, `run_duration_seconds`, `variables_by_phase` |

Details:

- A property counts as a script when its key contains "Script" or is one of `CustomVariables`, `DeviceVariables` and `Variables`.
- `--file -` reads the script from stdin. The diff of `script set` shows the old and new length and the first line that differs, never the whole script. For a key that contains "password" or "token" the diff shows `<redacted>` instead, unless `--show-secrets`.
- `--ccu` takes an interface id or an exact name. Without it, the import uses the only interface with model `PacketParserCCU`. With none, it fails with `not_found`. With several, it fails with `ambiguous`.
- `import` refuses Modbus configurations (`invalid_input`), refuses unresolved parameters, and warns when a device with the same name exists. After the write it checks that `AddedDevices` lists exactly what was sent, that every id exists with the sent name and model, that children point to their new module and that the device count grew. `property_differences` lists properties the Core stored differently from what was sent.
- `last-run` with `--object ccu` or `--object rule` expects the object's integer id in `DEV`. `--key` is required for devices and interfaces and not needed for rules. When the script never ran, `has_run` is false and nothing else is printed.

### rule

| Command | Kind | Sends | `data` |
|---|---|---|---|
| `rule list [--all] [--type ID] [--name SUBSTR]` | read, from the sync | nothing | `count`, `rules`: `id`, `name`, `type_id`, `type_name`, `version`, `enabled`, `changeset_id`, `deleted` |
| `rule get <RULE>` | read, from the sync | nothing | `id`, `name`, `type_id`, `type_name`, `model` |
| `rule apply <FILE>` | write | `upsertSmartRules`, `MessageUpdateTriggerModelsRequest` | write result, `diff`, `changed`, `rule_id` (on create) |
| `rule enable <RULE>`, `rule disable <RULE>` | write | the same message with `IsEnabled` changed | write result, `changed` |
| `rule delete <RULE>` | write | `deleteSmartRules`, `MessageDeleteTriggerModelsRequest` with `TriggerIds` | write result |
| `rule errors <RULE>` | read | `getSmartRuleErrors`, `MessageGetTriggerErrorsRequest` | `rule`, `count`, `errors` with `ErrorCodeName` (201 FormulaParseException, 202 FormulaEvaluationException, 203 LiteralArgumentException), `SeverityName` and `TimeIso` added |

`rule apply` also reports `changed: false` when the file matches the stored rule. It creates when `Id` is 0 or missing and updates when `Id` is the id of a live rule. `-` as the file reads stdin. `rule get` prints the whole decoded `TriggerModel` under `data.model`, so `jq .data.model` gives a valid file for `rule apply`. `rule enable` and `rule disable` report `changed: false` (with `rule_id` and `reason`) when the rule already has the target state, and send nothing. The Core keeps a deleted rule as a tombstone with the same id, so a deleted rule stays in `rule list --all`.

### io

| Command | Kind | Sends | `data` |
|---|---|---|---|
| `io values [DEVICE...] [--model GLOB] [--name SUBSTR]` | read, from the sync | nothing | `summary` (`devices`, `status_not_ok`: ids with a bad status), `devices`: `id`, `name`, `model`, `values` |
| `io errors <DEVICE> [--origin 0..3]` | read | `getDeviceErrors`, `MessageGetDeviceErrorsRequest` | `device`, `origin_id`, `count`, `errors` with `time`, `error_code_name` and `severity` added |
| `io log <DEVICE> [--limit N]` | read | `getDeviceLog`, `MessageGetDeviceLogRequest` | `device`, `total`, `returned`, `entries`: `time`, `time_th`, `values`, `change_origin`, `handling_status`. The newest N entries (default 50), oldest first. |
| `io watch [--seconds N] [--device DEV]...` | read | nothing after the sync, it listens for broadcasts | `seconds`, `changes` (`device_id`, `device_name`, `type`, `name`, `value`, `received_at`), `broadcasts` (count per class) |

Details:

- Each value entry has `type` (the `ValueLogType` id), `name`, `value` (a double, booleans are 0 and 1), `changed_at` and `changed_on_th` (TapHome time, seconds since 2016-01-01 UTC). The value with type 7 is the device status and gets a `status` object. Codes below 256 are the `DeviceStatus` enum with `name` and `ok`. Larger codes are packed counts of `errors`, `warnings` and `infos`, and count as not ok.
- `io values` shows the values as of the sync that this command just did. For a live view use `io watch`.
- `io errors` chooses `--origin 1` (module) for devices whose interface is `IHardwareParentModule` and 0 (device) otherwise. 2 is the interface and 3 the Core.
- `io watch` accepts 0 to 300 seconds (default 10).

### raw

| Command | Kind | What it does |
|---|---|---|
| `raw ops [--kind read\|subscribe\|write\|action] [--verified true\|false]` | offline | Prints the operation registry: `count` and `operations` with `class`, `ids`, `kind`, `verified`, `response`, `endpoint`, `min_protocol`. |
| `raw call <MessageClass> [--json J \| --json-file F] [--allow-write] [--allow-unverified] [--dry-run] [--wait-broadcasts S]` | depends on the class | Sends one request. |

`raw call` rules:

1. The class must be a known request class, else `invalid_input`.
2. `--dry-run` is offline. It prints `operation`, `verified`, `kind`, `message` (`class`, `fields`), `diff: null`, `frame_hex` and `frame_redacted`, and sends nothing, not even a sync. The frame and `message.fields` are built from the redacted fields.
3. A write or action class needs `--allow-write`, else `write_not_allowed`. A class that is not in the registry counts as a write.
4. A class with `verified: false`, or one that is not in the registry, needs `--allow-unverified`, else `unverified_operation`. So a class without a registry entry needs both switches.
5. A read or subscribe class prints `class`, `response` and `broadcasts` (counts per class collected for `--wait-broadcasts` seconds, up to 3600).
6. A write goes through the [write flow](#the-write-flow). There is no expected state to check, so a real raw write reports `verify.ok: null`. Compare `delta` (the signed change of each count between `state_before` and `state`).

`--json` and `--json-file` hold the request fields as one JSON object. Field names come from `data/catalog.json` and encoding is strict, so an unknown name is an error.

## The write flow

Every write command runs the same steps, in this order.

1. Open a session and read the configuration (sync).
2. Build the message and a diff from that configuration. Checks that fail here (unknown key, name taken, children present) end the command with no change.
3. Decide whether the operation is verified. It is verified when its request class is verified in the registry and the command's variant is not marked unverified.
4. With `--dry-run`, print `{"dry_run": true, "operation": ..., "verified": ..., "message": {"class": ..., "fields": ...}, "diff": [...]}` and stop. Nothing is sent.
5. If the operation is unverified and `--allow-unverified` is missing, fail with `unverified_operation`.
6. Send the request and check the answer. A `MessageGenericResponse` with `ErrorCode` 1 is success. Any other code becomes a Core error. A typed response must have the expected class.
7. Collect broadcasts for 400 ms (`raw call` uses `--wait-broadcasts` if larger).
8. Open a new session and sync again, so the check sees what the Core holds and not the local cache.
9. Run the command's checks against the new state. If one fails, exit with 8 and `verify_failed`, with the full result in `error.details`.

The result of a real write has `operation`, `response`, `broadcasts` (counts per class), `verify` (`ok` and `checks`, each with `name`, `ok` and sometimes `detail`), `state` and `state_before` (the same counts as `sync`). Commands add `diff`, `changed` and their own keys.

A diff entry is `{"op": "add|remove|change", "object": "device|rule|interface", "id": ..., "path": ..., "before": ..., "after": ...}`.

## Variants gated as unverified

The registry marks request classes. Commands can also mark a variant of a verified class as unverified. These need `--allow-unverified`.

| Command | Variant | Reason |
|---|---|---|
| `device delete --force` | Delete although children exist. | Never tested. It is not known whether the Core takes the children along. |
| `device add-virtual` | Any model except `VirtualVariableGeneral` with interface `Variable`, or any use of `--prop`. | Only a plain virtual variable was created on a real Core. |
| `pp import` | A module whose model is not `PacketParserModuleHttp`. | Only that module model was imported. |
| `pp import` | Devices without a new module (a top-level device with `HardwareParentId` of an existing module). | Never imported on a real Core. |
| `rule apply` | An update (`Id` of a live rule) whose `TriggerTypeId` is not 12. | Only the update of a formula rule was tested. |
| `rule apply` | A create of a type other than 12 (FormulaTrigger), or with `IsEnabled` true. | Only a disabled formula rule was created. |
| `raw call` | Any class with `verified: false` in `raw ops`, and classes missing from the registry. | See `taphome raw ops --verified false`. |

Deleting a rule, enabling and disabling a rule of any type, updating a formula rule, renaming a device, changing properties, replacing a script, importing a Packet Parser template with one HTTP module and creating a plain virtual variable are verified variants. `rule enable` and `rule disable` re-send the Core's own bytes with one flag changed and refuse with `roundtrip_mismatch` when the round trip is not byte-exact, so they need no switch for any rule type. The authoritative record is [`docs/verification.md`](verification.md).

## Packet Parser import formats

`pp inspect` and `pp import` take a file whose extension decides the format. Anything else is `invalid_input`.

### XML template (`.xml`)

The format the TapHome app exports for Packet Parser templates.

- The root is `<Templates format="0|1|2" protocolVersion="N">`. Other format numbers are refused. `protocolVersion` defaults to 66.
- The file has exactly one `<Template>` (attributes `id` as a Guid and `revision`). Its children are `Name`, `CcuModel`, `ImportParameters`, `Module` elements and `Device` elements.
- `ImportParameters` lists `<Parameter>` entries with `Id`, `Name` (the label) and an optional `Value` (the default).
- A `Module` has `Name`, `Id`, `Model`, `DeviceProperties` (one child element per property, the text is the value) and `Devices` with `Device` children. A `Device` has `Name`, `Id`, `Model` and `DeviceProperties`.
- The parameter `DeviceNamePrefix` always exists (default empty). The CLI prefixes every name with it, as the app does.
- Parameters are written as `$[Id]` inside names and property values. `--param K=V` sets a value and overrides the default. An unknown id is refused and the message lists the known ids. A token with no value and no default stays in the text, appears in `unresolved_tokens`, and makes `pp import` fail with the list of missing ids.

### JSON config (`.json`)

Either a bare configuration object, or an object with `CcuId` and `ConfigJson`, where `ConfigJson` is an object or a string that holds the JSON. A `CcuId` there selects the interface unless `--ccu` is given.

The configuration has optional `ProtocolVersion` (default 66), `Name`, `Id` and `Revision`, plus `Modules` and `Devices`. At least one of them must be non-empty.

- Module: `Name`, `Model` (both required), optional `Id`, `CategoryId`, `ZoneId`, `DeviceProperties` and `Devices`.
- Device: the same fields, and a top-level device also needs `HardwareParentId`, the id of an existing module.
- Property values may be strings, numbers or booleans. They are converted to text the way the app does, with booleans as `True` and `False`. Arrays and objects are refused, so write JSON values as text.
- A Guid field must be a valid Guid or empty.
- JSON files have no parameter list. Any `$[Id]` token found in the file counts as a parameter you must supply with `--param`. Text like the JSON path `$[0]` is not a token.

### Placeholder ids

Modules and devices in the file get a negative placeholder id when they have none (`-1`, `-2`, and so on, skipping ids already used). The Core assigns real ids and returns them in `created`. Variable lists inside properties (`DeviceVariables`) refer to devices by placeholder id, so keep those consistent with the ids in the file. A positive `Id` on a module or device is refused with `invalid_input` ("ids in a config are placeholders and must be 0 or negative; the Core allocates real ids"). `HardwareParentId` may be positive in the devices-only form, where it names an existing module.

### What is refused

- Modbus configurations (`ProtocolVersion` 56, a `CcuModel` starting with `Modbus`, or a model starting with `Modbus`) fail with `invalid_input`, because they use another message class.
- A missing `Name` or `Model`, a non-integer `Id`, a top-level device without `HardwareParentId`, a bad Guid, an empty config.
- A `--param` with an unknown id, and missing values for tokens.
- A file extension other than `.xml` and `.json`.

## Rule file format

The file holds one `TriggerModel` object. `rule get` prints such an object under `data.model`.

| Field | Meaning |
|---|---|
| `Id` | 0 or missing to create. The `Id` of a live rule to update. |
| `TriggerTypeId` | The rule type, for example 11 EvaluationTrigger and 12 FormulaTrigger. It cannot change on update. `taphome` prints names in `type_name`. |
| `Name` | Required and non-empty on create. A live rule with the same name makes the create fail with `already_exist` (exit 7), because the new rule is found by name afterwards. |
| `Version` | Optional. If present it must equal the Core's schema version for the type (shown in `sync --section core` under `trigger_versions`). If missing, the Core's version is used. |
| `IsEnabled` | Sent as given. Omitted means false. |
| `TriggerParameters` | The rule itself. It must contain `"$type"`, the concrete parameters class, for example `FormulaTriggerParameters` or `EvaluationTriggerParameters`. No table maps type ids to classes, so copy the value from `rule get` of an existing rule of that type. The error message lists the `$type` values seen on this Core for the type. |

Minimal formula rule:

```json
{
  "Id": 0,
  "TriggerTypeId": 12,
  "Name": "TEST-zz-agent-test formula",
  "IsEnabled": false,
  "TriggerParameters": {
    "$type": "FormulaTriggerParameters",
    "InputVariables": [{"DeviceId": 11, "ValueLogType": 62, "Abbreviation": "Va"}],
    "OutputVariables": [{"DeviceId": 11, "ValueLogType": 62}],
    "EvaluationFormula": "Va"
  }
}
```

Replace `11` with the id of a real variable device. `ValueLogType` 62 is `VariableState`.

Fields the Core manages: `ChangesetId`, `TriggerInputs`, `Priorities` and `IsDeleted`. On create the CLI removes them from the file before sending and the Core fills them. On update the file's `TriggerInputs`, `ChangesetId` and `IsDeleted` are ignored: the stored values are kept and a warning appears when the file's value differs. `Priorities` is a legitimate edit. When the file's `Priorities` differs from the stored one, it shows in the diff and the CLI checks after the write that it reads back equal.

An update applies the file over the stored model. Keys missing from the file keep their stored values. If the result equals the stored rule, the command prints `changed: false` and sends nothing. Before an update the CLI checks that the stored rule decodes and re-encodes to the exact bytes the Core sent. If not, it stops with `roundtrip_mismatch`, so a field the codec does not understand is never lost. After a write it checks that the stored rule contains every key that was sent and that `ChangesetId` grew (create checks that exactly one live rule has the name and type, and that `IsEnabled` and `TriggerParameters` read back as sent).

## Configuration

### Environment variables

| Variable | When | Meaning |
|---|---|---|
| `TAPHOME_HOST` | run time | Core address, `host` or `host:port`. When unset, the host stored with the session is used. |
| `TAPHOME_USER` | run time | User name. Stored lower-case. |
| `TAPHOME_PASSWORD` | run time | Password. Needed for login. |
| `TAPHOME_HOST_FILE`, `TAPHOME_USER_FILE`, `TAPHOME_PASSWORD_FILE` | run time | A file with the value (surrounding whitespace is trimmed). Used when the plain variable is unset or empty. An unreadable file is `missing_config`. |
| `TAPHOME_CLOUD_EMAIL`, `TAPHOME_CLOUD_PASSWORD` | run time | TapHome account for remote access through the cloud tunnel. Setting the e-mail switches the CLI to the cloud. Setting it together with `TAPHOME_HOST` is `missing_config`. |
| `TAPHOME_LOCATION` | run time | Name or id of the location. Needed only when the cloud account has several. |
| `TAPHOME_CONFIG_DIR` | run time and build time | Directory for the session, the device id and the salt file. |
| `TAPHOME_PASSWORD_SALT` | build time and run time | The salt. At run time it overrides the built-in value. There is no `_FILE` variant. |
| `TAPHOME_REQUIRE_SALT` | build time | `1` makes a build without a salt fail. |
| `TAPHOME_DEBUG` | run time | `1` prints details of internal errors and panics on stderr. |

A stored session is used only when it matches the configured host and user. An empty variable counts as unset.

### Files in the config directory

The directory is `$TAPHOME_CONFIG_DIR`, else `~/.config/taphome` on macOS and Linux, else `%APPDATA%\taphome` on Windows. If neither `TAPHOME_CONFIG_DIR` nor home (`APPDATA` on Windows) is set, commands that need the directory fail with `missing_config` and the message to set `HOME` or `TAPHOME_CONFIG_DIR`. There is no relative fallback. At build time a missing home only means there is no salt file. A directory that the CLI creates gets mode 700 (an existing one is left alone). Files get mode 600 on unix and are written to a temporary file and renamed. Windows has no mode bits, so the profile directory permissions protect the files.

| File | Content |
|---|---|
| `session.json` | `host`, `location_id`, `user`, `session_id` and an optional `db_hash` (a cache that saves a round trip). Created by `auth login`. |
| `device-id` | 16 random hex characters that identify this CLI to the Core. Created on first use. |
| `salt` | Optional. Read only by a build, never at run time. |

### Salt lookup

At build time, `build.rs` takes the first that exists.

1. The environment variable `TAPHOME_PASSWORD_SALT`.
2. The file `salt` in the config directory (`~/.config/taphome/salt`, or `%APPDATA%\taphome\salt` on Windows). One trailing newline is removed.

It writes the bytes to a file in the build output directory, which `src/salt.rs` includes. The value never goes through `cargo:rustc-env` or build output. With neither source, the build prints a warning and embeds an empty value, unless `TAPHOME_REQUIRE_SALT=1` is set, in which case it fails.

At run time, `TAPHOME_PASSWORD_SALT` overrides the built-in value. With neither, login fails with `salt_missing`, and every command that reuses a stored session still works. `taphome auth status` shows which source is in use as `salt_source`. Where to get the salt is described in [`docs/auth.md`](auth.md).

## Verified against a real Core

Core software 2025.2.32396, protocol 89, 2026-11-17. Details, observations and the list of untested variants are in [`docs/verification.md`](verification.md).

| Command | Status | Needs `--allow-unverified` |
|---|---|---|
| `auth login`, `auth status`, `sync` | Verified on a real Core | No |
| `device list`, `device get` | Verified on a real Core | No |
| `device add-virtual` | Verified for these variants only | Any model except `VirtualVariableGeneral`, or `--prop` |
| `device rename`, `device set-props` | Verified on a real Core (key `Name`) | No |
| `device delete` | Verified for these variants only | `--force` |
| `io values`, `io errors`, `io log`, `io watch` | Verified on a real Core (error lists seen empty only) | No |
| `pp inspect` | Offline, nothing to verify | No |
| `pp import` | Verified for these variants only (template with one HTTP module) | Other module models, devices without a new module |
| `pp script get`, `pp script last-run`, `pp script set` | Verified on a real Core | No |
| `rule list`, `rule get`, `rule errors` | Verified on a real Core | No |
| `rule apply` | Verified for these variants only (create of a disabled formula rule, update of a formula rule) | Create of another type or with `IsEnabled` true, update of a rule that is not type 12 |
| `rule enable`, `rule disable` | Verified on a real Core | No |
| `rule delete` | Verified on a real Core | No |
| `raw ops`, `raw call` (reads) | Verified on a real Core | Classes with `verified: false` or missing from the registry |
| `raw call` (writes) | Implemented, not verified | Yes, together with `--allow-write` |

Release binaries for Linux and Windows, the release workflow and `install.sh` were not run, because no release exists yet.
