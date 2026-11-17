---
name: taphome-cli
description: Use when configuring or inspecting a TapHome Core with the `taphome` CLI. Covers listing devices and virtual devices, importing Packet Parser templates, reading and changing Packet Parser scripts, creating and editing smart rules, and checking inputs, outputs and errors after a change. Not for switching lights or other day-to-day device control.
---

# taphome CLI for agents

`taphome` talks to a real home. Every command prints one JSON document. The full reference is `docs/cli.md` in this repository.

## Preconditions

1. `taphome --version` works. If not, install it as the README describes.
2. `taphome auth status` prints `"valid": true`. If it does not, ask the owner to log in. Credentials come only from the environment (`TAPHOME_HOST`, `TAPHOME_USER`, `TAPHOME_PASSWORD`, or the `_FILE` variants) or from the owner through the harness secret prompt. Never put them on the command line, in a file inside a repository, or in a message.
3. `salt_source` in the status output must not be `missing`. If it is, login fails with `salt_missing`. Tell the owner to use a release binary.
4. On macOS, `connection_failed` with "No route to host" means the app running you lacks the Local Network permission. Tell the owner to grant it under System Settings, Privacy and Security, Local Network.

## Rules for a live home

- Reads are free: `sync`, `device list|get`, `rule list|get|errors`, `io values|errors|log|watch`, `pp inspect`, `pp script get|last-run`, `raw ops`.
- Before any write, run the same command with `--dry-run`, show the owner the `diff` and the message, and wait for approval. A dry run sends nothing.
- Test objects get the prefix `TEST-zz-agent-test` in their name. Delete them when the test is done.
- Never pass `--allow-unverified` or `raw call --allow-write` unless the owner agreed to that specific operation. Names of operations are in the `operation` field of the dry run. A class that is missing from `raw ops` needs both switches.
- Run `taphome sync` before and after a write and compare `counts`. A write prints `state_before` and `state` too.
- Device control (`MessageDeviceAction` and similar) is out of scope. Do not use `raw call` for it.
- Never print or copy the Core address, user names, passwords, tokens or the salt into a repository, a report or a ticket.

## Reading results

- Success: `{"ok": true, "command": ..., "data": {...}, "warnings": [...]}`. Read `warnings`, they point at real problems such as a name that is already taken.
- Failure: `{"ok": false, "error": {"code": ..., "message": ..., "details": {...}}}`. Branch on `error.code`.
- Exit codes: 0 ok, 2 usage or invalid input, 3 auth or config, 4 connection or timeout, 5 the Core rejected the request, 6 not found or ambiguous, 7 refused by a CLI gate, 8 verification failed.
- A real write verifies itself with a fresh sync. Check `data.verify.ok` is true. Exit 8 means the Core answered but the expected state was not found. Look at `error.details`, then `sync` before retrying.
- `error.details.write_sent` equal to `"unknown"` after a timeout means the change may have been applied. Read the state before sending it again.
- Exit 7 with `unverified_operation` means the variant was never tested on a real Core. Stop and ask the owner. It is not a reason to add `--allow-unverified`.

## Recipes

Replace `<device>` with an id or an exact name. A name that matches several devices fails with `ambiguous`, so prefer ids.

### Inspect the home

```sh
taphome sync
taphome device list --model 'PacketParser*'
taphome device get <device>
taphome rule list
taphome io values --name Lamp
```

`sync` gives the interfaces with their ids and the counts. `device list` accepts `--model`, `--name`, `--parent` and `--ccu`.

### Import a Packet Parser template

1. `taphome pp inspect template.xml --param Key=value` shows parameters, modules, devices, `unresolved_tokens` and `variant.notes`. Offline.
2. `taphome pp import template.xml --param Key=value --param DeviceNamePrefix=TEST-zz-agent-test- --dry-run`. Show the `diff` and `variant_notes` to the owner.
3. After approval, run the same command without `--dry-run`.
4. Read the new ids from `data.created.modules[].id` and `data.created.devices[].id`. Look at `warnings` and `data.property_differences`.
5. `taphome pp script last-run <device> --key <ScriptKey>` and `taphome io errors <device>` show whether the scripts run.

`pp import` takes an XML template or a JSON config. If `unresolved_tokens` is not empty, pass `--param ID=VALUE` for each one. With several Packet Parser interfaces, pass `--ccu <id>`.

### Change a script

```sh
taphome pp script get <device>
taphome pp script get <device> --key ReadStateScriptPacketParser | jq -r .data.text > current-script.txt
taphome pp script set <device> --key ReadStateScriptPacketParser --file new-script.txt --dry-run
taphome pp script set <device> --key ReadStateScriptPacketParser --file new-script.txt
taphome pp script last-run <device> --key ReadStateScriptPacketParser
taphome io errors <device>
```

The first `get` lists script keys with lengths. With `--key` the output holds the full text under `data.text`. The dry-run diff shows the old and new length and the first line that differs. For a script rule, `pp script last-run <rule-id> --object rule` shows its last run.

### Create a formula rule from a JSON file

Save this as `rule.json`. Replace `11` with the id of a variable device (`device list --model VirtualVariableGeneral`). `ValueLogType` 62 is the variable state.

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

```sh
taphome rule apply rule.json --dry-run
taphome rule apply rule.json
taphome rule errors "TEST-zz-agent-test formula"
```

A disabled formula rule is the only verified create. Another rule type, or `IsEnabled` set to true, is gated as unverified. `TriggerParameters` must contain `"$type"`. For another type, copy it from `taphome rule get <existing rule of that type>`. Do not send `ChangesetId`, `TriggerInputs`, `Priorities` or `IsDeleted`, the Core manages them. The new rule's id is in `data.rule_id`.

### Edit an existing rule

```sh
taphome rule get "<rule>" | jq .data.model > rule.json
# edit rule.json
taphome rule apply rule.json --dry-run
taphome rule apply rule.json
```

`Id` stays in the file, so the command updates that rule. An update is an unverified variant, so it needs the owner's agreement before `--allow-unverified`. Keys you remove from the file keep their stored values. On an update the CLI ignores `TriggerInputs`, `ChangesetId` and `IsDeleted` from the file and warns when they differ from the stored ones. A changed `Priorities` is applied and shown in the diff. `rule get` hides secrets such as a `Passcode`. A file that still holds `<redacted>` is refused with `invalid_input`, so read the rule again with `--show-secrets` if you must apply it. `taphome rule enable` and `rule disable` are unverified too.

### Rename or reconfigure a device

```sh
taphome device get <device>
taphome device rename <device> "New name" --dry-run
taphome device set-props <device> --prop IsReadOnly=True --dry-run
```

`set-props` refuses a key the device does not have (typo guard) and suggests close names. `--allow-new-key` overrides it, use it only when you know the key is valid. Values are text, so booleans are `True` and `False`.

### Clean up test objects

1. `taphome device list --name TEST-zz-agent-test` and `taphome rule list --name TEST-zz-agent-test`.
2. Delete rules: `taphome rule delete "<name or id>" --dry-run`, then for real.
3. Delete child devices first, then the module: `taphome device delete <child>` for each, then `taphome device delete <module>`. A device with children is refused, and `--force` is unverified. A device that live smart rules reference is refused with the rule ids and names in `details.rules`. Delete or change those rules first, or pass `--allow-referenced` when the owner agreed.
4. `taphome sync` and compare `counts` with the numbers from before the test.

## Pitfalls

- The Core reuses ids. After a delete, a new device or rule can get an old id. Never keep an id across a delete, and find new objects by the ids a write returns or by name.
- A deleted rule stays in the list as a tombstone with its id. `rule list` hides it, `rule list --all` shows it. A tombstone is not a leftover to clean up.
- Names can be ambiguous. A second object with the same name makes name lookup fail with `ambiguous`. `add-virtual` and `pp import` warn when a name is already taken.
- `io values` shows values from the sync that the command just did. Use `io watch --seconds 20 --device <device>` to see changes happen.
- `rule apply` with a taken name refuses a create (`already_exist`). Pick another name or update the live rule by `Id`.
- A property you send may come back different, because the Core drops or normalises some keys. `pp import` lists these in `property_differences`.
- `io errors` asks for the module's errors (origin 1) when the device is a module and for the device's own errors (origin 0) otherwise. `--origin 0|1|2|3` overrides it (device, module, interface, core).
