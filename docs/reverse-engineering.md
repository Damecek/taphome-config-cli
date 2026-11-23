# Reverse engineering guide

This page explains how to reproduce the analysis behind `openapi/taphome-core.yaml`. It covers where the app comes from, the tool pipeline, what stays out of the repository and how to test safely against a real Core. This is an independent interoperability project and it is not affiliated with TapHome.

## Where the app comes from

TapHome publishes its Android apps as downloads from its own public download folder. The folder is linked from the TapHome support page "TapHome Apps". Two builds were analysed.

| Line | Package | Version |
|---|---|---|
| Official | `com.taphome.android.offline` | 2025.2.32375 |
| Next | not recorded here, read it from the manifest of your download | 2026.1.41842 |

Both APKs carry the same signing certificate, SHA-256 `ff6508174eb4176fca34b358e31d6e7f4a8d1ae8459e76d38a5cbd5aba75d4ec`. Check this value after a download before you analyse a file. The spec describes the Official line. The Next build is used for comparison (`docs/architecture.md`).

## What the app is

The app is a .NET for Android (MAUI) application. Its code is not Java. The managed assemblies are in an assembly store inside a native library, `lib/arm64-v8a/libassemblies.arm64-v8a.blob.so`. The store holds the TapHome assemblies and the libraries they use. The important TapHome assemblies are these.

| Assembly | Content |
|---|---|
| `TapHome.Android` | UI, communication manager, login helper, discovery |
| `TapHome.Core.Lib.Model` | Message classes and the serializer model |
| `TapHome.Core.Lib.Common` | Shared code, including the password hash and the formula parser |
| `TapHome.Core.Lib.Client.CloudModel` | Cloud API data classes |

A separate `websocket-sharp` fork provides the WebSocket client.

## Pipeline

Install the tools on macOS.

```sh
brew install openjdk jadx apktool android-platform-tools dotnet uv
dotnet tool install -g ilspycmd
```

All outputs go to `work/`, which Git ignores. The commands run from the repository root.

1. Put the APK in `work/apk/` and unpack it, for example with `apktool d` or `unzip`. You need `lib/arm64-v8a/libassemblies.arm64-v8a.blob.so` and `AndroidManifest.xml`.
2. Extract the assemblies from the store.

   ```sh
   uv run --with lz4 scripts/extract_assembly_store.py \
     work/official/lib/arm64-v8a/libassemblies.arm64-v8a.blob.so work/official/dll
   ```

   `scripts/extract_assembly_store.py` reads the XABA store (64-bit, format 2 or 3), decompresses entries that use LZ4 and writes plain DLL files.
3. Decompile each TapHome assembly to C# with ILSpy. The tool needs a runtime that matches its build, so allow a newer major version.

   ```sh
   export DOTNET_ROOT=/opt/homebrew/opt/dotnet/libexec DOTNET_ROLL_FORWARD=Major
   ilspycmd -p -o work/official/src/TapHome.Core.Lib.Model -r work/official/dll \
     work/official/dll/TapHome.Core.Lib.Model.dll
   ```

   `scripts/build_catalog.py` expects one directory per assembly under the source directory, with namespaces as sub-directories and one file per class. Project mode (`-p`) gives that layout. Repeat the command for every `TapHome.*.dll` except the translation and embedded resource assemblies.
4. Build the protobuf type catalog.

   ```sh
   python3 scripts/build_catalog.py work/official/src work/catalog/official.json
   python3 scripts/build_catalog.py work/next/src work/catalog/next.json
   python3 scripts/build_catalog.py --diff work/catalog/official.json work/catalog/next.json work/catalog/diff-official-vs-next.md
   ```

   The variant name defaults to the name of the parent directory (`official` or `next`). The script reads the `ProtoContract`, `ProtoMember` and `ProtoInclude` attributes and cross-checks them against the generated serializer. It uses only the standard library and never runs the decompiled code. `python3 scripts/build_catalog.py --self-test` runs its own checks.
5. Build the OpenAPI document.

   ```sh
   uv run --with pyyaml scripts/build_openapi.py          # write openapi/taphome-core.yaml
   uv run --with pyyaml scripts/build_openapi.py --check  # validate and compare with the file on disk
   ```

   Inputs are `openapi/operations/*.yaml`, `openapi/static.yaml`, `openapi/verification.yaml` and `work/catalog/official.json`. The output is deterministic.
6. Test against a Core with the probe in `scripts/probe` (next sections).

## REA setup

The REA MCP server and its skill help with the decompiled code. REA is a general reverse-engineering toolkit and none of the pipeline steps depends on it.

The project uses [REA](https://github.com/morluto/rea) (`rea-agents@6.9.0`) as a project-scoped MCP server, configured in `.mcp.json`, with its skill in `.agents/skills/reverse-engineer-anything` (linked from `.claude/skills`). On top of the pipeline tools above, it needs the headless jadx MCP server.

```sh
mkdir -p ~/.local/share/rea/android && cd ~/.local/share/rea/android
curl -fL -o jadx-headless-mcp-0.7.1-all.jar \
  https://github.com/1013503897/jadx-headless-mcp/releases/download/v0.7.1/jadx-headless-mcp-0.7.1-all.jar
echo '6e5eacf500b64292bfb73c49797c1958f6ee44646e43e868039ae7feb573ff75  jadx-headless-mcp-0.7.1-all.jar' | shasum -a 256 -c -
```

Override the tool paths with `REA_JADX_MCP_JAR`, `REA_APKTOOL_COMMAND` and `REA_ADB_PATH` if yours differ.

## What stays in work/

`work/` is git-ignored. It holds the APKs, the extracted DLLs, the decompiled sources, the type catalog, raw captured frames, working notes and local secrets. Do not commit any of it.

- The APKs, assemblies and decompiled sources are TapHome's code. The repository publishes only the interface description and original code.
- The catalog is generated from that code. Only its derived result, the OpenAPI document, is committed.
- Captures and notes can contain credentials, tokens or the owner's addresses.
- `work/secrets/` holds the password salt, the device id and the stored session.

## Environment variables

The probe reads credentials only from the environment, never from files in the repository.

| Variable | Meaning |
|---|---|
| `TAPHOME_HOST` | Address of the Core. Required. |
| `TAPHOME_USER` | Local user name. |
| `TAPHOME_PASSWORD` | Password of that user. |
| `TAPHOME_USER_FILE`, `TAPHOME_PASSWORD_FILE` | Path to a file with the value. Used when the plain variable is not set. |
| `TAPHOME_PASSWORD_SALT` | The password salt (see `docs/auth.md`). If it is not set, the probe reads `work/secrets/salt.txt`. |
| `TAPHOME_CATALOG` | Catalog file. Default `work/catalog/official.json`. |

Never commit the Core address, user names, passwords, tokens or the salt. Ask the owner through a secret prompt instead of typing them into a command line.

## Running the probe

Run it from the `scripts/` directory. It needs only the `websockets` package.

```sh
cd scripts
uv run --python /opt/homebrew/bin/python3.14 --with websockets python -m probe identify
uv run --python /opt/homebrew/bin/python3.14 --with websockets python -m probe version-probe
uv run --python /opt/homebrew/bin/python3.14 --with websockets python -m probe login
uv run --python /opt/homebrew/bin/python3.14 --with websockets python -m probe sync
uv run --python /opt/homebrew/bin/python3.14 --with websockets python -m probe call MessageGetBasicCorePropertiesRequest
uv run --python /opt/homebrew/bin/python3.14 --with websockets python -m probe listen 30
```

| Command | What it does |
|---|---|
| `identify` | Reads the `LocationInfo` from `/Identify`. |
| `version-probe` | Reconnects with a random session id and prints the result and any version fields. |
| `login` | Logs in and stores the session in `work/secrets/session.json`, reusing it when it is still valid. |
| `sync` | Reconnects, writes the full response to `work/captures/`, and prints a summary. |
| `call <Message>` | Sends one request. Options are `--json`, `--wait-broadcasts` and `--allow-write`. |
| `listen <seconds>` | Counts broadcasts by class name. |

The probe redacts the password, hash, salt, session id and tokens in everything it prints. The codec has unit tests. Run `python3 -m unittest` in `scripts/probe`. The tests need the catalog.

### macOS note

On macOS, a Python managed by uv cannot reach the local network. The system's Local Network privacy rule blocks it with `OSError 65 No route to host`. Use the system or Homebrew interpreter with `uv run --python /opt/homebrew/bin/python3.14`, as in the commands above (confirmed on the test machine).

## Read-only allowlist and approval for writes

`probe call` refuses any message that is not on the allowlist unless you pass `--allow-write`. The allowlist holds every request class whose name starts with `MessageGet`, and every request class that the working notes mark as `read` or `subscribe` and never as a write.

Rules for testing against a real Core.

1. Read-only calls are fine.
2. Before any write, describe the change to the owner and get approval. Do not pass `--allow-write` without it.
3. Create test objects with the prefix `zz-agent-test` and remove them afterwards.
4. Treat factory reset, restore, clone, user and permission changes and firmware upload as destructive.
5. After a test, add the result to `openapi/verification.yaml` (see `docs/verification.md`) and mark the operation as verified only if the observed behaviour matched.

## Evidence in the spec

Every operation lists where the app implements it (`x-evidence`, as `Assembly: Namespace.Class.Method`) and whether it was tested (`x-verified`). A reader can follow the evidence with the decompiled sources they produce in step 3.
