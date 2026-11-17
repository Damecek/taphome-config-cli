"""Unit tests for codec.py. Run: cd scripts/probe && python3 -m unittest (needs work/catalog/official.json)."""
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from probe import codec  # noqa: E402

LOC = "00112233-4455-6677-8899-aabbccddeeff"
H = bytes.fromhex


class CodecTests(unittest.TestCase):
    def test_example_a_basic_core_properties(self):
        got = codec.encode_message("MessageGetBasicCorePropertiesRequest", {}, 5)
        self.assertEqual(got, H("f2080008" "05"))
        self.assertEqual(codec.decode_message(got), {"$type": "MessageGetBasicCorePropertiesRequest", "RequestsId": 5})

    def test_example_c_ack(self):
        got = codec.encode_message("MessageReceiveAcknowledgement",
                                   {"ReceivedMessagesStamps": [0x200000007, 0x30000000c]})
        self.assertEqual(got, H("a2060c" "0887808080" "20" "088c80808030"))
        self.assertEqual(codec.decode_message(got)["ReceivedMessagesStamps"], [0x200000007, 0x30000000c])

    def test_example_d_reconnect(self):
        fields = {"LocationId": LOC, "Login": "admin", "SessionId": "11111111-2222-3333-4444-555555555555",
                  "IsLocalAccount": True, "Platform": "Android", "ClientProtocolVersion": 89}
        got = codec.encode_message("MessageReconnectRequest", fields, 1)
        want = H("f2063c" "0a12093322110055447766118899aabbccddeeff" "1205" + b"admin".hex()
                 + "1a12091111111122223333114444555555555555" "2001" "2a07" + b"Android".hex() + "5859" "0801")
        self.assertEqual(got, want)
        back = codec.decode_message(got)
        self.assertEqual(back["SessionId"], fields["SessionId"])
        self.assertEqual(back["$type"], "MessageReconnectRequest")

    def test_guid_example(self):
        self.assertEqual(codec.encode("LocationInfo", {"LocationId": LOC}),
                         H("0a12093322110055447766118899aabbccddeeff"))

    def test_roundtrip_map_and_nested_subtype(self):
        msg = {
            "ReconnectResultId": 1, "CurrentProtocolVersion": 89, "CoreVersionString": "2025.1.1",
            "GroupPermissionsForDevices": {1: {2: 3, 4: -5}},         # map of map
            "UserGroupUserConnections": {7: [LOC, "11111111-2222-3333-4444-555555555555"]},  # map of list
            "UserGroups": {1: "admins"},
            "SmartRules": ["deadbeef", "00"],
        }
        back = codec.decode_message(codec.encode_message("MessageReconnectResponse", msg, 1))
        self.assertEqual(back["RequestsId"], 1)
        for k, v in msg.items():
            self.assertEqual(back[k], v, k)
        gr = {"ErrorCode": 3, "ProtoParameters": [{"$type": "ProtoParameterString", "GenericValue": "x"},
                                                  {"$type": "ProtoParameterInt", "GenericValue": -2}]}
        back = codec.decode_message(codec.encode_message("MessageGenericResponse", gr, 9))
        self.assertEqual(back["ProtoParameters"][1], {"$type": "ProtoParameterInt", "GenericValue": -2})

    def test_deep_inheritance(self):
        raw = codec.encode("AlexaResponse", {"$type": "AlexaSetValuesResponse", "Message": "m", "DeviceId": 4})
        back = codec.decode("AlexaResponse", raw)
        self.assertEqual((back["$type"], back["Message"], back["DeviceId"]), ("AlexaSetValuesResponse", "m", 4))

    def test_unknown_fields_preserved(self):
        raw = codec.encode_message("MessageGetBasicCorePropertiesRequest", {}, 5) + H("c80101")  # tag 25 varint 1
        d = codec.decode_message(raw)
        self.assertEqual(d["$unknown"][0]["tag"], 25)
        self.assertEqual(codec.encode("MessageBase", d), raw)

    def test_dates(self):
        s = "2025-03-04T05:06:07.1234567"
        self.assertEqual(codec.dec_datetime(codec.enc_datetime(s)), s)
        self.assertEqual(codec.dec_timespan(codec.enc_timespan(90.5)), 90.5)

    def test_catalog_types_all_resolve(self):
        c = codec._codec()
        known = codec.INT_TYPES | {"string", "bool", "double", "float", "byte[]", "Guid", "DateTime", "TimeSpan"}

        def leaves(ts):
            cl = codec.classify(ts)
            if cl[0] == "name":
                return [cl[1]]
            parts = cl[1:] if cl[0] != "tuple" else cl[1]
            return [x for p in parts for x in leaves(p)]

        for t in c.catalog["types"]:
            if t.get("proto_lib") != "protobuf-net":
                continue
            for _, name, ts, *_ in c.member_specs(t):
                for n in leaves(ts):
                    self.assertTrue(n in known or n.split(".")[-1] in c.types, "%s.%s: %s" % (t["name"], name, n))


if __name__ == "__main__":
    unittest.main()
