# Authentication

This page lists every authentication method the TapHome app uses, with the exact flow and the message fields. Status values. "Confirmed" means seen on a live Core (details in `docs/verification.md`). Everything else is derived from the app code and untested.

## Methods at a glance

| Method | Used for | Credential | Carried in | Status |
|---|---|---|---|---|
| Local account | Core configuration over LAN or tunnel | user name and password of a Core user | `MessageLoginRequest` | confirmed |
| Cloud account on a Core | Core configuration for a cloud user | cloud e-mail and the per-location authorization token | `MessageLoginRequest` | derived |
| Cloud HTTP API bearer | Cloud API (locations, tunnel endpoints, history, updates) | token from `login2` | `Authorization: Bearer` header | derived |
| TapHomeApi token | Control-only HTTP API of the Core or cloud | Guid of the TapHomeApi interface | query parameter `token` | derived |
| Access token | First setup and forgotten admin password | token printed on or shown by the Core | `MessageSetupNewLocationRequest`, `MessageForgottenPasswordRequest` | derived |

`/Identify` and discovery need no credential. The WebSocket handshake never carries credentials. The app does not set an `Authorization` header on it, and it sets no sub-protocol.

## Flow for a local account (confirmed)

This is the flow a configuration client needs.

1. Learn the Core's `LocationId`. Open `ws://<core>/Identify` and read the one frame, or use discovery.
2. Open a first socket to `ws://<core>/Core`.
3. Send one `MessageBase` with a `MessageLoginRequest`. `RequestsId` is 0, so field 1 is omitted.
4. Read frames until a `MessageLoginResponse` arrives. The Core may send other messages first. The app tolerates up to 10 other messages and then gives up.
5. Check `LoginResult`. On 1 (Ok), compare `LocationInfo.LocationId` with the requested id. The app treats a mismatch as a wrong login.
6. Keep `SessionId` and close the login socket. The Core also closes it.
7. Open a second socket to `ws://<core>/Core`. Send a `MessageReconnectRequest` with the `SessionId` and `RequestsId` 1.
8. The Core answers with a `MessageReconnectResponse`. On `ReconnectResult` 1 the socket is the working socket and the response holds the full configuration.

The session is never put into a URL or a header. Every new `/Core` socket, local or tunneled, starts with a reconnect request.

A fresh client sends all changeset ids as 0 and an empty `DbHash`. Confirmed: the Core then answers `ReconnectResult` 4 (FaultyStorageClient) with its current `DbHash`. The client reconnects with that `DbHash` and gets `ReconnectResult` 1 and everything in one message.

## Flow for a cloud account (derived)

1. `POST api/user/login2` with e-mail and password returns a cloud token and `myInfo.myLocations[]`. Each location has a `locationId`, a `name`, an `ipOnLocalNetwork`, a `partialAccessToken` and an `authorizationToken` (a Guid).
2. For the Core, send a `MessageLoginRequest` with `LoginOrEmail` set to the e-mail, `IsLocalAccount` false, and the secret set to the authorization token written as a lowercase Guid with hyphens.
3. The secret goes through the same password hash as a local password (next section).
4. The rest is identical to the local flow. The Core answers `WrongAuthToken` (4) when the token is stale, and the app then refreshes the cloud user information, at most once every 60 s.

Cloud accounts reach the Core over the LAN or over the tunnel with the same message. User management for cloud users is a Core operation. The Core keeps the list of users that may access it, and the cloud only proves who the person is.

## Cloud HTTP API bearer (derived)

- Base URL is `https://cloudapi.taphome.com/`. The environment variable `X_TAPHOME_CLOUDAPI_URL` overrides it in the app.
- `POST api/user/login2` and `register2` need no header. All other user calls send `Authorization: Bearer <token>`. The app skips the header for any URL that contains `/login` or `/register`.
- The history endpoint sends the same bearer token and returns a signed URL for Azure Table Storage.
- Lifetime is unknown. The response has no expiry field and the client has no refresh call. A 401 makes the app log the user out.
- The client defines other scheme names (`Tunnel`, `Core`, `TapHome`, `CloudLogs`) but never uses them. They appear to belong to cloud-side services.

## TapHomeApi token (derived)

- The token is a Guid. It lives in the `Parameter1` field of the TapHomeApi interface. The app shows it and can read it later from the interface.
- A caller sends it as the query parameter `token`, for example `...getDeviceValue/{externalId}?token=<guid>&valueTypeId=<n>`. The name of the client's `TapHome` scheme suggests an `Authorization` header form, but the app never uses it.
- `MessageRefreshTapHomeApiTokenRequest` (operation `refreshTapHomeApiToken`) generates a new token. The old token stops working.
- The token grants control and read access only to devices the owner exposed.

## Access token (derived)

The Core has a setup token, also called access token. The Core announces only a masked copy. Confirmed: on `/Identify` the field is 16 characters, 12 asterisks followed by the last 4 characters of the token. The cloud location list holds the same masked value as `partialAccessToken`.

- `/SetUp` takes the full token in `MessageSetupNewLocationRequest.AccessToken` for a Core with no configuration.
- `/ForgottenPassword` takes it in `MessageForgottenPasswordRequest` and `MessageSetNewPasswordRequest`, together with the user id and the new password hash.
- The last 4 characters are also used as `shorttoken` when the app fetches update rules from the cloud.

## Login message

`MessageLoginRequest` has include tag 108 in `MessageBase`.

| Tag | Field | Type | Value |
|---|---|---|---|
| 1 | `LocationId` | Guid | Location Guid of the Core |
| 2 | `LoginOrEmail` | string | User name, lowercase, for a local account. E-mail for a cloud account. |
| 3 | `PasswordHashOrAuthorizationToken` | string | Password hash of the secret. Omitted when the secret is empty. |
| 4 | `Platform` | string | `Android` |
| 5 | `SoftwareVersion` | string | App version, for example `2025.2.32375` |
| 6 | `DeviceId` | string | Client device id |
| 7 | `ClientDeviceName` | string | Client device name |
| 8 | `ServerClientDeviceId` | int64 | 0 at login |
| 9 | `IsLocalAccount` | bool | true for a local account |
| 10 | `ClientProtocolVersion` | int32 | 89 for the Official line |
| 11 | `LanguageCode` | string | Next line only |

`MessageLoginResponse` has include tag 109.

| Tag | Field | Type |
|---|---|---|
| 1 | `LoginResultId` | int32 |
| 2 | `SessionId` | Guid |
| 3 | `UserInfo` | `UserWithoutHash` |
| 4 | `LocationInfo` | `LocationInfo` |
| 5 | `DeviceInfo` | `ClientDeviceShort` |
| 6 | `Ccus` | list of `GenericCcu` |
| 7 | `Devices` | list of `GenericDevice` |
| 8 | `DeviceTimers` | map from device id to list of timer info |

The reconnect request (include tag 110) repeats `LocationId` (1), `Login` (2), the `SessionId` (3), `IsLocalAccount` (4), `Platform` (5), `SoftwareVersion` (6), `DeviceId` (7), `ClientDeviceName` (8), `ServerClientDeviceId` (9) and `ClientProtocolVersion` (11). Fields 12 to 24 are the cached changeset ids for permissions, dashboards and smart rules, and field 24 is the `DbHash`. The full field list is in the OpenAPI schema `MessageReconnectRequest`.

## Password hash

The Core stores and compares a hash, never the plain password, and the client sends the hash.

1. Take the secret as text (the password, or the authorization token for a cloud account).
2. Append a fixed salt string to it and encode the result as UTF-8.
3. Compute SHA-512 of those bytes.
4. Compute SHA-512 of the 64-byte digest from step 3 (a binary digest, not its hex text).
5. Write the result as 128 lowercase hex characters.

The app calls the function with an iteration count of 2, which is why SHA-512 runs twice. The same function hashes the new password in `MessageSetNewPasswordRequest`.

The salt is one constant compiled into the app. It is the same for every user and every Core. This repository does not contain it. A client must take it from the app binary, class `TapHome.Core.Lib.Password` in the assembly `TapHome.Core.Lib.Common`, after the app has been extracted and decompiled (see `docs/reverse-engineering.md`). The tooling in `scripts/probe` reads it from the environment variable `TAPHOME_PASSWORD_SALT`, or from the git-ignored file `work/secrets/salt.txt`.

A client in Python computes the value like this.

```python
import hashlib, os
salt = os.environ["TAPHOME_PASSWORD_SALT"]
data = (password + salt).encode("utf-8")
for _ in range(2):
    data = hashlib.sha512(data).digest()
password_hash = data.hex()
```

Status. Confirmed, because logins on the test Core succeed with this algorithm.

## Session

| Property | Value | Status |
|---|---|---|
| Format | A Guid issued by the Core in `MessageLoginResponse.SessionId`. | confirmed |
| Presentation | `SessionId` field of the first `MessageReconnectRequest` on every `/Core` socket. Not in the URL, headers or cookies. | derived |
| Persistence | The app stores it per location and reuses it across restarts and reconnects. | derived |
| Lifetime | No expiry field and no refresh message. The session ends when the Core answers `WrongSession`. The behaviour across a Core restart, a day or a password change is not known. | derived |
| Logout | Client side only. The app closes its sockets and deletes the session. There is no logout message. | derived |
| Limit | `TooManyConnections` exists. The limit is not known. | derived |

A reconnect with an unknown `SessionId` returns `ReconnectResult` 2 (WrongSession) and no version fields. This is confirmed. A client can use this answer to test whether a stored session is still valid, but it cannot read the Core version this way.

## Result tables

`LoginResult` (derived, field `LoginResultId`).

| Value | Name | Meaning |
|---|---|---|
| 0 | None | Not set |
| 1 | Ok | Login accepted (confirmed) |
| 2 | WrongPassword | Password hash does not match |
| 3 | WrongLogin | Unknown user, or location mismatch |
| 4 | WrongAuthToken | Stale cloud authorization token |
| 5 | Deleted | The user was deleted |
| 6 | AppTooOld | The client protocol version is too low |

`ReconnectResult` (field `ReconnectResultId`).

| Value | Name | Client reaction in the app | Status |
|---|---|---|---|
| 0 | None | Not set | derived |
| 1 | Ok | Take the configuration and start acknowledgements and pings. | confirmed |
| 2 | WrongSession | Drop the session and log in again. | confirmed |
| 3 | TooManyConnections | Treated as unknown, log in again. | derived |
| 4 | FaultyStorageClient | Wipe the cache, store the new `DbHash` and reconnect. | confirmed |
| 5 | RemoteAccessDisabled | Stop. The user has no remote access. | derived |
| 6 | AppTooOld | Stop. Update the client. | derived |
| 7 | NotReady | The Core is still starting. Wait 10 s and reconnect. | derived |
| 8 | WrongLocationId | Treated as unknown, log in again. | derived |

An `Ok` answer with zero devices and interfaces while the cache has some counts as `NotReady`.

## Security observations

These are statements about the protocol, not about the owner's setup.

- LAN traffic is plain `ws://` on port 80 with no TLS. A device on the same network can read every message, including login frames and all configuration (derived from the app code and consistent with the live tests).
- The password hash is sent as the credential. Whoever captures it can log in without knowing the password, so a client must treat the hash like a password. It must not appear in logs, shell history or the repository. `scripts/probe` redacts it in all output.
- The salt is the same on every installation. Equal passwords give equal hashes on every Core, and the salt is public to anyone who has the app (derived).
- The cloud authorization token for a location is a Guid that acts as the credential on the LAN path and the cloud path. A stale token is replaced on refresh (derived).
- `/Identify` needs no login and exposes the location id, the name, the address and the last 4 characters of the access token. Confirmed. It does not expose the full token or the authorization token.
- The session is a bare Guid. Anyone who has it can open a working socket until the Core invalidates it (derived).
