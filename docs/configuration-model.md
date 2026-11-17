# Configuration model

This page describes the objects a client reads and writes on a Core, and how the app changes them. Message sequences refer to operation ids from `openapi/operations/*.yaml`. Each operation is also in `openapi/taphome-core.yaml` as `POST /Core/messages/{RequestClass}`.

Status values. "Confirmed" means seen on a live Core (see `docs/verification.md`). Everything else is derived from the app code and not yet tested. Live write tests on Core 2025.2.32396 confirmed these sequences: C (virtual variable), the single-device parts of D (Packet Parser module and device), and creating, deleting and renaming as listed in "Confirmed write payloads" below. Sequences A, B, E and F are still plans derived from the app's behaviour. Sequences that create objects should use names with the prefix `zz-agent-test` and delete the objects afterwards.

## Object overview

| Concept | What it is | Identity | Status |
|---|---|---|---|
| Interface (CCU) | A communication channel. Examples are TapHome Bus, Modbus, Packet Parser, MQTT and virtual. | Integer `CcuId`, allocated by the Core | confirmed for the models `VirtualCCU`, `TapHomeBusCCU`, `PacketParserCCU` and `MqttCCU` |
| Module | A device that represents hardware or a connection target. Examples are a TapHome Bus board, a Modbus slave and a Packet Parser host. | Integer device id, allocated by the Core | confirmed for a Packet Parser HTTP module, derived for the rest |
| Device | Anything with values and actions. Examples are a light, a button, a thermometer or a module. | Integer `ID`, allocated by the Core | confirmed for the model name families `TapHomeBus*`, `PacketParser*` and `Virtual*` |
| Terminal | A physical input or output of a module that is configured as a device. It is a child device with `HardwareParentId` set to the module id. | Integer device id | derived |
| Zone, category, dashboard | A grouping of devices. All three are `Dashboard` objects of a dashboard set. | Guid, allocated by the client | derived |
| Smart rule | An automation. The code calls it a trigger. | Integer `Id`, allocated by the Core, may reuse the id of a deleted rule | confirmed for create and delete |

On the test Core, the reconnect response held 136 devices, 4 interfaces, 88 smart rules and 24 dashboards (confirmed).

## Interfaces, modules and devices

### Where a client gets the model

The reconnect response carries all interfaces (`Ccus`), all devices with their current values, the smart rules, the dashboards, the permissions and the Core properties. There is no "list devices" request. Operation `getDeviceListOnLogin` documents this. Afterwards the Core pushes `MessageDeviceListUpdatedBroadcast` (full lists), `MessageDeviceUpdatedBroadcast` (one device) and `MessageDeviceValueChangedBroadcast` (one value). All of these are confirmed, as is the single-message initial sync. The device list is in the reconnect response, not in the login response (the login response was not checked for devices). `MessageDeviceAggregatedValueChangedBroadcast` also arrives passively and can be ignored.

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

Values are doubles keyed by a `ValueLogType` id. Booleans are 0 and 1. The ids come from the declaration order of the enum in the app. A live Core confirmed 48 SwitchState, 22 OperationMode, 5 RealTemperature, 6 TemperatureSetPoint, 3 Humidity, 42 AnalogOutputValue, 67 AnalogOutputDesiredValue, 7 DeviceStatus, 62 VariableState, 44 ReedContact and 69 for a clock. The others are derived.

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
| Variable | `VariableState` (62) |

`DeviceStatus` below 256 is a plain enum (0 OK, 1 BatteryLow, 2 BatteryCritical, 3 Damaged, 4 CommunicationError, 5 UnknownError, 6 AuthorizationError, 7 ConfigurationError). From 256 it is packed, with the error count in bits 8 to 15, the warning count in bits 16 to 23 and the info count in bits 24 to 31. The full table is in the generated schemas.

### Device type

`DeviceType` is the semantic role, for example Light 20, PowerOutlet 30, MotionSensor 1010, AirTemperature 3001 or Module 7001. It is set through the `DeviceType` property, as an integer in a string. The allowed types depend on the device kind. Changing the type in the app also rewrites type-specific defaults in the same write, such as `OnStateName` and `OffStateName`. Strings such as `${general_on}` are translation keys, not literal text.

### Zones and categories

Zones and categories are dashboards. A `Dashboard` belongs to a `DashboardSet`, where set 0 holds dashboards, set 1 zones and set 2 categories. The client picks the Guid. Operations. `addZone` (`MessageAddDashboardRequest`), `updateZoneProperties`, `deleteZone`, `assignDevicesToZone` and `assignDevicesToCategory`. The last two are a property update that sets `ZoneId` or `CategoryId` on the devices.

### Id allocation

| Object | Who picks the id |
|---|---|
| Interface, device, module, smart rule | The Core. The client sends 0 or leaves it out, and reads the id from the response or the broadcast. A device create answers with the new device. A smart rule create answers with no id. |
| Zone, category, dashboard | The client, as a new Guid. |
| Long-running task | The client, as a Guid. |
| Objects inside one Packet Parser or Modbus config | Placeholder ids, negative integers that exist only inside that message. The Core assigns real ids (confirmed for Packet Parser) and sets the child's `HardwareParentId` to the new module id. |

Two live findings change what a client may assume about ids. First, the Core frees the id of a deleted device. After a delete, the highest device id went back to its old value, so a later create can get the same id again. A client must not keep a deleted id as a stable reference. Second, a new smart rule did not get "highest id plus 1". The Core reused the id of an existing soft-deleted rule. A client therefore finds its new rule by name and changeset in `MessageTriggerModelsUpdatedBroadcast` or in the next sync, never by guessing the id.

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

### Sequence C. Create a virtual variable (confirmed)

1. `addVirtualVariable` with `DeviceName` "zz-agent-test-var", `Icon` 0, `Interface` "Variable", `Model` "VirtualVariableGeneral" and `DeviceTypeId` 0. Send no `CcuId` and no `DeviceProperties`.
2. Read `AddedDevice.ID` from the response. The Core attached the device to the virtual CCU and set the serial to `VIRT-<guid>`.
3. `updateMultipleDevicesProperties` for name, unit or start value. Read `AddedDevice.DeviceProperties` for the key names. The defaults are `LogValues`, `IsIgnored`, `DeviceType`, `ZoneId`, `CategoryId`, `HardwareParentId`, `SerializedValueLogTypeConversions`, `IsReadOnly` and `IconId`.
4. Clean up with `deleteDevice` (`MessageDeleteDeviceRequest` with `DeviceId` and `ForceDelete` false).

The other virtual models in the table were not tried.

## Packet Parser and Modbus

### What a template is

A template is an XML file with a module, its devices and their scripts, plus parameters such as an IP address. Templates exist only on the client. The app reads the XML, shows the parameters, replaces every `$[ParamId]` token in names and property values, converts the result to JSON and sends it. The Core never sees the XML. A client can therefore skip templates and build the JSON itself. The operations are `exportPacketParserTemplate`, `loadLocalTemplateFile` and `downloadCommunityTemplates`, which are client-side only and listed in `openapi/operations/EXCLUDED.md`.

### The config message

`importPacketParserTemplate` sends `MessageHttpApiConfigRequest` with `CcuId` (field 1) and `ConfigJson` (field 2). The name comes from the interface that was called HTTP API before protocol 66. `importModbusTemplate` sends `MessageModbusConfigRequest` with the same layout. Both answer with `CcuId`, `AddedDevices` and `RemovedDevices`. A failure is `MessageGenericResponse`, for example error 26 when the Core is too old.

`ConfigJson` is JSON with PascalCase names, nulls omitted, enums as numbers and Guids as text. A `ConfigJson` that also carried the template metadata keys `Name`, `Id` and `Revision` was accepted by the Core.

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
| `StayConnected` | Keep the socket open. `True` or `False`. A hand-built HTTP module did not keep this key after creation. |
| `InternalPollInterval` | Poll interval of the module. The unit is not known. The values "3600000" (module) and "60000" (device) were accepted and kept as sent. |
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

The only debug channel after saving is `getScriptLastRunValues` (`MessageGetScriptLastRunValuesRequest`). It takes a `LastRunId` with an object type (Device 0, Ccu 1, Smartrule 2), the object id and the `ScriptKey`, which is the property name. The response has the variable values of each capture phase, the result, the run time and the duration. Confirmed for a Packet Parser device with `ScriptKey` "ReadStateScriptPacketParser": the response has `Result` and `RunTimestampUtc`. `Result` is a `DataType`. For a double, `SerializerValueTypeId` is 4 and `SerializerValueBytes` holds the IEEE-754 bits of the value as an int64. Before the script ran, the response held only the echoed `LastRunId`. The response also holds one snapshot per capture phase with the script variables (confirmed: phase 0 held the variable, NaN before its first value, and phase 1 was empty), and the CLI decodes it. Calls to `ADDERROR`, `ADDWARNING` and `ADDINFO` appear as device errors, readable with `getDeviceErrors`.

### Sequence D. Packet Parser module with one device (confirmed for an HTTP module with one variable)

1. Find the interface with model `PacketParserCCU` in the cached list. If it is missing, `addPacketParserInterface` with `Model` "PacketParserCCU" and read `CcuId` from the response.
2. Build `ConfigJson` with one module (`Id` -1, model `PacketParserModuleHttp`, properties `IpAddress`, `Port`, `StayConnected`, `InternalPollInterval`) and one device (`Id` -2, model `PacketParserSwitch`, properties with the read and write script texts). Prefix the names with "zz-agent-test".
3. `importPacketParserTemplate` with `CcuId` and `ConfigJson`.
4. Read the module id and device ids from `AddedDevices`.
5. The state script runs at once, so the value shows in the next sync or in a `deviceValueChanged` broadcast within seconds. `getScriptLastRunValues` for the script key shows the result.
6. Change scripts with `updateMultipleDevicesProperties` and check again. The new script also runs at once, with no restart.
7. Clean up. Delete the device first, then the module (`deleteDevice`). The module was deleted only after its child.

The same flow is expected to work for Modbus with `addModbusInterface` and `importModbusTemplate`, but Modbus was not tested. Whether the Core remaps placeholder ids inside `DeviceVariables`, and whether an import is atomic, is not known.

## Automations (smart rules)

### Model

A smart rule is one `TriggerModel`.

| Field | Meaning |
|---|---|
| `Id` | Allocated by the Core. Send 0 to create. |
| `TriggerTypeId` | The rule type. Ids from 10000 are safety rules. |
| `Name` | Free text. The app makes it unique by appending a number. |
| `Version` | Schema version of the type. Copy it from `TriggerVersions[type]` in the reconnect response. |
| `IsEnabled`, `IsDeleted` | Enable flag. The Core sets the soft-delete marker. A created rule keeps `IsEnabled` as sent. A deleted rule stays in the list with `IsDeleted` true, no parameters, and its name, type and version. |
| `ChangesetId` | Set by the Core and grows with every change. |
| `LimitingCondition*` fields | Optional gate. It can be a formula, a time window, a switch or a multi-value switch. Safety rules cannot have it. |
| `TriggerParameters` | The rule itself. The concrete subclass must match `TriggerTypeId` and `Version`. |
| `TriggerInputs` | Derived by the Core from the parameters. Confirmed: the Core filled it for a new formula rule, so a client does not send it. |
| `Priorities` | Map from output device id to priority. A higher value wins when rules compete. The Core fills it for a new rule (priority 8 for the one output in the test). |

A rule is standalone. It refers to devices by id inside its parameters. There is no call that attaches a rule to a device. Actions inside rules are `ActionModel` entries, either a device action (`OutputDeviceId`, `OperationMode`, a list of value requests) or a notification. The app normalises action lists before saving. For hold-only lists it forces `OperationMode` to 0, removes command-style requests and sets `HoldAtTheEnd`. A client should do the same.

### The single update message

`upsertSmartRules` sends `MessageUpdateTriggerModelsRequest` with a list `TriggerModels`. It creates (`Id` 0), updates, enables and disables rules. Enabling or disabling means sending the whole model with a changed `IsEnabled`, not a patch. Enabling, disabling and a changed `EvaluationFormula` of an existing formula rule were confirmed on a live Core. Each gave `ErrorCode` 1, a raised `ChangesetId` and an otherwise unchanged stored model. Updates of other rule types were not tried. The answer is `MessageGenericResponse`, followed by `MessageTriggerModelsUpdatedBroadcast`. Deleting is a separate message, `deleteSmartRules` (`MessageDeleteTriggerModelsRequest` with `TriggerIds`), followed by `MessageTriggerModelsDeletedBroadcast`. `listSmartRules` is part of the reconnect response (`SmartRules`, each element a serialized `TriggerModel`, plus `TriggerVersions` and `SmartRuleErrorStatuses`).

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
4. Wait for `MessageTriggerModelsUpdatedBroadcast`. The rule appears with a non-zero `Id`, filled `TriggerInputs` and a `ChangesetId`. Match it by name and type. The `Id` can be one that belonged to a deleted rule.
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

## Confirmed write payloads

These payloads were sent to Core 2025.2.32396 (protocol 89) and accepted. They are shown in the JSON form of `scripts/probe`, where the `$type` key is left out and `<id>` stands for an id from an earlier response. Each answer is `MessageGenericResponse` with `ErrorCode` 1 unless a typed response is named. The test objects were named `TEST-zz-agent-test-*` and were removed afterwards.

Add a virtual variable (`MessageAddDeviceRequest`). The answer is `MessageAddDeviceResponse` with `AddedDevice` and the interface list. One `MessageDeviceListUpdatedBroadcast` follows.

```
{"DeviceName": "<name>", "Icon": 0, "Interface": "Variable", "Model": "VirtualVariableGeneral", "DeviceTypeId": 0}
```

Rename a device (`MessageUpdateMultipleDevicesPropertiesRequest`). One `MessageDeviceUpdatedBroadcast` follows, and the new name is in the next sync.

```
{"DeviceIds": [<id>], "DeviceProperties": {"Name": "<new name>"}}
```

Update a script (same message). The new script ran at once.

```
{"DeviceIds": [<id>], "DeviceProperties": {"ReadStateScriptPacketParser": "8"}}
```

Create a smart rule (`MessageUpdateTriggerModelsRequest`). The request leaves out `Priorities` and `TriggerInputs`. The Core fills both and keeps `IsEnabled`. Variables are `FormulaDeviceVariable` entries, here on value type 62 (VariableState) of one variable device. One `MessageTriggerModelsUpdatedBroadcast` follows. The rule has the `Id` of an old deleted rule, so find it by name.

```
{"TriggerModels": [{
  "Id": 0, "TriggerTypeId": 12, "Name": "<name>", "Version": 13, "IsEnabled": false, "ChangesetId": 0,
  "TriggerParameters": {
    "InputVariables":  [{"DeviceId": <id>, "ValueLogType": 62, "Abbreviation": "Va"}],
    "OutputVariables": [{"DeviceId": <id>, "ValueLogType": 62}],
    "EvaluationFormula": "Va"}}]}
```

Delete a smart rule (`MessageDeleteTriggerModelsRequest`). The rule stays as a tombstone. One `MessageTriggerModelsDeletedBroadcast` follows.

```
{"TriggerIds": [<id>]}
```

Create a Packet Parser module with one device (`MessageHttpApiConfigRequest`, `CcuId` is the id of the interface with model `PacketParserCCU`). The answer is `MessageHttpApiConfigResponse` with `CcuId` and `AddedDevices` (module first, then the device). One `MessageDeviceListUpdatedBroadcast` follows. `ConfigJson` is one JSON string, shown here pretty-printed. The Core derives the child's `HardwareParentId` from the nesting.

```
{"CcuId": <id>, "ConfigJson": {
  "ProtocolVersion": 66,
  "Modules": [{
    "Name": "<module name>", "Model": "PacketParserModuleHttp", "Id": -1, "HardwareParentId": 0,
    "CategoryId": "00000000-0000-0000-0000-000000000000", "ZoneId": "00000000-0000-0000-0000-000000000000",
    "DeviceProperties": {"IpAddress": "<host>", "Port": "<port>", "StayConnected": "False",
                         "InternalPollInterval": "3600000", "UseHttps": "False"},
    "Devices": [{
      "Name": "<device name>", "Model": "PacketParserVariable", "Id": -2, "HardwareParentId": 0,
      "CategoryId": "00000000-0000-0000-0000-000000000000", "ZoneId": "00000000-0000-0000-0000-000000000000",
      "DeviceProperties": {"ReadStateScriptPacketParser": "7", "InternalPollInterval": "60000", "IsReadOnly": "True"}}]}]}}
```

Delete a device (`MessageDeleteDeviceRequest`). Delete a child before its module. One `MessageDeviceListUpdatedBroadcast` follows.

```
{"DeviceId": <id>, "ForceDelete": false}
```

Read a script result (`MessageGetScriptLastRunValuesRequest`).

```
{"LastRunId": {"ObjectIdType": 0, "ObjectId": <device id>, "ScriptKey": "ReadStateScriptPacketParser"}}
```

What the Core adds on a create. A device gets a serial (`VIRT-<guid>`, `PacketParser--<8 hex>` for the module, `PacketParser-Variable-<8 hex>` for the device) and all default properties. A Packet Parser module got 23 keys, such as `ReadScriptPacketParser`, `WriteScriptPacketParser`, `CustomVariables`, `DeviceVariables`, `ServiceAttributesScriptsPacketParser`, `ServiceActionsScriptsPacketParser`, `TemplateIdentity`, `User`, `Password`, `IgnoreSSLErrors` and `HttpAuthentification` ("None"). It has `Interface` "IHardwareParentModule" and `DeviceType` 7001. The device's `ReadScriptPacketParser` was filled with a commented example script. A client should read `AddedDevices[].DeviceProperties` for the real key names instead of hard-coding them.
