"""Reproduce the denial-of-service findings of docs/SECURITY_REVIEW.md (section 14) against a LOCAL server.

    RUSETECT_DEMO=1 RUSETECT_RATE_LIMIT=100000 ./target/release/rusetect-server --port 8012 &
    python3 -I tools/security_repro.py --base http://localhost:8012 --pid $(pgrep -x rusetect-server) --label after

Never point it at a deployment you do not own: the last probe hung the server before the fix. Results are merged into
results/security_repro.json under --label.
"""
import argparse
import json
import os
import time
import urllib.error
import urllib.request

MIB2 = 2 * 1024 * 1024


def post(base, path, data, timeout):
    req = urllib.request.Request(base + path, data=data, headers={"content-type": "application/json"})
    t = time.time()
    try:
        with urllib.request.urlopen(req, timeout=timeout) as r:
            code, body = r.status, r.read()
    except urllib.error.HTTPError as e:
        code, body = e.code, e.read()
    except (TimeoutError, OSError):
        return {"status": "no answer", "seconds": round(time.time() - t, 2)}
    return {"status": code, "answer_bytes": len(body), "seconds": round(time.time() - t, 2)}


def peak_rss_mb(pid):
    if not pid:
        return None
    for line in open(f"/proc/{pid}/status"):
        if line.startswith("VmHWM:"):
            return round(int(line.split()[1]) / 1024)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--base", default="http://localhost:8012")
    ap.add_argument("--pid", type=int)
    ap.add_argument("--label", required=True)
    ap.add_argument("--out", default=os.path.join(os.path.dirname(__file__), "..", "results", "security_repro.json"))
    a = ap.parse_args()
    out = {"peak_rss_mb_at_start": peak_rss_mb(a.pid)}
    n = (MIB2 - 10) // 3
    out["batch_2mib_of_empty_items"] = post(a.base, "/api/v2/payments/batch", ("[" + ",".join(["{}"] * n) + "]").encode(), 120)
    out["peak_rss_mb_after_batch"] = peak_rss_mb(a.pid)
    contacts = '{"account":"x","normal_pin":"1234","duress_pin":"9999","trusted_contacts":[' + ",".join(["1"] * ((MIB2 - 100) // 2)) + "]}"
    out["duress_2mib_of_bad_contacts"] = post(a.base, "/api/v2/duress/register", contacts.encode(), 120)
    pays = [{"timestamp": "2022-09-%02d %02d:%02d" % (1 + i // 1440 % 28, i // 60 % 24, i % 60), "from_bank": str(1 + i % 997),
             "from_account": "%09X" % (i * 7919), "to_bank": str(1 + i * 31 % 997), "to_account": "%09X" % (i * 104729 + 1),
             "amount_paid": 100.0 + i, "payment_currency": "US Dollar", "receiving_currency": "US Dollar", "payment_format": "ACH"}
            for i in range(8000)]
    out["batch_8000_valid_payments_state_lock"] = post(a.base, "/api/v2/payments/batch", json.dumps(pays, separators=(",", ":")).encode(), 120)
    # last: before the fix this request never ended and held the state lock, so every later request waited
    out["message_a_dot_x2300"] = post(a.base, "/api/v2/message", json.dumps({"text": "a." * 2300}).encode(), 30)
    out["health_after"] = post(a.base, "/api/health", None, 10) if out["message_a_dot_x2300"]["status"] == 200 else "not tried: server hung"
    try:
        res = json.load(open(a.out))
    except FileNotFoundError:
        res = {}
    res[a.label] = out
    with open(a.out, "w") as f:
        json.dump(res, f, indent=1)
        f.write("\n")
    print(json.dumps(out, indent=1))


if __name__ == "__main__":
    main()
