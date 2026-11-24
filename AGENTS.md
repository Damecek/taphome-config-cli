# Agent instructions

Read `README.md` first. The project goals, step 1 (OpenAPI spec from the TapHome Android app) and step 2 (CLI for agents), are in `docs/project-brief.md`.

- Reverse engineering: use the REA MCP server (`.mcp.json`) and the `reverse-engineer-anything` skill.
- Keep APKs, decompiled sources and traffic captures in `work/` (git-ignored). Never commit TapHome code, binaries or captures that contain credentials or tokens.
- Never commit the Core address, user names, passwords or tokens. Read them from environment variables (`TAPHOME_HOST`, `TAPHOME_USER`, `TAPHOME_PASSWORD`) or ask the owner through the harness secret prompt.
- Against the real Core: read-only calls are fine. Before any write, describe the change and get the owner's approval. Prefer creating test objects with a `TEST-zz-agent-test` prefix and remove them afterwards.
- Mark each endpoint in the spec as `x-evidence` (where it was found) and `x-verified: true|false` (tested against a real Core).
- The CLI lives in `src/` (Rust). Run `cargo fmt`, `cargo clippy --all-targets -- -D warnings` and `cargo test` before a commit.
- Agents that operate a real Core follow `.agents/skills/taphome-cli/SKILL.md`.
