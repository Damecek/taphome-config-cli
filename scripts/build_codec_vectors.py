#!/usr/bin/env python3
"""Write tests/vectors/codec.json: byte vectors from the Python probe codec for the Rust codec tests.

Usage: python3 scripts/build_codec_vectors.py

Every value here is invented. The probe codec runs on the committed catalog data/catalog.json. Each vector
holds the decoded value (what the Rust codec must decode `hex` to) and the bytes (what it must encode `value`
to). The script checks that the Python codec reproduces the bytes from the decoded value.
JSON has no NaN or infinity, so non-finite floats are written as "NaN", "Infinity", "-Infinity" (the Rust
codec's convention).
"""
from __future__ import annotations

import json
import math
import os
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
os.environ["TAPHOME_CATALOG"] = str(ROOT / "data" / "catalog.json")
sys.path.insert(0, str(ROOT / "scripts"))
from probe import codec  # noqa: E402

OUT = ROOT / "tests" / "vectors" / "codec.json"
G1 = "00112233-4455-6677-8899-aabbccddeeff"
G2 = "11111111-2222-3333-4444-555555555555"
G0 = "00000000-0000-0000-0000-000000000000"


def finite(v):
    if isinstance(v, float) and not math.isfinite(v):
        return "NaN" if math.isnan(v) else ("Infinity" if v > 0 else "-Infinity")
    if isinstance(v, dict):
        return {str(k).lower() if isinstance(k, bool) else k: finite(x) for k, x in v.items()}
    if isinstance(v, list):
        return [finite(x) for x in v]
    return v


def rule(params, **extra):
    d = {"Id": 7, "TriggerTypeId": 12, "Name": "zz test rule", "Version": 13, "IsEnabled": True, "ChangesetId": 3,
         "TriggerParameters": params, "Priorities": {41: 8, 42: 0}}
    d.update(extra)
    return d


ACTION_DEVICE = {"ActionType": 1, "Parameters": {
    "$type": "DeviceActionParameters", "OperationMode": 1, "ValueLogTypeValueRequests": [
        {"ActionType": 2, "ValueLogType": 1, "ForceOneTimeOnly": True,
         "Parameters": {"DoubleParameters": {2: 0.5, 6: 1500.0}, "BoolParameters": {1: True, 11: False}}}]}}
ACTION_NOTIFY = {"ActionType": 2, "Parameters": {
    "$type": "NotifyActionParameters", "Silent": True, "Title": "t", "Message": "m",
    "ReceiverGroupIds": [1, 2], "ReceiverUserIds": [G1]}}

PROTO_PARAMS = [
    {"$type": "ProtoParameterString", "GenericValue": "x"},
    {"$type": "ProtoParameterString", "GenericValue": ""},
    {"$type": "ProtoParameterInt", "GenericValue": -2},
    {"$type": "ProtoParameterInt", "GenericValue": 2147483647},
    {"$type": "ProtoParameterLong", "GenericValue": -9223372036854775808},
    {"$type": "ProtoParameterLong", "GenericValue": 9223372036854775807},
    {"$type": "ProtoParameterDouble", "GenericValue": -1.25},
    {"$type": "ProtoParameterDouble", "GenericValue": float("nan")},
    {"$type": "ProtoParameterDouble", "GenericValue": float("-inf")},
    {"$type": "ProtoParameterBool", "GenericValue": True},
    {"$type": "ProtoParameterGuid", "GenericValue": G1},
    {"$type": "ProtoParameterUInt16", "GenericValue": 65535},
    {"$type": "ProtoParameterUInt32", "GenericValue": 4294967295},
    {"$type": "ProtoParameterULong", "GenericValue": 18446744073709551615},
    {"$type": "ProtoParameterFloat", "GenericValue": 0.25},
    {"$type": "ProtoParameterInt16", "GenericValue": -3},
    {"$type": "ProtoParameterUInt8", "GenericValue": 255},
    {"$type": "ProtoParameterInt8", "GenericValue": -128},
    {"$type": "ProtoParameterChar", "GenericValue": 65},
    {"$type": "ProtoParameterInt", "GenericValue": 0},
]

# (name, type, input value)
TYPED = [
    ("doc guid example", "LocationInfo", {"LocationId": G1}),
    ("location info with empty guid and bool", "LocationInfo", {"LocationId": G0, "Name": "home", "IsInTestMode": True}),
    ("doc root-first GenericDevice", "DeviceBase", {"$type": "GenericDevice", "CcuId": 3, "Name": "x"}),
    ("GenericDevice declared as derived type", "GenericDevice", {"$type": "GenericDevice", "CcuId": 3, "Name": "x"}),
    ("GenericDevice full", "DeviceBase", {
        "$type": "GenericDevice", "ID": 12, "Name": "Lamp", "Icon": 5, "Model": "m", "ManualChangeTimeout": 0,
        "ZoneId": G1, "CategoryId": G0, "HardwareParentId": -1, "SupportedActionTypes": [1, 2],
        "CurrentValues": {1: {"Type": 1, "Value": 21.5, "ChangedOnTHUtc": 123456}},
        "AggregatedValues": {5: {7: 1.5, 8: float("nan")}},
        "EnabledValueAggregations": {5: [{"$type": "DeviceValueAggregationExtended", "StatisticsFunction": 1,
                                          "AggregationUsage": 2, "HistorySubAggregationsHashCodes": [3, -4]},
                                         {"$type": "DeviceValueAggregationExtended",
                                          "HistorySubAggregationsHashCodes": []}]},
        "CcuId": 3, "DeviceProperties": {"a": "b", "empty": ""}, "WrappingDevicesIds": [0, 7]}),
    ("doc map entries", "TriggerModel", {"Priorities": {1: 0}}),
    ("rule with formula parameters", "TriggerModel", rule({
        "$type": "FormulaTriggerParameters", "EvaluationFormula": "a := b + 1", "EvaluationMode": 1, "EventDeviceId": 0,
        "InputVariables": [{"DeviceId": 41, "ValueLogType": 1, "Abbreviation": "b",
                            "ValueAggregation": {"$type": "DeviceValueAggregationExtended", "PeriodType": 2}}],
        "OutputVariables": [{"DeviceId": 42, "ValueLogType": 6, "Abbreviation": "a"}]})),
    ("rule with switch parameters and actions", "TriggerModel", rule({
        "$type": "SwitchTriggerParameters", "InputDeviceId": 5, "OnActions": [ACTION_DEVICE], "OffActions": [ACTION_NOTIFY]},
        TriggerTypeId=32, IsEnabled=False)),
    ("rule with tuple map and timespans", "TriggerModel", rule({
        "$type": "TemperaturePIDTriggerParameters", "ThermostatOutputMap": {3: [0.5, -1.0]},
        "SwitchOutputMap": {4: 300.0, 5: 0.0}, "Kp": 1.5, "MinUpdateRate": 1.0, "MaxUpdateRate": float("inf")},
        TriggerTypeId=33)),
    ("tombstone rule", "TriggerModel", {"Id": 9, "TriggerTypeId": 12, "Name": "old", "Version": 13, "IsDeleted": True}),
    ("timespans", "TriggerParametersBase", {"$type": "CountdownTimerTriggerParameters", "TargetDeviceId": 0,
                                            "Timeout": 300.0, "StartupDelay": 90.5, "TriggerDeviceIds": [1]}),
    ("timespan small units", "TriggerParametersBase", {"$type": "CountdownTimerTriggerParameters",
                                                       "Timeout": 0.0001, "StartupDelay": 1e-07}),
    ("timespan days and negative", "TriggerParametersBase", {"$type": "CountdownTimerTriggerParameters",
                                                             "Timeout": 172800.0, "StartupDelay": -3600.0}),
    ("timespan min and max", "TriggerParametersBase", {"$type": "CountdownTimerTriggerParameters",
                                                       "Timeout": float("-inf"), "StartupDelay": float("inf")}),
    ("timespan zero", "TriggerParametersBase", {"$type": "CountdownTimerTriggerParameters", "Timeout": 0.0}),
    ("log entry with tuples", "DeviceLogEntryBase", {"$type": "ValueChangedDeviceLogEntry",
                                                     "Values": [[1, 0.5], [6, 21.0]], "ChangeOriginId": 2}),
]

MESSAGES = [
    ("doc example A", "MessageGetBasicCorePropertiesRequest", {}, 5),
    ("doc example C ack", "MessageReceiveAcknowledgement", {"ReceivedMessagesStamps": [0x200000007, 0x30000000c]}, 0),
    ("doc example D reconnect", "MessageReconnectRequest", {
        "LocationId": G1, "Login": "admin", "SessionId": G2, "IsLocalAccount": True, "Platform": "Android",
        "ClientProtocolVersion": 89}, 1),
    ("login request", "MessageLoginRequest", {"LocationId": G1, "LoginOrEmail": "user", "PasswordHashOrAuthorizationToken": "ab", "IsLocalAccount": True}, 0),
    ("negative RequestsId", "MessageGetBasicCorePropertiesRequest", {}, -1),
    ("generic response with every scalar parameter", "MessageGenericResponse",
     {"ResponseFlagsId": 1, "ErrorMessage": "Ok", "ErrorCode": 1, "ProtoParameters": PROTO_PARAMS}, 9),
    ("reconnect response maps", "MessageReconnectResponse", {
        "ReconnectResultId": 1, "CurrentProtocolVersion": 89, "CoreVersionString": "2025.1.1", "DbHash": G2,
        "SmartRulesChangesetId": G0,
        "UserGroupUserConnections": {7: [G1, G2], 8: []},
        "GroupPermissionsForDashboards": {1: {G1: 3}, 2: {}},
        "UserGroups": {1: "admins"}, "SmartRules": ["deadbeef", ""], "SmartRuleErrorStatuses": {4: -5}}, 1),
    ("access logs with dates", "MessageGetAccessLogsRequest", {
        "DeviceId": 4, "UserId": G1, "FromUtc": "2025-03-04T05:06:07.1234567", "ToUtc": "9999-12-31T23:59:59.9999999"}, 2),
    ("dates at the edges", "MessageGetAccessLogsRequest", {
        "UserId": G0, "FromUtc": "0001-01-01T00:00:00.0000000", "ToUtc": "1970-01-01T00:00:00.0000000"}, 3),
    ("dates before 1970 and whole units", "MessageGetAccessLogsRequest", {
        "FromUtc": "1969-12-31T23:59:59.5000000", "ToUtc": "2026-01-01T00:00:00.0000000"}, 4),
    ("map with a null message value", "MessageGetAffectingTriggersInfoResponse", {"Actions": {5: None, 6: {"TriggerId": 6}}}, 5),
    ("unsigned and byte members", "MessageGetModuleInfoResponse", {"ProtocolVersion": 200}, 6),
    ("value changed broadcast", "MessageDeviceValueChangedBroadcast", {"DeviceId": 3, "ValueTypeId": 1, "Value": 0.1}, 7),
]


def vector(name, type_name, raw, given=None):
    value = codec.decode(type_name, raw)
    missing = {k for k, v in (given or {}).items() if k not in value and k != "$type" and v not in (0, G0, None)}
    assert not missing, "%s: the Python codec dropped %s" % (name, missing)
    assert codec.encode(type_name, value) == raw, name
    assert "$errors" not in json.dumps(value), name
    return {"name": name, "type": type_name, "value": finite(value), "hex": raw.hex()}


def main():
    vectors = [vector(n, t, codec.encode(t, v), v) for n, t, v in TYPED]
    for n, cls, fields, rid in MESSAGES:
        raw = codec.encode_message(cls, fields, rid)
        v = vector(n, "MessageBase", raw, fields)
        v.update({"class": cls, "fields": finite(fields), "requests_id": rid})
        vectors.append(v)
    raw = codec.encode_message("MessageGetBasicCorePropertiesRequest", {}, 5) + bytes.fromhex("c80101" "d2010261 62")
    vectors.append(vector("unknown fields preserved", "MessageBase", raw))
    raw = codec.encode("DeviceBase", {"$type": "GenericDevice", "CcuId": 3}) + bytes.fromhex("f80701")
    vectors.append(vector("unknown field in a base class", "DeviceBase", raw))
    OUT.parent.mkdir(parents=True, exist_ok=True)
    OUT.write_text(json.dumps(vectors, indent=1, ensure_ascii=False, allow_nan=False) + "\n", encoding="utf-8")
    print("wrote %s: %d vectors" % (OUT.relative_to(ROOT), len(vectors)))


if __name__ == "__main__":
    main()
