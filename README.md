# taphome-config-cli

Lets AI agents configure a TapHome smart home without a person copying templates into the TapHome app by hand.

The goal is configuration and administration (devices, virtual devices, Packet Parser devices, automations, inputs and outputs), not day-to-day control of the home.

## Step 1: OpenAPI specification from the Android app

Reverse engineer the TapHome Android app and write an OpenAPI 3.1 specification of the API it uses to read and change configuration, most likely on the TapHome Core over the LAN, possibly through the TapHome cloud.

The specification must cover:

- authentication: every supported method, the login flow, token format and lifetime
- transport: LAN versus cloud, base URLs, discovery, protocol (REST, WebSocket, binary)
- create, read, update and delete for devices, virtual devices, Packet Parser devices and automations
- request and response schemas, error formats, and how changes are saved or committed to the Core

Every endpoint in the spec links to the evidence for it (decompiled class or captured traffic). Each endpoint is verified against a real Core before it is marked as confirmed.

Output: `openapi/taphome-core.yaml` and notes in `docs/`.

## Step 2: CLI for agents

Build a command line tool on top of the specification so that an agent can:

- list and inspect the current configuration (devices, modules, inputs, outputs)
- add and change devices, virtual devices, Packet Parser devices and automations
- check inputs and outputs after a change

The CLI returns JSON, supports a dry run, and never stores credentials in the repository.

## Reverse engineering setup (REA)

The project uses [REA](https://github.com/morluto/rea) (`rea-agents@6.9.0`) as a project-scoped MCP server, configured in `.mcp.json`, with its skill in `.agents/skills/reverse-engineer-anything` (linked from `.claude/skills`).

Prerequisites on macOS:

```sh
brew install openjdk jadx apktool android-platform-tools
mkdir -p ~/.local/share/rea/android && cd ~/.local/share/rea/android
curl -fL -o jadx-headless-mcp-0.7.1-all.jar \
  https://github.com/1013503897/jadx-headless-mcp/releases/download/v0.7.1/jadx-headless-mcp-0.7.1-all.jar
echo '6e5eacf500b64292bfb73c49797c1958f6ee44646e43e868039ae7feb573ff75  jadx-headless-mcp-0.7.1-all.jar' | shasum -a 256 -c -
```

Override the tool paths with `REA_JADX_MCP_JAR`, `REA_APKTOOL_COMMAND` and `REA_ADB_PATH` if yours differ.

## Legal

This is an independent interoperability project, not affiliated with TapHome. APKs and decompiled sources stay local in `work/` (git-ignored) and are never committed. Only the API description and original code are published.
