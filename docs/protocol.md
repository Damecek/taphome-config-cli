# Wire protocol

This page describes the byte-level protocol between a client and the TapHome Core, precisely enough to write a client. Status values. "Confirmed" means seen on a live Core (see `docs/verification.md`). Everything else is derived from the app code and the generated serializers, and untested. Byte examples were computed from those rules and decoded by `scripts/probe`, not captured from the Core, unless the text says so.

## Framing

- Transport is a WebSocket on port 80, path `/Core`. The server is `websocket-sharp`. The handshake is a standard RFC 6455 upgrade with no sub-protocol and no credentials.
- One binary WebSocket message is one complete protobuf serialization of `MessageBase`. There is no length prefix and no envelope. Text frames are rejected. Confirmed for `/Identify`, where the single frame is a bare `LocationInfo`. For `/Core`, confirmed for login, reconnect and acknowledgement frames.
- The app does not propose compression, so every frame is uncompressed. A compressed frame from the server aborts the app's socket.
- The app splits outgoing messages into WebSocket fragments of 1016 bytes. A client may send one unfragmented frame (derived).
- The serializer is protobuf-net 2.4.6 with a precompiled model. Plain `Serialize` is used, not the length-prefixed variant.

## MessageBase

| Member | Tag | Type | Notes |
|---|---|---|---|
| `RequestsId` | 1 | int32 varint | Written only when not 0. |
| subtype | 100 to 1027 | nested message | One field per message class. The field number is the include tag. |
| `MessageType` | not serialized | byte | Fixed per class. |

`MessageType` values are 0 None, 1 Request, 2 Response, 3 Broadcast, 4 Stream, 5 Acknowledgement, 6 Container and 7 Progress.

## protobuf-net conventions

### Subtype before base fields

A message class is written as a length-delimited field in `MessageBase`. The field number is the include tag and the payload holds the class's own fields. The key is `(tag << 3) | 2` as a varint. Examples: tag 100 is `a2 06`, 108 is `e2 06`, 110 is `f2 06` and 142 is `f2 08`.

protobuf-net writes the subtype field first and field 1 (`RequestsId`) after it. The reader needs the subtype first to create the instance. A client must write them in that order (derived, from the generated reader).

Example. `MessageGetBasicCorePropertiesRequest` has include tag 142 and no fields. With `RequestsId` 5 the frame is `f2 08 00 08 05`. The bytes are the key, a length of 0, then field 1 with value 5.

### Root-first hierarchy rule (confirmed)

The serializer always writes a class hierarchy from the root base class, even when a field is declared with a derived type. Each inheritance level adds one nested length-delimited field under its include tag.

A `GenericDevice` in a device list is written as the `DeviceBase` fields at the top level, plus the `GenericDevice` fields nested under include tag 100. A client that encodes `GenericDevice` directly as the top-level type produces data the Core and the probe cannot decode. The same applies to smart rule parameters (`TriggerParametersBase` subclasses nest under their include tags), to `GenericCcu` and to every other class with includes. The OpenAPI schemas record the chain in `x-proto-include-path`.

### Scalars

| CLR type | Encoding | Default handling |
|---|---|---|
| `int`, `short`, `long`, `byte`, enums stored as ints | varint. A negative `int32` takes 10 bytes (sign extended). | omitted when 0 |
| `bool` | varint 0 or 1 | omitted when false |
| `string` | length-delimited UTF-8 | omitted when null. An empty string is written. |
| `byte[]` | length-delimited | omitted when null |
| `double` | fixed64 | omitted when 0 |
| `float` | fixed32, in a few types only | |
| nullable `T?` | written only when it has a value. The value is always written, even when 0. | |
| `sint64` zigzag | only in `HistoryItemsSession` | |

Most enums are stored in an int property whose name ends in `Id`, for example `LoginResultId`, so they are plain varints.

### Repeated fields and maps

- A list or array of messages is a repeated length-delimited field, one entry per element.
- A repeated scalar is not packed. Each element has its own key. Only `HistoryItemsSession` and `DeviceValueAggregationExtended` use packed fields.
- A dictionary is a repeated length-delimited field. Each entry is a message with the key in field 1 and the value in field 2. Both are always written, even when 0 or empty. An entry for `{"a": "b"}` is `<key> 06 0a 01 61 12 01 62`. An entry from int 1 to int 0 is `<key> 04 08 01 10 00`.
- A map value that is itself a list or map nests one more entry level.

### Guid

A Guid is a nested message with two fixed64 fields taken from the .NET byte order (`Guid.ToByteArray()`, which Python calls `uuid.UUID(...).bytes_le`). Field 1 holds bytes 0 to 7 and field 2 holds bytes 8 to 15. An empty Guid is a zero-length message.

The Guid `00112233-4455-6677-8899-aabbccddeeff` as field 1 is

```
0a 12 09 33 22 11 00 55 44 77 66 11 88 99 aa bb cc dd ee ff
```

Confirmed in use: session ids and location ids decode correctly in live messages.

### TimeSpan and DateTime

A `TimeSpan` is a nested message. Field 1 is the value as a zigzag varint. Field 2 is the scale, the largest unit that divides the ticks exactly. Field 3 (kind) is not written. The scales are 0 Days, 1 Hours, 2 Minutes, 3 Seconds, 4 Milliseconds, 5 Ticks and 15 MinMax. Field 1 is omitted when the value is 0. Field 2 is omitted when the scale is Days. One tick is 100 ns.

| Value | Payload |
|---|---|
| 5 minutes | `08 0a 10 02` (value 5 zigzag is 10, scale 2) |
| 1 second | `08 02 10 03` |
| 0 | empty |
| `TimeSpan.MaxValue` | `08 02 10 0f` (value 1 zigzag is 2, scale 15) |

A `DateTime` is the same message, holding the time span since 1970-01-01T00:00:00. The kind is ignored, with no time zone conversion, and the app reads it back as unspecified. Fields named `...Utc` hold UTC. Many timestamps are plain integer fields, and a live Core confirmed three units.

| Field | Unit |
|---|---|
| `long` fields named `...TUtc`, for example `FirstLoginOnTUtc`, `PerformedOnTUtc`, `ValidToTUtc`, `RunTimestampUtc` | .NET ticks, 100 ns since 0001-01-01 UTC. `DateTime.MaxValue.Ticks` marks "no end". |
| `ValueLog.ChangedOnTHUtc`, `TapHomeTime` in log and error entries | TapHome time, seconds since 2016-01-01 UTC |
| Core property `Current Time` (uint32) | seconds since 2000-01-01, and it equals the Core's local wall-clock time, not UTC |

A `DateTime` field such as `LastHistoryCloudFlushOnTUtc` uses the bcl encoding above.

The Next line always writes both fields. Readers accept both forms.

## Requests, responses and numbering

- The client keeps one counter per socket. Each request that expects an answer gets `RequestsId` = previous + 1. The reconnect request is 1, the next request is 2, and so on.
- The login request uses `RequestsId` 0, because the login socket does not use the counter. So field 1 is not written.
- A response, or a progress message, carries the `RequestsId` of its request. Confirmed for the reconnect response, which echoes 1. The app also accepts `-1` as "any pending request" and accepts a reconnect response with a different id.
- Requests that expect no answer (subscriptions, task cancellation) still carry a `RequestsId` and are still acknowledged.
- The Core numbers its own messages with a running `RequestsId`. Confirmed: acknowledgements from the Core carry a non-zero `RequestsId`. A client must ignore that value on acknowledgements and broadcasts.
- Long-running tasks answer with `MessageLongRunningTaskProgress` (include tag 426) under the request's `RequestsId`. Fields are `TaskId` (Guid), `Progress` (int) and `Parameters`.
- The `MessageTapHomeApi...` read messages (location info, discovery, get device value, get multiple device values) are tunnel messages. A client session got no answer within 60 s and stayed usable, so do not wait for them on a LAN socket (confirmed).
- Before the reconnect handshake succeeds, a socket may receive only the reconnect response, acknowledgements, generic responses, containers and broadcast classes. The app closes the socket on anything else.

## Acknowledgements (protocol 23 or higher)

| Item | Rule | Status |
|---|---|---|
| Message | `MessageReceiveAcknowledgement`, include tag 100, field 1 repeated int64, not packed. | confirmed |
| Stamp | `MessageType` shifted left by 32 bits, combined with `RequestsId`, as int64. A response with id 7 is `0x0000000200000007`. A broadcast with id 12 is `0x000000030000000c`. A request with id 1 is `0x0000000100000001`. | confirmed for the request stamp |
| Core to client | The Core acknowledges each request before it sends the response. | confirmed |
| Client to Core | Collect the stamp of every received message that needs one, and send one acknowledgement every 4 s. | derived |
| Exempt | Acknowledgements, streams, `MessageLoginRequest`, `MessageCheckTunnelStatusRequest` and the inner messages of a container. | derived |
| Timeout | The app closes the socket when a sent stamp is unacknowledged for 10.5 s (close code 1008). The Core probably applies the same rule to clients. | derived |

An acknowledgement for stamps 0x200000007 and 0x30000000c encodes as `a2 06 0c 08 87 80 80 80 20 08 8c 80 80 80 30`. The short live runs closed their sockets before 10 s, so the Core's timeout behaviour is not tested.

## Ping

The app sends a WebSocket ping every 6 s and expects a pong within 10 s. Without a pong it closes the socket with code 1002. The server also answers pings with the same payload. Whether the Core requires pings from a client is not tested.

## Containers

`MessageContainer` (include tag 383, protocol 47 or higher) has one field, a repeated `bytes`. Each element is a complete serialized `MessageBase`. The Core can use it to batch messages. The client processes each inner message normally, acknowledges the container and not the inner messages. The Official app never sends containers. Login, setup and forgotten-password sockets unwrap the first element if a container arrives.

The Next line adds `MessageCompressed` (include tag 99, protocol 90 or higher) with a Brotli payload. See `docs/architecture.md`.

## Broadcasts and subscriptions

A broadcast has `MessageType` 3. The Core sends it after a change, to every connected client, including the one that caused the change (confirmed for the writer). A client updates its cache from broadcasts. The OpenAPI document lists each broadcast as a webhook.

| Broadcast | Meaning |
|---|---|
| `MessageDeviceListUpdatedBroadcast` | Full replacement of devices and interfaces. Sent once after a device add, a device delete and a Packet Parser import (confirmed). |
| `MessageDeviceUpdatedBroadcast` | One full device. Sent once after a property update (confirmed). |
| `MessageDeviceValueChangedBroadcast` (include tag 107) | One value changed. |
| `MessageDeviceAggregatedValueChangedBroadcast` | Aggregated statistics values. Passive, confirmed. |
| `MessageTriggerModelsUpdatedBroadcast`, `MessageTriggerModelsDeletedBroadcast` | Smart rule changes. Sent once after a create and once after a delete (confirmed). |
| Permission and dashboard broadcasts | Changes with a changeset id. |

Device values, device lists, device updates and smart rule changes arrive without a subscription (confirmed). `MessageDeviceValueChangedBroadcast` and `MessageDeviceAggregatedValueChangedBroadcast` (about one every 2 s) arrive on an idle session. The aggregated one has no request that enables it, and a client can ignore it after acknowledging it. A `getDeviceLog` request with `ShouldSubscribe` true produced no extra log broadcast class in a 15 s test, only the same value-change broadcasts. The app renews the log subscription every 5 s. The device and rule broadcasts below were observed once per write on the socket that sent the write.

| Subscribe request | Include tag |
|---|---|
| `MessageSubscribeBroadcastRequest` (with `DeviceId`) | 350 |
| `MessageDeviceLogSubscribeBroadcastRequest` | 364 |
| `MessageDeviceErrorsSubscribeBroadcastRequest` | 368 |
| `MessageTriggerErrorsSubscribeBroadcastRequest` | 441 |

## Generic response and errors

Any request can be answered by `MessageGenericResponse` (include tag 102). Write operations normally are.

| Tag | Field | Type |
|---|---|---|
| 1 | `ResponseFlagsId` | int16. 0 None, 1 Handled, 2 Failed. |
| 2 | `ErrorMessage` | string |
| 3 | `ErrorCode` | int32 |
| 4 | `ProtoParameters` | list of `ProtoParameterBase` |

Success is `ErrorCode` 1, with `ErrorMessage` "Ok" (confirmed for every write test). Device actions count `ResponseFlags` Handled as success. Some app flows treat any answer as success, and a client should always check both fields.

`CommonErrorCodes` (derived).

| Code | Name | Code | Name |
|---|---|---|---|
| 1 | Ok | 15 | DeserializationException |
| 2 | Unauthorized | 16 | IncompatibleCoreSmartRuleVersion |
| 3 | Exception | 17 | UpdateStarted |
| 4 | ParameterTooShortOrLong | 18 | UpdateFinished |
| 5 | AlreadyExist | 19 | UpdateFailed |
| 6 | DoesNotExist | 20 | AtLeastOneAdmin |
| 7 | InvalidRequest | 21 | CoreOffline |
| 8 | MissingEmail | 22 | RequestNotSupported |
| 9 | WrongEmailFormat | 23 | PasswordDoesNotMeetRequirements |
| 10 | MissingPassword | 24 | UserIsPlaceholder |
| 11 | PasswordTooShort | 25 | Cancelled |
| 12 | WrongEmailOrPassword | 26 | RequiresCoreUpdate |
| 13 | MissingParameter | 27 | NotEnoughSpace |
| 14 | AlreadyInProgress | 28 | MissingData |

Observed in the code. Code 2 is the permission error. Code 21 on a tunnel closes the socket because the Core is offline. Code 26 means the Core is too old for the content. A Core may answer an unknown message with 22 or 15. Which code each operation returns for each failure is mostly untested.

## How changes are committed

- There is no commit, save, apply, transaction, edit mode or lock message (confirmed). A write is one request, answered by a response, followed by one broadcast that updates every client's cache.
- A write takes effect when the Core answers and is durable at once. A fresh client that connected from a new socket seconds later saw every write, with no restart and no second step. Persistence across a Core restart was not tested.
- A changed script or a new device ran immediately. A Packet Parser state script produced a value within seconds, so the Core applies configuration live.
- Changeset ids on permissions, dashboards and smart rules are version counters for cache synchronisation, not transactions. A smart rule write raises the rule's changeset and the global smart rule changeset.
- A restart is needed only for module replacement on Cores below protocol 13. `MessageRestartCoreRequest` (include tag 210) exists for the settings page.
- A multi-object message such as a list of smart rules or a bus configuration is one request. Whether the Core applies it atomically is not known. A config message with two objects (a Packet Parser module and its device) created both.

### Id allocation (confirmed)

- The Core picks ids for devices, modules and smart rules. The client sends 0, leaves the id out or uses a negative placeholder inside a config message.
- Negative placeholder ids in a Packet Parser `ConfigJson` are only labels. The Core replaced them and set the child's `HardwareParentId` to the new module id.
- A device create returns the new device with its id. A smart rule create returns no id, so a client matches the rule by name and changeset in the broadcast or the next sync.
- Ids are reused. A deleted device frees its id. A new smart rule took the id of a soft-deleted rule. A client must not treat "highest id plus 1" as the next id, and must not keep a deleted id.
- A deleted smart rule is not removed. It stays in the list as a tombstone with `IsDeleted` true and its parameters removed.

## Timeouts

| Item | Value |
|---|---|
| TCP connect and handshake | 20 s |
| Login (whole exchange) | 15 s |
| `/Identify`, `/SetUp`, `/ForgottenPassword` | 7 s for Identify, 15 s for the others |
| Reply to a request | 60 s by default. Long tasks use their own limit. The reconnect request waits 20 s. |
| Ping interval and pong wait | 6 s and 10 s |
| Acknowledgement interval and limit | 4 s and 10.5 s |
| Back-off after a failure | random exponential, 50 ms to 4 s locally and 8 s on the tunnel |
| Core not ready | wait 10 s, then reconnect |

These are the app's values (derived).

## Minimal client sequence

1. Read `ws://<core>/Identify` for `LocationId` (confirmed).
2. Log in on a first socket and close it (confirmed).
3. Open a second socket. Send a reconnect request with the session and `RequestsId` 1. Handle `ReconnectResult` 4 by reconnecting with the returned `DbHash` (confirmed).
4. Keep a cache from the reconnect response and the broadcasts.
5. Acknowledge every 4 s and ping every 6 s.
6. Send requests with `RequestsId` 2, 3 and so on. Match answers by `RequestsId`. Check `ErrorCode` and `ResponseFlags`.

## How the OpenAPI document maps this protocol

OpenAPI describes HTTP, so the document uses these conventions.

- Each request class is a virtual path, `POST /Core/messages/{RequestClass}`. For the other endpoints the prefix is `/SetUp/messages/`, `/ForgottenPassword/messages/` or `/Identify/messages/`. Nothing is posted over HTTP. `x-transport: websocket` and `x-ws-endpoint` mark this.
- The request body and the 200 response use the media type `application/x-protobuf` and refer to schemas named after the message classes.
- Several operations can share one request class, for example `moduleBlinkLed`, `moduleIdentification` and `setDeviceValue` all use `MessageDeviceAction`. They share one path. The first id is the `operationId` and the others are listed in the description under "Variants".
- Broadcasts are `webhooks`, one per message class. `x-broadcasts` on an operation lists the ones it triggers.
- `x-proto-include-tag` is the field number of the message in `MessageBase`. `x-proto-include-path`, `x-proto-included-by`, `x-proto-base`, `x-proto-abstract` and `x-proto-includes` describe the hierarchy.
- `x-proto-tag` is the field number of a property. `x-proto-wire`, `x-proto-packed`, `x-proto-format` (zigzag), `x-proto-default`, `x-proto-required` and `x-proto-map` describe its encoding. Guid, DateTime and TimeSpan carry `x-proto-encoding` with the bcl form.
- `x-kind` is read, write, subscribe or action. `x-min-protocol-version` is the lowest Core protocol version that supports the operation.
- `x-evidence` lists where in the app the operation was found, as `Assembly: Namespace.Class.Method`. `x-verified` is true only for operations tested against a real Core (see `docs/verification.md`).
- The security scheme `coreSession` is virtual (`x-virtual: true`). It stands for the session Guid that goes into the reconnect message. No such HTTP header exists and nothing is sent in the handshake.
- Real HTTP endpoints, such as the Core status page, the TapHomeApi routes and the cloud API, are ordinary paths with their own servers and security schemes.
