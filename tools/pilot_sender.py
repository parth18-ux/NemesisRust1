"""Pilot sender: sign and post each payment in a CSV or JSON-lines file to RuseTect's webhook.

    RUSETECT_WEBHOOK_SECRET=... python3 -I tools/pilot_sender.py payments.jsonl \
        --url http://127.0.0.1:8000/api/v2/events/payment --rate 20 --log results.jsonl
    python3 -I tools/pilot_sender.py --self-test

Each record is the webhook body (docs/PILOT_ONBOARDING.md, field mapping): event_id, timestamp, from_bank,
from_account, to_bank, to_account, amount_paid, payment_currency, payment_format, optional receiving_currency.
CSV columns are sent as strings except amount_paid (a number); empty cells are dropped.

Signing: X-RuseTect-Signature: v1=<hex HMAC-SHA256 of "{ts}.{raw body}">, X-RuseTect-Timestamp: epoch seconds, a fresh
timestamp per attempt. Retries network errors, 409, 429 and 5xx with the same body and event_id (exponential backoff
with full jitter, honouring Retry-After when larger); other 4xx are not retried. The secret is read from an
environment variable (--secret-env) and never printed. Prints a JSON summary on stdout; standard library only.
"""
import argparse, csv, hashlib, hmac, json, os, random, sys, time, urllib.error, urllib.request

NUMERIC = {"amount_paid"}


def read_events(path):
    """Yield the records of a .csv file or a JSON-lines file (one object per line)."""
    with open(path, newline="", encoding="utf-8") as f:
        if path.lower().endswith(".csv"):
            for row in csv.DictReader(f):
                yield {k: (float(v) if k in NUMERIC else v) for k, v in row.items() if v not in ("", None)}
        else:
            for line in f:
                if line.strip():
                    yield json.loads(line)


def sign(secret, ts, body):
    return "v1=" + hmac.new(secret, ts.encode() + b"." + body, hashlib.sha256).hexdigest()


def post_once(url, secret, body, timeout):
    """One signed POST. Returns (status, headers, parsed body or None)."""
    ts = str(int(time.time()))
    req = urllib.request.Request(url, data=body, method="POST", headers={
        "Content-Type": "application/json", "X-RuseTect-Timestamp": ts, "X-RuseTect-Signature": sign(secret, ts, body)})
    try:
        with urllib.request.urlopen(req, timeout=timeout) as r:
            return r.status, r.headers, json.loads(r.read() or b"null")
    except urllib.error.HTTPError as e:
        raw = e.read()
        try:
            return e.code, e.headers, json.loads(raw or b"null")
        except ValueError:
            return e.code, e.headers, None


def send(url, secret, event, attempts=8, base=1.0, cap=60.0, timeout=15.0, sleep=time.sleep):
    """Send one event with retries. Returns a result dict (no secret in it)."""
    body = json.dumps(event, separators=(",", ":")).encode()
    out = {"event_id": event.get("event_id"), "attempts": 0}
    for n in range(attempts):
        out["attempts"] = n + 1
        t0 = time.perf_counter()
        try:
            status, headers, resp = post_once(url, secret, body, timeout)
        except OSError as e:  # network error or timeout: the event may or may not be committed, so retry
            out.update(status=None, error=type(e).__name__)
            sleep(random.uniform(0, min(cap, base * 2 ** n)))
            continue
        out["status"], out["rtt_ms"] = status, round((time.perf_counter() - t0) * 1000, 2)
        if status == 200:
            out.update(action=resp.get("action"), final_decision=resp.get("final_decision"), alert_id=resp.get("alert_id"),
                       latency_ms=resp.get("latency_ms"), replayed=headers.get("Idempotent-Replayed") == "true")
            out.pop("error", None)
            return out
        out["error"] = (resp or {}).get("detail") if isinstance(resp, dict) else None
        if status < 500 and status not in (409, 429):
            return out  # 401, 413, 422: retrying will not help
        retry_after = float(headers.get("Retry-After") or 0)
        sleep(max(retry_after, random.uniform(0, min(cap, base * 2 ** n))))
    out["gave_up"] = True
    return out


def run(path, url, secret, rate, attempts, log=None, base=1.0):
    """Send every event in `path`, at most `rate` per second. Returns the summary."""
    s = {"sent": 0, "accepted": 0, "replayed": 0, "failed": 0, "retried": 0, "actions": {}, "failures": {}}
    gap, nxt = (1.0 / rate if rate > 0 else 0.0), time.monotonic()
    logf = open(log, "w", encoding="utf-8") if log else None
    try:
        for ev in read_events(path):
            wait = nxt - time.monotonic()
            if wait > 0:
                time.sleep(wait)
            nxt = max(nxt, time.monotonic()) + gap
            r = send(url, secret, ev, attempts=attempts, base=base)
            s["sent"] += 1
            s["retried"] += r["attempts"] > 1
            if r.get("status") == 200:
                s["replayed" if r["replayed"] else "accepted"] += 1
                s["actions"][r["action"]] = s["actions"].get(r["action"], 0) + 1
            else:
                s["failed"] += 1
                k = str(r.get("status") or r.get("error"))
                s["failures"][k] = s["failures"].get(k, 0) + 1
            if logf:
                logf.write(json.dumps(r) + "\n")
    finally:
        if logf:
            logf.close()
    return s


def self_test():
    """Offline check against a stub webhook: signature, CSV and JSON-lines input, 503 + Retry-After, idempotent replay."""
    import tempfile, threading
    from http.server import BaseHTTPRequestHandler, HTTPServer
    secret, seen, calls = b"self-test-secret-not-real-0000", {}, {"n": 0}

    class Stub(BaseHTTPRequestHandler):
        def do_POST(self):
            body = self.rfile.read(int(self.headers["Content-Length"]))
            ts, sig = self.headers["X-RuseTect-Timestamp"], self.headers["X-RuseTect-Signature"]
            calls["n"] += 1
            if not hmac.compare_digest(sig, sign(secret, ts, body)) or abs(time.time() - int(ts)) > 300:
                return self.reply(401, {"detail": "bad signature"})
            ev = json.loads(body)
            if ev["event_id"] == "e2" and calls["n"] == 2:  # first attempt of e2 is refused once
                return self.reply(503, {"detail": "store unreachable"}, {"Retry-After": "0"})
            if not isinstance(ev["amount_paid"], float):
                return self.reply(422, {"detail": "amount_paid must be a number"})
            hdr = {"Idempotent-Replayed": "true"} if ev["event_id"] in seen else {}
            seen.setdefault(ev["event_id"], "hold" if ev["amount_paid"] > 5000 else "allow")
            self.reply(200, {"action": seen[ev["event_id"]], "final_decision": "x", "alert_id": None, "latency_ms": 1.0}, hdr)

        def reply(self, code, obj, hdr=None):
            raw = json.dumps(obj).encode()
            self.send_response(code)
            for k, v in {"Content-Type": "application/json", "Content-Length": str(len(raw)), **(hdr or {})}.items():
                self.send_header(k, v)
            self.end_headers()
            self.wfile.write(raw)

        def log_message(self, *a):
            pass

    srv = HTTPServer(("127.0.0.1", 0), Stub)
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    url = f"http://127.0.0.1:{srv.server_address[1]}/api/v2/events/payment"
    rec = {"timestamp": "2022-09-09 10:00", "from_bank": "070", "from_account": "C1", "to_bank": "011", "to_account": "M1",
           "payment_currency": "US Dollar", "payment_format": "ACH"}
    with tempfile.TemporaryDirectory() as d:
        jl, cv = os.path.join(d, "p.jsonl"), os.path.join(d, "p.csv")
        with open(jl, "w") as f:
            for i, amt in [(1, 100.0), (2, 9000.0), (1, 100.0)]:
                f.write(json.dumps({**rec, "event_id": f"e{i}", "amount_paid": amt}) + "\n")
        with open(cv, "w", newline="") as f:
            w = csv.DictWriter(f, ["event_id", *rec, "amount_paid", "receiving_currency"])
            w.writeheader()
            w.writerow({**rec, "event_id": "c1", "amount_paid": "250", "receiving_currency": ""})
        a = run(jl, url, secret, rate=0, attempts=3, base=0.01)
        b = run(cv, url, b"wrong-secret-0000000000000000", rate=0, attempts=3, base=0.01)
        c = run(cv, url, secret, rate=0, attempts=3, base=0.01)
    srv.shutdown()
    assert a == {"sent": 3, "accepted": 2, "replayed": 1, "failed": 0, "retried": 1, "actions": {"allow": 2, "hold": 1},
                 "failures": {}}, a
    assert b["failed"] == 1 and b["failures"] == {"401": 1} and b["retried"] == 0, b
    assert c["accepted"] == 1, c
    assert secret.decode() not in json.dumps([a, b, c])
    print("pilot_sender self-test passed")


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("file", nargs="?", help=".csv or JSON-lines file of webhook bodies")
    ap.add_argument("--url", default="http://127.0.0.1:8000/api/v2/events/payment")
    ap.add_argument("--rate", type=float, default=20.0, help="events per second (0: no limit)")
    ap.add_argument("--attempts", type=int, default=8)
    ap.add_argument("--secret-env", default="RUSETECT_WEBHOOK_SECRET", help="environment variable holding the secret")
    ap.add_argument("--log", help="write one JSON line per event (no secret) to this file")
    ap.add_argument("--self-test", action="store_true")
    a = ap.parse_args()
    if a.self_test:
        return self_test()
    if not a.file:
        ap.error("a file is required")
    secret = os.environ.get(a.secret_env, "")
    if len(secret) < 16:
        sys.exit(f"set {a.secret_env} (16+ characters) in the environment")
    print(json.dumps(run(a.file, a.url, secret.encode(), a.rate, a.attempts, a.log), sort_keys=True))


if __name__ == "__main__":
    main()
