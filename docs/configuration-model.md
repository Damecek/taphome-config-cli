# Configuration model

This page describes the objects a client reads and writes on a Core, and how the app changes them. Message sequences refer to operation ids from `openapi/operations/*.yaml`. Each operation is also in `openapi/taphome-core.yaml` as `POST /Core/messages/{RequestClass}`.

Status values. "Confirmed" means seen on a live Core (see `docs/verification.md`). Everything else is derived from the app code and not yet tested. No write operation has been tried on a real Core, so every sequence below is a plan derived from the app's behaviour. Sequences that create objects should use names with the prefix `zz-agent-test` and delete the objects afterwards.

## Object overview

| Concept | What it is | Identity | Status |
|---|---|---|---|
| Interface (CCU) | A communication channel. Examples are TapHome Bus, Modbus, Packet Parser, MQTT and virtual. | Integer `CcuId`, allocated by the Core | confirmed for the models `VirtualCCU`, `TapHomeBusCCU`, `PacketParserCCU` and `MqttCCU` |
| Module | A device that represents hardware or a connection target. Examples are a TapHome Bus board, a Modbus slave and a Packet Parser host. | Integer device id, allocated by the Core | derived |
| Device | Anything with values and actions. Examples are a light, a button, a thermometer or a module. | Integer `ID`, allocated by the Core | confirmed for the model name families `TapHomeBus*`, `PacketParser*` and `Virtual*` |
| Terminal | A physical input or output of a module that is configured as a device. It is a child device with `HardwareParentId` set to the module id. | Integer device id | derived |
| Zone, category, dashboard | A grouping of devices. All three are `Dashboard` objects of a dashboard set. | Guid, allocated by the client | derived |
| Smart rule | An automation. The code calls it a trigger. | Integer `Id`, allocated by the Core | derived |

On the test Core, the reconnect response held 136 devices, 4 interfaces, 88 smart rules and 24 dashboards (confirmed).

## Interfaces, modules and devices

### Where a client gets the model

The reconnect response carries all interfaces (`Ccus`), all devices with their current values, the smart rules, the dashboards, the permissions and the Core properties. There is no "list devices" request. Operation `getDeviceListOnLogin` documents this. Afterwards the Core pushes `MessageDeviceListUpdatedBroadcast` (full lists), `MessageDeviceUpdatedBroadcast` (one device) and `MessageDeviceValueChangedBroadcast` (one value). Confirmed for the single-message initial sync, derived for the broadcasts.

### Interface

`GenericCcu` has `CcuId`, `Name`, `Model`, `SerialNumber`, `PollInterval`, `IP`, `LogValues`, `IsIgnored` and `CcuProperties`, a string dictionary. The interface type follows from the `Model` string.

| Type | Model string | Created by client | Min protocol |
|---|---|---|---|
| TapHome Bus | `TapHomeBusCCU` | no, built in | |
| Virtual | `VirtualCCU` | no, built in | |
| Virtual variables, weather | `VirtualVariablesCCU`, `WeatherCCU` | no, built in | |
| Modbus RTU, Modbus TCP | `ModbusRtuCCU`, `ModbusTcpCCU` | yes | 56 for the config messages |
| Packet Parser | `PacketParserCCU` | yes | 66 |
| MQTT | `MqttCCU` | yes | 69 |
| KNX, ZigBee | `KNXCCU`, `ZigBeeCCU` | yes | 12, 31 |
| Other vendor interfaces | for example `JablotronCCU`, `LGACCCU`, `DaikinACCCU`, `SIPDoorbellCCU` | yes | varies |

Operations. `addInterface` and `addPacketParserInterface` (message `MessageAddCcuRequest`), `deleteInterface`, `updateInterfaceProperties` (`MessageUpdateCcuPropertiesRequest`, a property delta), `updateInterfacePorts`, `getAvailableInterfacePorts`. The interface models `TapHomeBusCCU` own serial ports through a `Port` property, a comma-separated list of system names such as `/dev/ttyS0`.

### Device

`GenericDevice` is a subclass of `DeviceBase`. Remember the root-first rule from `docs/protocol.md` when you encode it.

- Identity and placement. `ID`, `CcuId`, `Name`, `Icon`, `Model`, `Interface`, `SerialNumber`, `HardwareParentId`, `ZoneId`, `CategoryId`, `DeviceType`, `Visibility`, `IsIgnored` and `PollInterval`.
- Configuration. `DeviceProperties` is a `Dictionary<string,string>`. This is the whole editable configuration, and every value is a string, including numbers and JSON documents.
- Runtime state. `CurrentValues` (value type id to value, timestamp and origin), `Status` and `Timers`.
- Capabilities. `SupportedValueLogTypes`, `SupportedActionTypes`, `CanStartupValueBeSet` and `IsHardwareReadable`.

The device kind shown in the app (switch, push button, reed contact, analog output, blinds, thermostat and so on) is not an enum on the wire. The app derives it from `SupportedValueLogTypes` and from `Model`. `Interface` carries a class name and `Model` the concrete implementation name.

### Value types

Values are doubles keyed by a `ValueLogType` id. Booleans are 0 and 1. The ids come from the declaration order of the enum in the app, so check a few against a live Core before you rely on them.

| Purpose | Value type (id) |
|---|---|
| Switch output | `SwitchState` (48) |
| Push button | `ButtonPressed` (52), `ButtonHeldState` (38) |
| Reed contact, motion | `ReedContact` (44), `Motion` (51) |
| Analog output | `AnalogOutputValue` (42), `AnalogOutputDesiredValue` (67), as a fraction from 0 to 1 |
| Analog input | `AnalogInputValue` (55) |
| Temperature, set point, humidity | `RealTemperature` (5), `TemperatureSetPoint` (6), `Humidity` (3) |
| Impulse counter | `TotalImpulseCount` (56) |
| Blinds | `BlindsLevel` (46), `BlindsSlope` (10), `BlindsIsMoving` (66) |
| Every device | `DeviceStatus` (7), `OperationMode` (22), `ManualTimeout` (23), `IsSafetySmartruleActive` (63) |

`DeviceStatus` below 256 is a plain enum (0 OK, 1 BatteryLow, 2 BatteryCritical, 3 Damaged, 4 CommunicationError, 5 UnknownError, 6 AuthorizationError, 7 ConfigurationError). From 256 it is packed, with the error count in bits 8 to 15, the warning count in bits 16 to 23 and the info count in bits 24 to 31. The full table is in the generated schemas.

### Device type

`DeviceType` is the semantic role, for example Light 20, PowerOutlet 30, MotionSensor 1010, AirTemperature 3001 or Module 7001. It is set through the `DeviceType` property, as an integer in a string. The allowed types depend on the device kind. Changing the type in the app also rewrites type-specific defaults in the same write, such as `OnStateName` and `OffStateName`. Strings such as `${general_on}` are translation keys, not literal text.

### Zones and categories

Zones and categories are dashboards. A `Dashboard` belongs to a `DashboardSet`, where set 0 holds dashboards, set 1 zones and set 2 categories. The client picks the Guid. Operations. `addZone` (`MessageAddDashboardRequest`), `updateZoneProperties`, `deleteZone`, `assignDevicesToZone` and `assignDevicesToCategory`. The last two are a property update that sets `ZoneId` or `CategoryId` on the devices.

### Id allocation

| Object | Who picks the id |
|---|---|
| Interface, device, module, smart rule | The Core. The client sends 0 or leaves it out, and reads the id from the response or the broadcast. |
| Zone, category, dashboard | The client, as a new Guid. |
| Long-running task | The client, as a Guid. |
| Objects inside one Packet Parser or Modbus config | Placeholder ids, negative integers that exist only inside that message. The Core assigns real ids. |

### Write granularity

| Target | Granularity | Operation |
|---|---|---|
| Device configuration | Property delta. Send only the changed keys. | `updateMultipleDevicesProperties` |
| Interface configuration | Property delta | `updateInterfaceProperties` |
| TapHome Bus terminal mapping | Full desired state of one module, as JSON | `tapHomeBusConfig` |
| Virtual device | Create | `addVirtualDevice`, `addVirtualVariable` |
| Packet Parser, Modbus | Create a module with its devices in one message | `importPacketParserTemplate`, `importModbusTemplate` |
| Smart rule | The whole rule | `upsertSmartRules` |

Physical devices are created by a bus scan, a serial number or an interface-specific config message, never by `addDevice`.

## TapHome Bus devices and I/O

### Modules and terminals

- A bus module is a device whose `SerialNumber` has the form `AAAA-BBBB-CCCC-DDDD`. The app checks for at least 16 characters and exactly three hyphens. The first segment is the module type, for example `DI32`, `DO12`, `UI24`, `CORE20` or `LEDOC12`.
- A terminal device has the serial `<moduleSN>:<Type>[-<TerminalType>]-<index>` and `HardwareParentId` equal to the module id. Internal sensors end in `-INT` or `-EXT`.
- Each module type has a fixed terminal capacity, for example `DO12` has 12 digital outputs or 6 blinds, and `DI32` has 32 digital inputs.

### What a terminal can become

The configuration message `MessageTapHomeBusConfigRequest` has `DeviceId`, `CcuId` and `ConfigJson`. The JSON has a `Devices` list of `{Index, Type, ExtraAttributes}`. The index is 1 based inside its terminal group.

| Terminal | Allowed `Type` values |
|---|---|
| DI (digital input) | PushButton, Reed, ImpulseCounter |
| UI (universal input) | PushButton, Reed, ImpulseCounter, Thermometer, AnalogInput |
| NTC | Thermometer |
| DO (digital output) | Switch, Blind (uses two outputs), AnalogOutputOnDO |
| UO (universal output) | SwitchOnUO, LedDimmer, Dimmer, AnalogOutputOnUO, BlindOnUO, SlideOnUO, RGB, TunableWhite |
| AO (analog output) | AnalogOutput, RGB, TunableWhite |
| BLE (encoder blind) | EncoderBlind, CurrentLimitedBlind, EncoderSlat, EncoderSlide, CurrentLimitedSlide |

`ExtraAttributes` holds `DeviceName`, `DeviceZoneId`, `DeviceCategoryId`, `TerminalType` and `DeviceTypeProperties`, a JSON string with a `DeviceType` id. Blinds name their outputs with `TerminalUp` and `TerminalDown`, for example "DO3". The message replaces the whole mapping of the module. A partial list may remove the other terminals (not known), so read the current mapping first and merge.

To add a module, send the message with `DeviceId` 0, the interface's `CcuId` and `{"AddModules":["<serial>"]}`. The response lists `AddedDevices`, and an empty list means the module was not found on the bus.

### Sequence A. Add a module and a light on one relay

1. Check the cached device list that no device has the module serial.
2. `tapHomeBusConfig` with `DeviceId` 0, the `CcuId` of the `TapHomeBusCCU` interface and `ConfigJson` `{"AddModules":["<serial>"]}`.
3. Read `AddedDevices[0].ID` from the response. This is the module id.
4. Optional. `moduleBlinkLed` (message `MessageDeviceAction`, action `module-led-start`, later `module-led-stop`) to find the box.
5. `tapHomeBusConfig` with `DeviceId` set to the module id, `CcuId` 0 and `ConfigJson` listing all wanted terminals, for example one `Switch` at index 1 on a `DO` terminal with `DeviceName` "zz-agent-test light".
6. Wait for `deviceListUpdated`. The new child device has `HardwareParentId` equal to the module id and the serial `<moduleSN>:Switch-1`.
7. Change type, name or zone later with `updateMultipleDevicesProperties`, for example `{"DeviceType":"20"}`.

### Testing outputs and reading inputs

- Test an output with `setDeviceValue`, which sends `MessageDeviceAction` with `DeviceId`, `DeviceName`, an `Action` of `switch`, `dim-level` or `blind-level`, and one parameter. Accept the answer only if `ResponseFlags` has Handled. A missing flag means the Core refused, for example because a safety rule is active.
- Confirm the result with `MessageDeviceValueChangedBroadcast` for the value type (48 for a switch), then restore the state.
- Read inputs from `CurrentValues` in the cached device list, and keep them current from `deviceValueChanged`. Use `getModuleInfo` and `getAllModulesInfo` for module health and `getDeviceErrors` for faults. `getDeviceLog` returns the log and can subscribe.

### Sequence B. Test an output and read it back

1. Take the id of the configured terminal device.
2. Optional. `getDeviceLog` with `ShouldSubscribe` true. Renew the subscription every 5 s.
3. `setDeviceValue` with action `switch` and value true.
4. Check `ResponseFlags` for Handled.
5. Wait for a `deviceValueChanged` broadcast with value type 48 and value 1.
6. `setDeviceValue` with value false to restore the state.
7. `getDeviceErrors` to look for output faults.

## Virtual devices

One message creates all of them. `addVirtualDevice` and `addVirtualVariable` send `MessageAddDeviceRequest` with `DeviceName`, `Icon`, `Interface`, `Model`, `CcuId` (leave it out), `SerialNumber`, `DeviceProperties` and `DeviceTypeId`. The response `MessageAddDeviceResponse` has `Ccus` and `AddedDevice`.

| Model | Interface |
|---|---|
| `VirtualSwitch` | Switch |
| `VirtualThermostatDummy` | Thermostat |
| `VirtualAnalogOutput` | Dimmer |
| `VirtualPushButton`, `VirtualReedContact`, `VirtualMotionSensor` | PushButton, ReedContact, motion |
| `VirtualMultiValueSwitch` | MultiValueSwitch, with `Value0Name` to `Value9Name` in the properties |
| `VirtualElectricityMeter`, `VirtualTemperatureSensor` and similar | meters and sensors |
| `VirtualVariableGeneral` and time or sun variables | Variable |

### Sequence C. Create a virtual variable

1. `addVirtualVariable` with `DeviceName` "zz-agent-test-var", `Interface` "Variable", `Model` "VirtualVariableGeneral" and `DeviceTypeId` 0.
2. Read `AddedDevice.ID` from the response.
3. `updateMultipleDevicesProperties` for name, unit or start value. Read `AddedDevice.DeviceProperties` for the key names.
4. Clean up with `deleteDevice` (`MessageDeleteDeviceRequest` with `DeviceId` and `ForceDelete` false).

## Packet Parser and Modbus

### What a template is

A template is an XML file with a module, its devices and their scripts, plus parameters such as an IP address. Templates exist only on the client. The app reads the XML, shows the parameters, replaces every `$[ParamId]` token in names and property values, converts the result to JSON and sends it. The Core never sees the XML. A client can therefore skip templates and build the JSON itself. The operations are `exportPacketParserTemplate`, `loadLocalTemplateFile` and `downloadCommunityTemplates`, which are client-side only and listed in `openapi/operations/EXCLUDED.md`.

### The config message

`importPacketParserTemplate` sends `MessageHttpApiConfigRequest` with `CcuId` (field 1) and `ConfigJson` (field 2). The name comes from the interface that was called HTTP API before protocol 66. `importModbusTemplate` sends `MessageModbusConfigRequest` with the same layout. Both answer with `CcuId`, `AddedDevices` and `RemovedDevices`. A failure is `MessageGenericResponse`, for example error 26 when the Core is too old.

`ConfigJson` is JSON with PascalCase names, nulls omitted, enums as numbers and Guids as text.

```
TemplateConfiguration
  ProtocolVersion  int      66 for Packet Parser, 56 for Modbus
  Name, Id, Revision        optional
  Modules          [ModuleTemplate]
  Devices          [DeviceTemplate]    devices for an existing module (HardwareParentId required)

DeviceTemplate
  Id               int      placeholder, negative, 0 when unused
  Name, Model      string
  HardwareParentId int      real module id, or 0
  CategoryId, ZoneId  Guid  empty means none
  DeviceProperties {string: string}

ModuleTemplate = DeviceTemplate plus Devices [DeviceTemplate]
```

Module models are `PacketParserModuleHttp`, `...Tcp`, `...Udp`, `...Ftp`, `...Mqtt` and `ModbusModule`. Device models are `PacketParserSwitch`, `PacketParserDimmer`, `PacketParserThermostat`, `PacketParserMultiValueSwitch`, `PacketParserTemperatureSensor`, `PacketParserElectricityMeter`, `PacketParserReedContact`, `PacketParserVariable`, `PacketParserPushButton`, `PacketParserSlide`, `PacketParserHSBLight`, `PacketParserWhiteLight`, `PacketParserAnalogInput` and `PacketParserBlind`. Modbus uses the same names with the prefix `Modbus`. The Core accepts numbers given as strings in the JSON.

### Module properties

| Key | Meaning |
|---|---|
| `IpAddress`, `Port`, `UseHttps` | Target. App defaults for the port are 80 (http), 21 (ftp), 4096 (udp), 1024 (tcp) and 1883 (mqtt). |
| `StayConnected` | Keep the socket open. `True` or `False`. |
| `InternalPollInterval` | Poll interval of the module. The unit is not known. |
| `User`, `Password` | Credentials for http and ftp. |
| `RootTopic` | MQTT root topic. |
| `MessageFramingDefinition` | JSON with `Framing` (0 None, 1 Delimiter, 2 Satel, 3 KLF200) and a `Configuration` string. |
| `CustomVariables`, `DeviceVariables` | JSON lists of constants and of input variables for the scripts. |
| Modbus only | `SlaveId`, `IpAddress` (TCP), `InitializeScript`, `ReadErrorsScript`, `ModuleVariables`, `PrefetchModeId`. |

Which keys a device model has is decided by the Core. Read `AddedDevices[].DeviceProperties` after creation.

### Scripts

Scripts are plain text in device or module properties. The Core stores them and runs them. A script kind decides which functions are available.

| Kind | Where | Examples |
|---|---|---|
| Read | Module and device | `ReadScriptPacketParser`, `ReadSwitchStateScriptPacketParser`, `ReadTemperatureScriptPacketParser` |
| Write | Module and device | `WriteScriptPacketParser`, `WriteSwitchStateScriptPacketParser` |
| Listener | Module | `ListenerScriptPacketParser`, with the variables `RECEIVEDMSG` and `RECEIVEDBYTES` |
| Service | Module and device | `ServiceAttributesScriptsPacketParser`, `ServiceActionsScriptsPacketParser`, as JSON lists |
| Modbus | Module and device | `InitializeScript`, `ReadScript`, `WriteScript` and the `Read<X>Script` forms |

The language has `;` terminated statements, `VAR name := expr` declarations, `IF ... ELSEIF ... ELSE ... END`, `WHILE ... LOOP`, `RETURN(value)`, `#` comments at the start of a line, and case-insensitive keywords. Network functions (`SENDHTTPREQUEST`, `SENDDATA`, `FTPDOWNLOAD`, `MQTTPUBLISH`) belong to Packet Parser scripts. `MODBUSR`, `MODBUSW` and `MODBUSWNE` belong to Modbus scripts. Each function has a minimum protocol version.

An input variable of a script is a `FormulaDeviceVariable` with `DeviceId`, `ValueLogType`, `ValueAggregation`, `Abbreviation` and `ValueLogTypeFlavourId`. The app uses flavour 1 (`RealValue`) for Packet Parser and Modbus scripts.

### Debugging

The only debug channel after saving is `getScriptLastRunValues` (`MessageGetScriptLastRunValuesRequest`). It takes a `LastRunId` with an object type (Device 0, Ccu 1, Smartrule 2), the object id and the `ScriptKey`, which is the property name. The response has the variable values of each capture phase, the result, the run time and the duration. Calls to `ADDERROR`, `ADDWARNING` and `ADDINFO` appear as device errors, readable with `getDeviceErrors`.

### Sequence D. Packet Parser module with one device

1. Find the interface with model `PacketParserCCU` in the cached list. If it is missing, `addPacketParserInterface` with `Model` "PacketParserCCU" and read `CcuId` from the response.
2. Build `ConfigJson` with one module (`Id` -1, model `PacketParserModuleHttp`, properties `IpAddress`, `Port`, `StayConnected`, `InternalPollInterval`) and one device (`Id` -2, model `PacketParserSwitch`, properties with the read and write script texts). Prefix the names with "zz-agent-test".
3. `importPacketParserTemplate` with `CcuId` and `ConfigJson`.
4. Read the module id and device ids from `AddedDevices`.
5. After one poll interval, `getScriptLastRunValues` for the read script key.
6. Change scripts with `updateMultipleDevicesProperties` and check again.

The same flow works for Modbus with `addModbusInterface` and `importModbusTemplate`. Whether the Core remaps placeholder ids inside `DeviceVariables`, and whether an import is atomic, is not known.

## Automations (smart rules)

### Model

A smart rule is one `TriggerModel`.

| Field | Meaning |
|---|---|
| `Id` | Allocated by the Core. Send 0 to create. |
| `TriggerTypeId` | The rule type. Ids from 10000 are safety rules. |
| `Name` | Free text. The app makes it unique by appending a number. |
| `Version` | Schema version of the type. Copy it from `TriggerVersions[type]` in the reconnect response. |
| `IsEnabled`, `IsDeleted` | Enable flag. The Core sets the soft-delete marker. |
| `ChangesetId` | Set by the Core and grows with every change. |
| `LimitingCondition*` fields | Optional gate. It can be a formula, a time window, a switch or a multi-value switch. Safety rules cannot have it. |
| `TriggerParameters` | The rule itself. The concrete subclass must match `TriggerTypeId` and `Version`. |
| `TriggerInputs` | Derived by the Core from the parameters. |
| `Priorities` | Map from output device id to priority. A higher value wins when rules compete. |

A rule is standalone. It refers to devices by id inside its parameters. There is no call that attaches a rule to a device. Actions inside rules are `ActionModel` entries, either a device action (`OutputDeviceId`, `OperationMode`, a list of value requests) or a notification. The app normalises action lists before saving. For hold-only lists it forces `OperationMode` to 0, removes command-style requests and sets `HoldAtTheEnd`. A client should do the same.

### The single update message

`upsertSmartRules` sends `MessageUpdateTriggerModelsRequest` with a list `TriggerModels`. It creates (`Id` 0), updates, enables and disables rules. Enabling or disabling means sending the whole model with a changed `IsEnabled`, not a patch. The answer is `MessageGenericResponse`, followed by `MessageTriggerModelsUpdatedBroadcast`. Deleting is a separate message, `deleteSmartRules` (`MessageDeleteTriggerModelsRequest` with `TriggerIds`), followed by `MessageTriggerModelsDeletedBroadcast`. `listSmartRules` is part of the reconnect response (`SmartRules`, each element a serialized `TriggerModel`, plus `TriggerVersions` and `SmartRuleErrorStatuses`).

Reordering rules for one device means changing `Priorities` on the affected rules and sending them in one update.

### Trigger types

"Editable" means the app has a page for the type. The other types stay in old data and the app cannot create them.

| Id | Type | Purpose |
|---|---|---|
| 1 | AlarmStateTrigger | Actions when an alarm is armed or disarmed |
| 2 | AlarmTrigger | Alarm with sensors, delays, phases and passcode |
| 3 | BlindsAccordingToSunTrigger | Tilt or lower blinds by sun position and temperature |
| 5 | CountdownTimerTrigger | Turn a target on for a timeout after an input fires |
| 6 | DelayedAirVentTrigger | Run a vent for a while after sensors trigger |
| 7 | DeviceStatusNotificationTrigger | Notify when devices fail or recover |
| 9 | EmergencyAlarmTrigger | Sensors trigger an always-on alarm |
| 10 | EquithermicRegulationTrigger | Heating curve control |
| 11 | EvaluationTrigger | Boolean formula selects true or false actions |
| 12 | FormulaTrigger | Formula that writes output variables |
| 13 | HysteresisControlTrigger | Thermostat hysteresis for heating and cooling outputs |
| 14 | KeepDefaultValuesTrigger | Hold devices at fixed values |
| 18 | LinkTrigger | Mirror master devices to slaves |
| 19 | MultiValueSwitchTrigger | Different actions per value of a multi-value switch |
| 20 | OnePushButtonBlindControlTrigger | One button cycles blinds |
| 21 | OpenBlindsOnSensorTrigger | Open blinds while a sensor is active |
| 22 | PresenceSimulationTrigger | Simulate presence with lights |
| 25 | PushButtonBlindControlTrigger | Up and down buttons drive blinds |
| 26 | PushButtonDimmingTrigger | Buttons dim lights |
| 27 | PushButtonTrigger | Actions on push, long push, double and triple click |
| 28 | ReduceHeatingTrigger | Lower the set point while windows are open |
| 29 | SensorNotificationTrigger | Notify on sensor state change |
| 30 | SimpleAlarmTrigger | Sensors plus an arm switch |
| 32 | SwitchTrigger | On, off and toggle actions driven by a switch |
| 33 | TemperaturePID2Trigger | PID regulation of heating and cooling outputs |
| 36 | WatchdogTrigger | Restart or check a target that stops responding |
| 37 | WeekScheduleTrigger | Weekly schedule by weekday time slots |
| 38 | MultiValueSwitchLightSceneSequenceTrigger | Buttons step a multi-value switch through scenes |
| 39 | LightingTimerTrigger2 | Motion-activated light with brightness gate |
| 40 | DailyScheduleTrigger | Actions in and out of a period, by time or sun event |
| 41 | SequencerTrigger | Timed step sequence of actions |
| 42 | ProtectBlindsFromMildWindTrigger | Retract blinds above a wind speed |
| 43 | AccessTrigger | Actions per access event |
| 44 | DoorBellTrigger | Actions while a doorbell rings or is in a call |
| 45 | ScriptTrigger | Free-form script with read-only and read-write variables |
| 10000 | ProtectBlindsFromStrongWindSafetyTrigger | Safety rule, cannot have a limiting condition |
| 10001 | LimitTemperatureSafetyTrigger | Safety rule that shuts devices off above a temperature |

Ids 4, 8, 15, 16, 17, 23, 24, 31, 34 and 35 are legacy types with no editor. Some types have older parameter shapes (Formula, Script, Hysteresis, Presence, OnePushButtonBlind, Sequencer and Access use `V10` to `V12` classes). The `Version` that the Core reports in `TriggerVersions` selects the shape, and a client should refuse a Core that reports an older shape than it supports. The field-level layout of each parameter class is in the generated schemas.

### Formula and script validation

- A formula rule (type 12) holds `InputVariables`, `OutputVariables`, an `EvaluationFormula` text and an `EvaluationMode` (0 on any input change, 1 on a custom event). A script rule (type 45) holds `Script`, `ReadOnlyVariables`, `ReadWriteVariables` and either a push button id or a list of trigger variables. Variables are `FormulaDeviceVariable` entries, and the `Abbreviation` is the name used in the text. Output variables are assigned with `:=`.
- The app checks the text with a copy of the parser before saving, and shows "Line N, message". This check never blocks saving, and there is no message that asks the Core to validate. `validateScript` is therefore client-side only and left out of the spec.
- The Core parses the text again when it stores the rule and reports problems asynchronously as trigger errors. The codes are 201 (`FormulaParseException`), 202 (`FormulaEvaluationException`) and 203 (`LiteralArgumentException`). Read them with `getSmartRuleErrors` (`MessageGetTriggerErrorsRequest`), or watch `smartRuleErrorStatusChanges`. Whether the Core stores a rule that has a parse error is derived from the app, not tested.
- A client that wants a local pre-check has to implement the grammar. The cheaper approach is to save, read the errors, and use `getScriptLastRunValues` with object type Smartrule.
- Some functions are rejected by Cores that are older than the function's protocol version, with a "please update your core unit" message.

### Sequence E. Motion-activated light

1. After the reconnect, keep `TriggerVersions[39]` as `v` and the device list. Confirm that the motion sensor `M` has a motion value and the light `L` has a switch state.
2. Build a `TriggerModel` with `Id` 0, `TriggerTypeId` 39, `Name` "zz-agent-test motion light", `Version` `v`, `IsEnabled` true, `MotionSensorIds` `[M]`, a `MotionSensorTimeout` of 5 minutes and `ActiveActions` with one device action. The action has `OutputDeviceId` `L`, `OperationMode` 0 and one value request, `On` on `SwitchState`. Apply the hold-only normalisation.
3. `upsertSmartRules` with the model. Expect `ErrorCode` 1.
4. Wait for `MessageTriggerModelsUpdatedBroadcast`. The rule appears with a non-zero `Id`, filled `TriggerInputs` and a `ChangesetId`. Match it by name and type.
5. `getSmartRuleErrors` for the new id. An empty list means the Core accepted all references.
6. Disable with `upsertSmartRules` and the same model with `IsEnabled` false. Delete with `deleteSmartRules`.

### Sequence F. Formula rule

1. Keep `TriggerVersions[12]`. The app treats versions 10 to 12 as old shapes, so check which shape the Core wants.
2. Define an input `{DeviceId: T, ValueLogType: RealTemperature, Abbreviation: "T"}` and an output `{DeviceId: H, ValueLogType: TemperatureSetPoint, Abbreviation: "Sp"}`.
3. Build the model with `EvaluationFormula` `Sp := IF(T < 18, 22, 19)` and `EvaluationMode` 0. Send it with `upsertSmartRules`.
4. Wait for the broadcast, then `getSmartRuleErrors`. Code 201 is a syntax error and the custom text holds the message. Fix the text and send the model again with the same `Id`.
5. Debug with `getScriptLastRunValues`.

### Schedules

Time-based rules keep their schedule inside the rule. `WeekScheduleTrigger` has a `Minutes` dictionary and an `InputMode`. `DailyScheduleTrigger` starts and ends by clock time or sun event with an adjustment. Sun times need the Core's clock and location (`updateCoreTime` sets the clock). Week plans and date ranges (`addWeekPlan`, `addDateRange` and similar) belong to door access cards and are not needed for smart rules.
