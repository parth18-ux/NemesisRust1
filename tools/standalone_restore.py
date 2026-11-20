"""Rehearse a private Redis snapshot restore against the bundled payments fixture."""

import argparse
import base64
import hashlib
import json
import os
from pathlib import Path
import secrets
import shutil
import socket
import subprocess
import tempfile
import time
from contextlib import contextmanager
from urllib.error import HTTPError
from urllib.request import ProxyHandler, Request, build_opener

from pilot_rehearsal import ROOT, fixture, sender


def free_port():
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


@contextmanager
def process(cmd, log_path, env=None):
    with log_path.open("wb") as log:
        child = subprocess.Popen(cmd, env=env, stdout=log, stderr=subprocess.STDOUT)
        try:
            yield child
        finally:
            child.terminate()
            try:
                child.wait(timeout=10)
            except subprocess.TimeoutExpired:
                child.kill()
                child.wait()


def ready(child, check, name):
    deadline = time.monotonic() + 30
    while time.monotonic() < deadline and child.poll() is None:
        try:
            if check():
                return
        except (OSError, subprocess.CalledProcessError):
            pass
        time.sleep(0.1)
    raise RuntimeError(f"{name} did not become ready (exit={child.poll()})")


def redis(port, *arguments):
    return subprocess.run(
        ["redis-cli", "--raw", "-h", "127.0.0.1", "-p", str(port), *arguments],
        check=True, capture_output=True, text=True, timeout=10,
    ).stdout.strip()


@contextmanager
def store(port, directory, persistence):
    command = [
        "redis-server", "--bind", "127.0.0.1", "--port", str(port), "--dir", str(directory),
        "--dbfilename", "dump.rdb", "--save", "", "--appendonly", persistence,
        "--maxmemory-policy", "noeviction",
    ]
    with process(command, directory / "redis.log") as child:
        ready(child, lambda: redis(port, "PING") == "PONG", "Redis")
        yield


def call(base, path, *, key=None, body=None, nonce=None, expected=200):
    headers = {}
    if key:
        headers["X-Analyst-Key"] = key
    if nonce:
        headers["X-Nonce"] = nonce
    data = None
    if body is not None:
        headers["Content-Type"] = "application/json"
        data = json.dumps(body).encode()
    request = Request(base + path, headers=headers, data=data)
    opener = build_opener(ProxyHandler({}))
    try:
        with opener.open(request, timeout=10) as response:
            status, result = response.status, json.load(response)
    except HTTPError as exc:
        with exc:
            status, result = exc.code, None
    require(status == expected, f"{path} returned {status}, expected {expected}")
    return result


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


@contextmanager
def app(binary, port, directory, keys):
    env = {key: value for key, value in os.environ.items()
           if not key.startswith("RUSETECT_") and key not in ("KV_URL", "VERCEL")}
    env.update(keys, REDIS_URL=f"redis://127.0.0.1:{port}/0", RUSETECT_REDIS_PREFIX="restore:",
               RUSETECT_ROOT=ROOT, RUSETECT_RATE_LIMIT="1000000", NO_PROXY="127.0.0.1", no_proxy="127.0.0.1")
    app_port = free_port()
    base = f"http://127.0.0.1:{app_port}"
    with process([binary, "--host", "127.0.0.1", "--port", str(app_port)], directory / "app.log", env) as child:
        ready(child, lambda: call(base, "/api/health")["ready"], "RuseTect")
        health = call(base, "/api/health")
        require(health["durable"] and health["auth"] == "configured keys", "expected configured keys and durable Redis")
        yield base, env


def counts(port):
    return {
        "sequence": int(redis(port, "GET", "restore:live:seq")),
        "ledger": int(redis(port, "LLEN", "restore:live:ledger")),
        "alerts": int(redis(port, "HLEN", "restore:live:alerts")),
    }


def verify(base, alert_id, pack):
    signed_fields = {"alert_id": alert_id, "pack_hash": pack["pack_hash"]}
    signature = pack["signature"]["signature"]
    verdict = call(base, "/api/v2/verify", body={"signed_fields": signed_fields, "signature": signature})
    require(verdict["valid"], "evidence signature failed verification")


def drill(binary, number, scratch):
    source, target = scratch / "source", scratch / "restored"
    source.mkdir()
    target.mkdir()
    payments = scratch / "payments.jsonl"
    fixture(number, payments)
    viewer, analyst = secrets.token_hex(24), secrets.token_hex(24)
    keys = {
        "RUSETECT_ANALYST_KEYS": json.dumps({
            viewer: {"analyst": "restore-viewer", "role": "viewer"},
            analyst: {"analyst": "restore-analyst", "role": "analyst"},
        }),
        "RUSETECT_SIGNING_KEY": base64.b64encode(secrets.token_bytes(32)).decode(),
        "RUSETECT_CONSORTIUM_SECRET": secrets.token_hex(32),
        "RUSETECT_HASH_SALT": secrets.token_hex(16),
        "RUSETECT_WEBHOOK_SECRET": secrets.token_hex(32),
    }
    source_port = free_port()
    with store(source_port, source, "yes"), app(binary, source_port, source, keys) as (base, env):
        first_log = str(scratch / "first.jsonl")
        first = sender(str(payments), base + "/api/v2/events/payment", env, first_log)
        require(first["accepted"] == number and first["failed"] == 0, "fixture ingest was incomplete")
        original = counts(source_port)
        require(original["sequence"] == original["ledger"] == number, "incomplete Redis decision ledger")
        bind = {"key": viewer}
        original_feed = call(base, "/api/v2/live", **bind)["events"]
        alerts = call(base, "/api/v2/alerts?limit=1", **bind)["alerts"]
        require(alerts, "fixture slice produced no alert; increase --payments")
        alert = alerts[0]
        alert_id = alert["alert_id"]
        review_path = f"/api/v2/alerts/{alert_id}/review"
        evidence_path = f"/api/v2/alerts/{alert_id}/evidence"
        call(base, evidence_path, expected=401)
        review = {"action": "hold", "note": "Synthetic restore rehearsal"}
        call(base, review_path, key=viewer, body=review, nonce=secrets.token_hex(12), expected=403)
        reviewed = call(base, review_path, key=analyst, body=review, nonce=secrets.token_hex(12))
        require(reviewed["status"] == "held", "analyst action did not apply")
        original_pack = call(base, evidence_path, **bind)
        verify(base, alert_id, original_pack)
        original_audit = call(base, "/api/v2/audit", **bind)
        require(original_audit["chain_valid"] and original_pack["audit_entries"], "source process audit is missing")
        public_key = call(base, "/api/v2/keys/public")
        require(not public_key["ephemeral"], "the signing identity is ephemeral")
        require(redis(source_port, "SAVE") == "OK", "Redis snapshot failed")
        snapshot = source / "dump.rdb"
        with snapshot.open("rb") as data:
            snapshot_hash = hashlib.file_digest(data, "sha256").hexdigest()
        shutil.copyfile(snapshot, target / "dump.rdb")

    restore_port = free_port()
    with store(restore_port, target, "no"), app(binary, restore_port, target, keys) as (base, env):
        require(counts(restore_port) == original, "RESTORE changed Redis record counts")
        require(call(base, "/api/v2/keys/public") == public_key, "signing identity changed")
        require(call(base, "/api/v2/live", key=viewer)["events"] == original_feed, "RESTORE changed the live feed")
        verify(base, alert_id, original_pack)
        restored_pack = call(base, evidence_path, key=viewer)
        verify(base, alert_id, restored_pack)
        require(restored_pack["alert"] == original_pack["alert"], "RESTORE changed the alert or its actions")
        restored_audit = call(base, "/api/v2/audit", key=viewer)
        second_log = str(scratch / "second.jsonl")
        replay = sender(str(payments), base + "/api/v2/events/payment", env, second_log)
        require(replay["replayed"] == number and replay["accepted"] == replay["failed"] == 0,
                "RESTORE lost idempotency")
        fields = ("event_id", "action", "final_decision", "risk", "alert_id")
        with open(first_log, encoding="utf-8") as first_rows, open(second_log, encoding="utf-8") as second_rows:
            for left, right in zip(first_rows, second_rows, strict=True):
                before, after = json.loads(left), json.loads(right)
                require(all(before[key] == after[key] for key in fields), "RESTORE changed a replayed decision")
        require(counts(restore_port) == original, "replay changed durable record counts")

    return {
        "pass": True, "fixture_payments": number, "durable_records": original,
        "snapshot_sha256": snapshot_hash, "store": redis_version(), "replayed": replay["replayed"],
        "authorization": {"anonymous_evidence": 401, "viewer_review": 403},
        "signing_identity_preserved": True, "original_and_restored_signatures_verify": True,
        "alert_and_analyst_action_preserved": True, "app_audit_entries_before": len(original_audit["entries"]),
        "app_audit_entries_after": len(restored_audit["entries"]),
        "evidence_audit_entries_before": len(original_pack["audit_entries"]),
        "evidence_audit_entries_after": len(restored_pack["audit_entries"]),
        "regenerated_pack_hash_unchanged": original_pack["pack_hash"] == restored_pack["pack_hash"],
    }


def redis_version():
    return subprocess.check_output(["redis-server", "--version"], text=True).strip()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=Path(ROOT) / "target/release/rusetect-server")
    parser.add_argument("--payments", type=int, default=2_000)
    args = parser.parse_args()
    if args.payments < 1 or not args.binary.is_file():
        parser.error("require a positive --payments and a built --binary")
    for binary in ("redis-server", "redis-cli"):
        if shutil.which(binary) is None:
            parser.error(f"{binary} is required on PATH")
    try:
        with tempfile.TemporaryDirectory(prefix="rusetect-restore-") as directory:
            result = drill(str(args.binary.resolve()), args.payments, Path(directory))
    except (OSError, RuntimeError, subprocess.SubprocessError) as exc:
        parser.exit(1, f"restore drill: {exc}\n")
    print(json.dumps(result, indent=2))


if __name__ == "__main__":
    main()
