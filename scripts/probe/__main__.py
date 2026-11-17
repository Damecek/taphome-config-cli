"""Probe CLI for a TapHome Core.

Run from the scripts/ directory (needs only the `websockets` package):

    cd scripts && uv run --python /opt/homebrew/bin/python3.14 --with websockets python -m probe <command>

On macOS, uv-managed Pythons are blocked from the LAN ("No route to host", Local Network privacy);
the Homebrew interpreter works. Unit tests: cd scripts/probe && python3 -m unittest

Environment: TAPHOME_HOST, TAPHOME_USER, TAPHOME_PASSWORD (or TAPHOME_USER_FILE /
TAPHOME_PASSWORD_FILE), optional TAPHOME_CATALOG (default work/catalog/official.json),
TAPHOME_PASSWORD_SALT (default: contents of work/secrets/salt.txt).

Commands:
    identify                  LocationInfo from /Identify (pre-auth)
    version-probe             reconnect with a random session id; prints protocol and Core version
    login                     log in, store the session in work/secrets/session.json (reuses it if valid)
    sync                      reconnect, write the full response to work/captures/sync-<ts>.json, print a summary
    call <Message> [--json '{...}'] [--wait-broadcasts S] [--allow-write]
                              send one request (read-only allowlist unless --allow-write)
    listen SECONDS            count broadcasts by class name

Secrets (password, hash, salt, session id, tokens) are redacted in all printed output.
"""
from __future__ import annotations

import argparse
import asyncio
import collections
import contextlib
import hashlib
import json
import os
import re
import sys
import time
import uuid
from pathlib import Path

from . import client, codec

ROOT = client.ROOT
SECRETS = ROOT / "work" / "secrets"
CAPTURES = ROOT / "work" / "captures"
SECRET_KEYS = {"PasswordHashOrAuthorizationToken", "SessionId", "AccessToken", "AuthorizationToken",
               "NewPasswordHash", "PasswordHash"}
_secret_values: set = set()


# ---------------------------------------------------------------- config and output

def _env_or_file(name: str):
    if os.environ.get(name):
        return os.environ[name]
    f = os.environ.get(name + "_FILE")
    return Path(f).read_text().strip() if f else None


def config() -> dict:
    host = os.environ.get("TAPHOME_HOST")
    if not host:
        sys.exit("TAPHOME_HOST is not set")
    return {"host": host, "user": _env_or_file("TAPHOME_USER"), "password": _env_or_file("TAPHOME_PASSWORD")}


def password_hash(pwd: str) -> str:
    salt = os.environ.get("TAPHOME_PASSWORD_SALT") or (SECRETS / "salt.txt").read_text().rstrip("\n")
    h = (pwd + salt).encode()
    for _ in range(2):
        h = hashlib.sha512(h).digest()
    digest = h.hex()
    _secret_values.update({pwd, salt, digest})
    return digest


def redact(o):
    if isinstance(o, dict):
        return {k: ("<redacted>" if k in SECRET_KEYS and v else redact(v)) for k, v in o.items()}
    if isinstance(o, list):
        return [redact(x) for x in o]
    if isinstance(o, str) and o in _secret_values:
        return "<redacted>"
    return o


def show(o):
    print(json.dumps(redact(o), indent=2, ensure_ascii=False))


def write_capture(name: str, data: bytes) -> Path:
    """Write a capture readable by the owner only (captures hold the real configuration)."""
    CAPTURES.mkdir(mode=0o700, parents=True, exist_ok=True)
    CAPTURES.chmod(0o700)
    p = CAPTURES / name
    fd = os.open(p, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    with os.fdopen(fd, "wb") as f:
        f.write(data)
    p.chmod(0o600)  # O_CREAT keeps the mode of an existing file
    return p


def save_raw(name: str, frames: list):
    stamp = time.strftime("%Y%m%d-%H%M%S")
    for i, data in enumerate(frames):
        write_capture("%s-%s-%d.bin" % (name, stamp, i), data)


# ---------------------------------------------------------------- session store

def load_session():
    p = SECRETS / "session.json"
    return json.loads(p.read_text()) if p.exists() else None


def store_session(location_id, user, session_id):
    SECRETS.mkdir(parents=True, exist_ok=True)
    p = SECRETS / "session.json"
    # the Core knows local logins in lower case (the app lower-cases them), so reconnect must match login
    p.write_text(json.dumps({"location_id": location_id, "user": user.lower(), "session_id": session_id}))
    p.chmod(0o600)


async def do_login(cfg) -> dict:
    if not cfg["user"] or not cfg["password"]:
        sys.exit("TAPHOME_USER and TAPHOME_PASSWORD (or *_FILE) are required to log in")
    info = await client.identify(cfg["host"])
    resp = await client.login(cfg["host"], info["LocationId"], cfg["user"], password_hash(cfg["password"]))
    result = resp.get("LoginResultId")
    if result != 1:
        sys.exit("login failed: LoginResultId=%s" % result)
    store_session(info["LocationId"], cfg["user"], resp["SessionId"])
    _secret_values.add(resp["SessionId"])
    return resp


@contextlib.asynccontextmanager
async def opened(cfg):
    s = await open_session(cfg)
    try:
        yield s
    finally:
        await s.__aexit__(None, None, None)


async def open_session(cfg) -> client.Session:
    """Open a Session with the stored session id, logging in again on WrongSession (2)."""
    for attempt in (0, 1):
        st = load_session()
        if st is None or attempt:
            await do_login(cfg)
            st = load_session()
        _secret_values.add(st["session_id"])
        s = client.Session(cfg["host"], st["location_id"], st["user"], st["session_id"])
        await s.__aenter__()
        if s.reconnect.get("ReconnectResultId") == 4 and s.reconnect.get("DbHash"):
            # FaultyStorageClient: our cached DbHash is stale; the Core sends the current one, retry with it
            await s.__aexit__(None, None, None)
            s = client.Session(cfg["host"], st["location_id"], st["user"], st["session_id"], s.reconnect["DbHash"])
            await s.__aenter__()
        if s.reconnect.get("ReconnectResultId") == 1:
            return s
        await s.__aexit__(None, None, None)
        if attempt or s.reconnect.get("ReconnectResultId") != 2:
            sys.exit("reconnect failed: ReconnectResultId=%s" % s.reconnect.get("ReconnectResultId"))
    raise AssertionError


# ---------------------------------------------------------------- read-only allowlist

def read_allowlist() -> tuple:
    """(read-only requests, write requests) from the committed manifests openapi/operations/*.yaml.

    A request class used by any write/action entry is never read-only, even if a read entry shares it.
    """
    ok, bad = set(), set()
    for f in sorted((ROOT / "openapi" / "operations").glob("*.yaml")):
        kind = req = None
        for line in f.read_text().splitlines() + ["- id: end"]:
            if line.startswith("- id:"):
                if req and kind in ("read", "subscribe"):
                    ok.add(req)
                elif req:
                    bad.add(req)
                kind = req = None
            elif (m := re.match(r"^  kind: (\w+)", line)):
                kind = m.group(1)
            elif (m := re.match(r"^  request: (\w+)$", line)):
                req = m.group(1)
    return ok - bad, bad


def is_allowed(name: str) -> bool:
    ok, bad = read_allowlist()
    return name in ok or (name.startswith("MessageGet") and name not in bad)


# ---------------------------------------------------------------- commands

async def cmd_identify(cfg, args):
    raw = []
    info = await client.identify(cfg["host"], raw)
    save_raw("identify", raw)
    show(info)
    print("fields: " + ", ".join("%s=%s" % (k, "set" if v not in (None, "", False, "00000000-0000-0000-0000-000000000000") else "empty")
                                 for k, v in info.items() if k != "$type"))


async def cmd_version_probe(cfg, args):
    info = await client.identify(cfg["host"])
    async with client.Session(cfg["host"], info["LocationId"], cfg["user"] or "probe", client.new_session_id()) as s:
        save_raw("version-probe", s.raw)
        r = s.reconnect
        print("ReconnectResultId=%s CurrentProtocolVersion=%s CoreVersionString=%s" % (
            r.get("ReconnectResultId"), r.get("CurrentProtocolVersion"), r.get("CoreVersionString")))


async def cmd_login(cfg, args):
    if load_session():
        async with opened(cfg):
            pass
        print("stored session is valid (ReconnectResultId=1)")
    else:
        resp = await do_login(cfg)
        print("LoginResult=%s (session stored in work/secrets/session.json)" % resp["LoginResultId"])


async def cmd_sync(cfg, args):
    async with opened(cfg) as s:
        r = s.reconnect
    out = write_capture("sync-%s.json" % time.strftime("%Y%m%d-%H%M%S"),
                        json.dumps(r, indent=1, ensure_ascii=False).encode("utf-8"))
    print("devices=%d ccus=%d smart_rules=%d dashboards=%d protocol=%s core=%s\nwritten: %s" % (
        len(r.get("Devices", [])), len(r.get("Ccus", [])), len(r.get("SmartRules", [])),
        len(r.get("Dashboards", [])), r.get("CurrentProtocolVersion"), r.get("CoreVersionString"),
        out.relative_to(ROOT)))


async def cmd_call(cfg, args):
    if not (args.allow_write or is_allowed(args.message)):
        sys.exit("refused: %s is not in the read-only allowlist (openapi/operations kind read/subscribe, or MessageGet*). "
                 "Pass --allow-write to send it." % args.message)
    fields = json.loads(args.json) if args.json else {}
    async with opened(cfg) as s:
        resp = await s.request(args.message, fields, 60)
        if args.wait_broadcasts:
            await s.wait(args.wait_broadcasts)
        show(resp)
        if args.wait_broadcasts:
            print("broadcasts:", dict(collections.Counter(b["$type"] for b in s.broadcasts)))


async def cmd_listen(cfg, args):
    async with opened(cfg) as s:
        await s.wait(args.seconds)
        for name, n in collections.Counter(b["$type"] for b in s.broadcasts).most_common():
            print("%5d  %s" % (n, name))


def main():
    ap = argparse.ArgumentParser(prog="probe", description=__doc__.split("\n")[0])
    sub = ap.add_subparsers(dest="cmd", required=True)
    for n in ("identify", "version-probe", "login", "sync"):
        sub.add_parser(n)
    c = sub.add_parser("call")
    c.add_argument("message")
    c.add_argument("--json")
    c.add_argument("--wait-broadcasts", type=float, default=0)
    c.add_argument("--allow-write", action="store_true")
    ls = sub.add_parser("listen")
    ls.add_argument("seconds", type=float)
    args = ap.parse_args()
    cfg = config()
    if cfg["password"]:
        _secret_values.add(cfg["password"])
    fn = globals()["cmd_" + args.cmd.replace("-", "_")]
    asyncio.run(fn(cfg, args))


main()
