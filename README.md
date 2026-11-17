# taphome-config-cli

Lets AI agents configure a TapHome smart home without a person copying templates into the TapHome app by hand.

The goal is configuration and administration (devices, virtual devices, Packet Parser devices, automations, inputs and outputs), not day-to-day control of the home.

## Status

Step 1 is drafted and step 2 exists. Step 2 is `taphome`, a Rust command line tool in `src/` that reads and changes the configuration of a Core. Install and usage are in [Step 2](#step-2-cli-for-agents), the full reference is [`docs/cli.md`](docs/cli.md), and agents follow [`.agents/skills/taphome-cli/SKILL.md`](.agents/skills/taphome-cli/SKILL.md).

The result of the reverse engineering is that TapHome has no REST API for configuration. The Android app talks to the TapHome Core over a WebSocket on the LAN (`ws://<core>/Core`, port 80) and exchanges binary protobuf messages. The Core stores the whole configuration. The cloud only holds accounts and a location list, and relays the same WebSocket when the phone is away from home.

Login, the initial configuration sync, 48 read operations and the core write paths were confirmed against a real Core (software 2025.2.32396, protocol version 89). The confirmed writes are: create, rename and delete a virtual device, create a Packet Parser module with a device from a configuration JSON and change its script, create and delete a smart rule. The remaining operations are derived from the app code. [`docs/verification.md`](docs/verification.md) lists what is confirmed and what is not.

- [`openapi/taphome-core.yaml`](openapi/taphome-core.yaml): the OpenAPI 3.1 document. WebSocket messages appear as virtual `POST /Core/messages/{Class}` paths, broadcasts as `webhooks`.
- [`docs/architecture.md`](docs/architecture.md): components, LAN and cloud paths, discovery, endpoints, Official and Next lines.
- [`docs/auth.md`](docs/auth.md): authentication methods, login and session flow, password hash.
- [`docs/protocol.md`](docs/protocol.md): wire format, acknowledgements, errors, how changes are committed, how the spec maps the protocol.
- [`docs/configuration-model.md`](docs/configuration-model.md): interfaces, devices, I/O, Packet Parser, Modbus, smart rules, with message sequences.
- [`docs/reverse-engineering.md`](docs/reverse-engineering.md): how to reproduce the analysis.
- [`docs/verification.md`](docs/verification.md): what is confirmed against a real Core.

## Step 1: OpenAPI specification from the Android app

Reverse engineer the TapHome Android app and write an OpenAPI 3.1 specification of the API it uses to read and change configuration, most likely on the TapHome Core over the LAN, possibly through the TapHome cloud.

The specification must cover:

- authentication: every supported method, the login flow, token format and lifetime
- transport: LAN versus cloud, base URLs, discovery, protocol (REST, WebSocket, binary)
- create, read, update and delete for devices, virtual devices, Packet Parser devices and automations
- request and response schemas, error formats, and how changes are saved or committed to the Core

Every endpoint in the spec links to the evidence for it (decompiled class or captured traffic). Each endpoint is verified against a real Core before it is marked as confirmed.

Output: `openapi/taphome-core.yaml` and notes in `docs/`. The app uses no HTTP API for configuration, so the specification describes the WebSocket messages with OpenAPI conventions (see `docs/protocol.md`). Only the Core status page, the TapHomeApi routes and the cloud API are real HTTP endpoints.

## Step 2: CLI for agents

`taphome` lets an agent list and inspect the configuration (devices, modules, inputs, outputs), add and change devices, virtual devices, Packet Parser devices and smart rules, and check inputs and outputs after a change. Every command prints one JSON document and supports `--dry-run` for writes. Credentials never go into the repository. Device control (switching a light, setting a temperature) is out of scope.

### Install

Release binaries contain the TapHome password salt. The public source does not. A binary built from source cannot log in unless you give it the salt (see below). Where to find the salt is described in [`docs/auth.md`](docs/auth.md).

On macOS and Linux, the install script downloads the binary for your platform from the latest release, checks its SHA-256 against the release's `SHA256SUMS` and installs it to `~/.local/bin`.

```sh
curl -fsSL https://raw.githubusercontent.com/Damecek/taphome-config-cli/main/install.sh | sh
```

`TAPHOME_VERSION=v0.1.0` pins a release and `TAPHOME_INSTALL_DIR=/some/dir` changes the target directory. On Windows, download `taphome-x86_64-pc-windows-msvc.zip` from the [Releases page](https://github.com/Damecek/taphome-config-cli/releases). Releases carry five targets: macOS arm64 and x86_64, Linux musl arm64 and x86_64, and Windows x86_64.

To build from source you need a Rust toolchain and the salt.

```sh
TAPHOME_PASSWORD_SALT='...' cargo install --git https://github.com/Damecek/taphome-config-cli
```

Instead of the variable, a build reads the file `~/.config/taphome/salt`. A build with neither still works for every command except login, which fails with the error code `salt_missing`. Setting `TAPHOME_PASSWORD_SALT` at run time also works with such a build.

### Configure and log in

Set three environment variables, for example in the agent's environment or through its secret prompt.

| Variable | Meaning |
|---|---|
| `TAPHOME_HOST` | Address of the Core, `<core-address>` or `<core-address>:<port>`. Port 80 is the default. |
| `TAPHOME_USER` | The local user name. |
| `TAPHOME_PASSWORD` | The password. |

Each one also has a `_FILE` variant (`TAPHOME_PASSWORD_FILE=/path/to/file`) that names a file holding the value. The password is never accepted as a command line argument. The salt is the only other secret and also comes from the environment or the build, never from an argument.

```sh
taphome auth login
taphome auth status
```

`auth login` stores the session in `~/.config/taphome/session.json` (on Windows `%APPDATA%\taphome\session.json`, `TAPHOME_CONFIG_DIR` overrides the directory). The directory is created with mode 700 and `session.json` and `device-id` are written with mode 600. Later commands reuse the session and log in again when the Core rejects it and the password is in the environment. Without the password, they fail with `session_invalid`. `taphome auth logout` deletes the file.

### A short tour

```sh
taphome sync                                   # counts of devices, interfaces and rules
taphome device list --model 'Virtual*'         # compact rows
taphome pp inspect lamp.xml --param ApiKey=...  # offline: what an import would send
taphome pp import lamp.xml --param ApiKey=... --dry-run
taphome pp import lamp.xml --param ApiKey=...   # sends it, then verifies with a fresh sync
taphome rule get "My rule" | jq .data.model > rule.json   # edit rule.json
taphome rule apply rule.json --dry-run
taphome io values <device-id>                   # current values after the change
taphome io errors <device-id>
```

`rule get` prints the envelope, and the object under `data.model` is a valid file for `rule apply`. More commands, flags and output fields are in [`docs/cli.md`](docs/cli.md).

### Safety model

- Output is one JSON envelope with `ok`, `command`, `data` and `warnings`, or `error` on failure. Exit codes tell the failure class: 0 ok, 2 usage, 3 auth or config, 4 connection, 5 the Core rejected the request, 6 object not found or ambiguous, 7 refused by a CLI gate, 8 verification failed.
- Every write command has `--dry-run`. It reads the current configuration, builds the message, prints it with a diff and sends nothing.
- A real write is verified. After the Core answers, the CLI opens a new session, syncs again and checks the expected state. A failed check exits with 8 and still reports what the Core answered.
- Operations and variants that were never tested against a real Core are refused with `unverified_operation` unless you pass `--allow-unverified`.
- `taphome raw call` sends any request class. It refuses write and action classes without `--allow-write` and unverified classes without `--allow-unverified`.
- Passwords, tokens, the session id, the password hash and the salt are redacted from all output. `--show-secrets` reveals property passwords and tokens only.

### macOS and Local Network privacy

macOS blocks LAN access for a process started by an app that has no Local Network permission. The connection then fails with "No route to host", which the CLI reports as `connection_failed`. Grant the permission to the terminal or agent app under System Settings, Privacy and Security, Local Network, then start a new session of that app.

### Development

```sh
cargo fmt
cargo clippy --all-targets -- -D warnings
cargo test
```

Tests use a fake Core on `127.0.0.1` and an invented salt, so they need no network and no secret.

- `data/catalog.json` is the protocol catalog compiled into the binary. Regenerate it from the spec with `uv run --with pyyaml scripts/build_cli_catalog.py`, and check that it is current with `--check`.
- `tests/vectors/codec.json` holds byte vectors for the codec tests. Regenerate it with `python3 scripts/build_codec_vectors.py`.
- CI (`.github/workflows/ci.yml`) runs fmt, clippy and the tests on pull requests and on `main`. It builds without a salt.
- The release workflow (`.github/workflows/release.yml`) runs on a tag `v*`. It builds the five targets, packs them as `taphome-<target>.tar.gz` (a `.zip` on Windows), writes `SHA256SUMS` and publishes a GitHub release. The salt comes from the repository secret `TAPHOME_PASSWORD_SALT`, which reaches only the build step, and `TAPHOME_REQUIRE_SALT=1` makes the build fail when the secret is missing.

## Reverse engineering setup (REA)

The project uses [REA](https://github.com/morluto/rea) (`rea-agents@6.9.0`) as a project-scoped MCP server, configured in `.mcp.json`, with its skill in `.agents/skills/reverse-engineer-anything` (linked from `.claude/skills`).

Prerequisites on macOS:

```sh
brew install openjdk jadx apktool android-platform-tools dotnet uv
dotnet tool install -g ilspycmd
mkdir -p ~/.local/share/rea/android && cd ~/.local/share/rea/android
curl -fL -o jadx-headless-mcp-0.7.1-all.jar \
  https://github.com/1013503897/jadx-headless-mcp/releases/download/v0.7.1/jadx-headless-mcp-0.7.1-all.jar
echo '6e5eacf500b64292bfb73c49797c1958f6ee44646e43e868039ae7feb573ff75  jadx-headless-mcp-0.7.1-all.jar' | shasum -a 256 -c -
```

The app is a .NET application, so `ilspycmd` decompiles its assemblies and `uv` runs the scripts in `scripts/`. The full pipeline is in [`docs/reverse-engineering.md`](docs/reverse-engineering.md).

Override the tool paths with `REA_JADX_MCP_JAR`, `REA_APKTOOL_COMMAND` and `REA_ADB_PATH` if yours differ.

## Legal

This is an independent interoperability project, not affiliated with TapHome. APKs and decompiled sources stay local in `work/` (git-ignored) and are never committed. Only the API description and original code are published.
