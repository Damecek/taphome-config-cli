# Project brief

The goal is to let AI agents configure a TapHome smart home without a person copying templates into the TapHome app by hand. The scope is configuration and administration (devices, virtual devices, Packet Parser devices, automations, inputs and outputs), not day-to-day control of the home.

The work has two steps. Both are done. What was tested against a real Core is in [`verification.md`](verification.md).

## Step 1: OpenAPI specification from the Android app

Reverse engineer the TapHome Android app and write an OpenAPI 3.1 specification of the API it uses to read and change configuration, most likely on the TapHome Core over the LAN, possibly through the TapHome cloud.

The specification must cover:

- authentication: every supported method, the login flow, token format and lifetime
- transport: LAN versus cloud, base URLs, discovery, protocol (REST, WebSocket, binary)
- create, read, update and delete for devices, virtual devices, Packet Parser devices and automations
- request and response schemas, error formats, and how changes are saved or committed to the Core

Every endpoint in the spec links to the evidence for it (decompiled class or captured traffic). Each endpoint is verified against a real Core before it is marked as confirmed.

Output: [`openapi/taphome-core.yaml`](../openapi/taphome-core.yaml) and notes in `docs/`. The app uses no HTTP API for configuration, so the specification describes the WebSocket messages with OpenAPI conventions: messages appear as virtual `POST /Core/messages/{Class}` paths and broadcasts as `webhooks` (see [`protocol.md`](protocol.md)). Only the Core status page, the TapHomeApi routes and the cloud API are real HTTP endpoints.

## Step 2: CLI for agents

A CLI that lets an agent list and inspect the configuration (devices, modules, inputs, outputs), add and change devices, virtual devices, Packet Parser devices and smart rules, and check inputs and outputs after a change. Every command prints one JSON document and supports `--dry-run` for writes. Credentials never go into the repository. Device control (switching a light, setting a temperature) is out of scope.

Output: the `taphome` CLI in `src/` (Rust), its reference in [`cli.md`](cli.md) and the agent skill in [`.agents/skills/taphome-cli/SKILL.md`](../.agents/skills/taphome-cli/SKILL.md).
