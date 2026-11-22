"""Shadow-pilot rehearsal: run the server locally in memory mode, send a fixture stream through tools/pilot_sender.py,
check alerts, ledger, live feed and drift, re-send everything to check idempotency, then run the fixed-budget evaluators
(tools/pilot_freeze_policy.py, pilot_evaluate.py, pilot_queue.py) on the replay log, and record what was measured.

    cargo build --release -p rusetect-server --bin rusetect-server
    python3 -I tools/pilot_rehearsal.py [--n 0] [--reviews-per-day 50]

Fixture: the first N payments (0: all) of service/models/demo_stream.csv.gz (IBM AMLworld HI-Small, synthetic), in
time order, mapped to webhook bodies. The stream covers one calendar day, and the evaluators work in UTC days, so each
clock hour of it is re-dated to its own day (hour 11 to 2022-09-01, hour 12 to 2022-09-02, ...), keeping the time of
day: a plumbing rehearsal, not a measurement of the engine (gaps between hours become a day long). The engine starts
with no account history, as a fresh pilot install would before the bank's history is replayed. The server runs with
analyst keys and throwaway secrets generated for this run only (never printed or written to disk).
Labels: the stream's synthetic `y`; each labelled payment's case is its payee account, confirmed two days after it.
Output: results/pilot_rehearsal.json, and the generated block of docs/PILOT_REHEARSAL.md rewritten from it.
"""
import argparse, base64, csv, gzip, json, bisect, datetime as dt, os, re, secrets, socket, statistics, subprocess, sys, tempfile, time, urllib.request

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BIN = os.path.join(ROOT, "target", "release", "rusetect-server")
OUT = os.path.join(ROOT, "results", "pilot_rehearsal.json")
DOC = os.path.join(ROOT, "docs", "PILOT_REHEARSAL.md")
RATES = json.load(open(os.path.join(ROOT, "service", "models", "cascade_config.json")))["usd_rates"]
DAY0 = dt.date(2022, 9, 1)  # hour 11 of the stream
STREAM = os.path.join(ROOT, "service", "models", "demo_stream.csv.gz")


def fixture(n, path):
    """Write the webhook bodies; return the rows the evaluators need (never sent): id, time, payer, payee, USD amount, label."""
    rows = []
    with gzip.open(STREAM, "rt", newline="") as f, open(path, "w", encoding="utf-8") as out:
        for i, r in enumerate(csv.DictReader(f)):
            if i == n:
                break
            hh = int(r["ts"][11:13])
            ts = f"{DAY0 + dt.timedelta(days=hh - 11)} {r['ts'][11:]}"
            eid = f"rehearsal-{i:06d}"
            out.write(json.dumps({"event_id": eid, "timestamp": ts,
                                  "from_bank": r["fb"], "from_account": r["fa"], "to_bank": r["tb"], "to_account": r["ta"],
                                  "amount_paid": float(r["amt_paid"]), "payment_currency": r["cur_paid"],
                                  "receiving_currency": r["cur_recv"], "payment_format": r["fmt"]}) + "\n")
            rows.append((eid, dt.datetime.fromisoformat(ts), f"{r['fb']}:{r['fa']}", f"{r['tb']}:{r['ta']}",
                         float(r["amt_paid"]) * RATES.get(r["cur_paid"], 1.0), r["y"] == "1"))
    return rows


def tool(name, *args):
    r = subprocess.run([sys.executable, "-I", os.path.join(ROOT, "tools", name), *map(str, args)], capture_output=True, text=True)
    return json.loads(r.stdout) if r.returncode == 0 else {"_failed": r.returncode, "_stderr": r.stderr.strip()[-400:]}


def fixed_budget(rows, log, d, budget):
    """The PILOT.md evaluators on the first-pass replay log: freeze on the first three days, hold out the rest."""
    risk = {r["event_id"]: r.get("risk") for r in map(json.loads, open(log)) if r.get("status") == 200}
    iso = lambda t: t.strftime("%Y-%m-%dT%H:%M:00Z")
    last, velocity = {}, {}
    for eid, t, payer, *_ in rows:  # trailing 24 h count of the payer's earlier payments, payment-time data only
        ts = last.setdefault(payer, [])
        velocity[eid] = len(ts) - bisect.bisect_right(ts, t - dt.timedelta(hours=24))
        ts.append(t)
    files = {k: os.path.join(d, k + ".csv") for k in ("incumbent", "outcomes", "payments")}
    with open(files["incumbent"], "w", newline="") as fi, open(files["outcomes"], "w", newline="") as fo, \
            open(files["payments"], "w", newline="") as fp:
        wi, wo, wp = csv.writer(fi), csv.writer(fo), csv.writer(fp)
        wi.writerow(("event_id", "score")); wo.writerow(("event_id", "case_id", "confirmed_at"))
        wp.writerow(("event_id", "timestamp", "review_group", "served", "cascade", "amount_usd", "velocity"))
        for eid, t, payer, payee, usd, y in rows:
            wi.writerow((eid, round(usd, 2)))
            if y:
                wo.writerow((eid, payee, iso(t + dt.timedelta(days=2))))
            served = risk.get(eid)
            # the webhook returns one risk; the queue tool's separate cascade column gets the same value here
            wp.writerow((eid, iso(t), payer, served, served, round(usd, 2), velocity[eid]))
    days = sorted({t.date() for _, t, *_ in rows})
    val_end, end = days[0] + dt.timedelta(days=3), days[-1] + dt.timedelta(days=1)
    as_of = f"{end + dt.timedelta(days=7)}T00:00:00Z"
    policy = tool("pilot_freeze_policy.py", files["payments"], "--start", days[0], "--end", val_end,
                  "--reviews-per-day", budget, "--random-seed", 41)
    pol = os.path.join(d, "policy.json")
    json.dump(policy, open(pol, "w"))
    window = ("--start", val_end, "--end", end, "--as-of", as_of, "--reviews-per-day", budget)
    return {
        "setup": {"validation_utc": [str(days[0]), str(val_end)], "holdout_utc": [str(val_end), str(end)], "labels_as_of": as_of,
                  "reviews_per_utc_day": budget, "incumbent_stand_in": "USD amount (a naive rule; no real incumbent exists)",
                  "review_group": "payer bank:account", "case": "payee bank:account of each labelled payment",
                  "velocity": "payer's payments in the trailing 24 h", "cascade_column": "set to the served risk"},
        "policy": policy,
        "ranking": tool("pilot_evaluate.py", log, files["incumbent"], files["outcomes"], *window),
        "queue": tool("pilot_queue.py", files["payments"], files["outcomes"], pol, *window),
    }


def get(base, path, key=None):
    req = urllib.request.Request(base + path, headers={"X-Analyst-Key": key} if key else {})
    try:
        with urllib.request.urlopen(req, timeout=30) as r:
            return r.status, json.load(r)
    except urllib.error.HTTPError as e:
        return e.code, {"_error": e.read().decode(errors="replace")[:300]}


def pct(xs, q):
    xs = sorted(xs)
    return round(xs[min(len(xs) - 1, int(q * len(xs)))], 2) if xs else None


def sender(file, url, env, log):
    r = subprocess.run([sys.executable, "-I", os.path.join(ROOT, "tools", "pilot_sender.py"), file, "--url", url,
                        "--rate", "0", "--log", log], env=env, capture_output=True, text=True, check=True)
    return json.loads(r.stdout.strip().splitlines()[-1])


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--n", type=int, default=0, help="payments to send; 0 sends the whole stream")
    ap.add_argument("--reviews-per-day", type=int, default=50)
    a = ap.parse_args()
    if not os.path.exists(BIN):
        sys.exit("build first: cargo build --release -p rusetect-server --bin rusetect-server")
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        port = s.getsockname()[1]
    base = f"http://127.0.0.1:{port}"
    webhook, key = secrets.token_hex(32), secrets.token_hex(24)
    env = {k: v for k, v in os.environ.items() if k not in ("REDIS_URL", "KV_URL", "RUSETECT_DEMO", "RUSETECT_WEBHOOK_SECRET_PREVIOUS")}
    env.update(HOST="127.0.0.1", PORT=str(port), RUSETECT_ROOT=ROOT, RUSETECT_RATE_LIMIT="1000000", RUSETECT_MAX_EVENTS="1000000",
               RUSETECT_WEBHOOK_SECRET=webhook, RUSETECT_ANALYST_KEYS=json.dumps({key: {"analyst": "rehearsal", "role": "viewer"}}),
               RUSETECT_SIGNING_KEY=base64.b64encode(secrets.token_bytes(32)).decode(),
               RUSETECT_CONSORTIUM_SECRET=secrets.token_hex(32), RUSETECT_HASH_SALT=secrets.token_hex(16))
    srv = subprocess.Popen([BIN], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try:
        for _ in range(600):
            try:
                if get(base, "/api/health")[1]["ready"]:
                    break
            except OSError:
                pass
            time.sleep(0.5)
        else:
            sys.exit("server did not become ready")
        with tempfile.TemporaryDirectory() as d:
            fx, log1, log2 = (os.path.join(d, x) for x in ("fixture.jsonl", "pass1.jsonl", "pass2.jsonl"))
            rows = fixture(a.n or -1, fx)
            n_fixture = len(rows)
            url = base + "/api/v2/events/payment"
            first = sender(fx, url, env, log1)
            live_no_key = get(base, "/api/v2/live")[0]
            _, live = get(base, "/api/v2/live", key)
            _, alerts = get(base, "/api/v2/alerts", key)
            _, audit = get(base, "/api/v2/audit", key)
            drift_code, drift = get(base, "/api/v2/drift", key)
            _, health = get(base, "/api/health")
            second = sender(fx, url, env, log2)
            _, alerts2 = get(base, "/api/v2/alerts", key)
            _, audit2 = get(base, "/api/v2/audit", key)
            budget = fixed_budget(rows, log1, d, a.reviews_per_day)
            rows = [json.loads(x) for x in open(log1)]
            rows2 = [json.loads(x) for x in open(log2)]
    finally:
        srv.terminate()
        srv.wait(timeout=30)

    ledger = lambda au: sum(e.get("type") == "live_decision" for e in au["entries"])
    same = sum(r1.get("action") == r2.get("action") and r1.get("alert_id") == r2.get("alert_id") for r1, r2 in zip(rows, rows2))
    ok = [r for r in rows if r.get("status") == 200]
    res = {
        "what": "Shadow-pilot rehearsal: rusetect-server in memory mode with analyst keys and throwaway secrets, "
                "fixture sent through tools/pilot_sender.py, then sent again to check idempotency",
        "script": "tools/pilot_rehearsal.py",
        "fixture": f"{f'first {a.n}' if a.n else 'all'} payments ({n_fixture}) of service/models/demo_stream.csv.gz (IBM AMLworld "
                   "HI-Small, synthetic), time order, each clock hour re-dated to its own UTC day, engine started with no account history",
        "first_pass": {k: first[k] for k in ("sent", "accepted", "replayed", "failed", "retried")},
        "decisions_by_action": dict(sorted(first["actions"].items())),
        "decisions_by_band": dict(sorted({b: sum(r.get("final_decision") == b for r in ok) for b in {r.get("final_decision") for r in ok}}.items())),
        "second_pass": {k: second[k] for k in ("sent", "accepted", "replayed", "failed")},
        "second_pass_same_result": same,
        "alerts_open": len(alerts), "alerts_after_resend": len(alerts2),
        "ledger_entries": ledger(audit), "ledger_entries_after_resend": ledger(audit2), "ledger_chain_valid": audit2["chain_valid"],
        "live_feed": {"events_shown": len(live["events"]), "durable": live["durable"], "without_analyst_key_http": live_no_key},
        "drift": {"http": drift_code, "status": drift["status"], "window_n": drift["window"]["size"], "warming_up": drift["warming_up"],
                  "reasons": drift["reasons"]},
        "health": {k: health[k] for k in ("ready", "durable", "drift", "auth")},
        "latency_ms": {"server_p50": pct([r["latency_ms"] for r in ok], 0.5), "server_p95": pct([r["latency_ms"] for r in ok], 0.95),
                       "round_trip_p50": pct([r["rtt_ms"] for r in ok], 0.5), "round_trip_p95": pct([r["rtt_ms"] for r in ok], 0.95),
                       "note": "one machine, sender and server on loopback, one event at a time; not a capacity figure"},
        "fixed_budget": budget,
    }
    with open(OUT, "w") as f:
        json.dump(res, f, indent=1)
        f.write("\n")
    write_doc(res)
    print(json.dumps(res, indent=1))


def budget_rows(b):
    su, pol, rk, q = b["setup"], b["policy"], b["ranking"], b["queue"]
    fmt = lambda v: "n/a" if v is None else (f"{v:.3f}" if isinstance(v, float) else str(v))
    side = lambda m: (f"{m['reviews']} reviews, {m['confirmed_cases_found']} of the confirmed cases found, "
                      f"yield {fmt(m['confirmed_event_yield'])}, recall {fmt(m['confirmed_case_recall'])}")
    out = [f"| fixed-budget setup | validation {su['validation_utc'][0]} to {su['validation_utc'][1]}, holdout {su['holdout_utc'][0]} to "
           f"{su['holdout_utc'][1]} (UTC, end exclusive), {su['reviews_per_utc_day']} reviews per UTC day; incumbent stand-in: "
           f"{su['incumbent_stand_in']} |"]
    for name, rep in (("frozen policy", pol), ("ranking (`pilot_evaluate.py`)", rk), ("queue (`pilot_queue.py`)", q)):
        if "_failed" in rep:
            out.append(f"| {name} | failed (exit {rep['_failed']}): {rep['_stderr'].splitlines()[-1] if rep['_stderr'] else ''} |")
    if "_failed" not in pol:
        out.append("| frozen thresholds | " + ", ".join(f"{k} {fmt(v)}" for k, v in pol["thresholds"].items()) + " |")
    if "_failed" not in rk:
        out.append(f"| ranking, {rk['payments']} holdout payments, {rk['confirmed_cases']} confirmed cases | RuseTect: {side(rk['rusetect'])}; "
                   f"incumbent stand-in: {side(rk['incumbent'])}; found by both {rk['cases_found_by_both']} |")
    if "_failed" not in q:
        out.append(f"| queue, {q['payments']} holdout payments, {q['confirmed_cases']} confirmed cases | " + "; ".join(
            f"{k}: {side(m)}, median minutes to first review {fmt(m['median_minutes_to_first_review'])}" for k, m in q["methods"].items()) + " |")
    return "\n".join(out)


def write_doc(r):
    p, s2, lat, dr = r["first_pass"], r["second_pass"], r["latency_ms"], r["drift"]
    acts = ", ".join(f"{k} {v}" for k, v in r["decisions_by_action"].items())
    bands = ", ".join(f"{k} {v}" for k, v in r["decisions_by_band"].items())
    block = f"""<!-- rehearsal:begin (generated by tools/pilot_rehearsal.py from results/pilot_rehearsal.json; do not edit) -->
| | measured |
|---|---|
| fixture | {r["fixture"]} |
| first pass | {p["sent"]} sent, {p["accepted"]} accepted, {p["replayed"]} replayed, {p["failed"]} failed, {p["retried"]} retried |
| decisions by action | {acts} |
| decisions by band | {bands} |
| alerts opened, ledger entries | {r["alerts_open"]} alerts; {r["ledger_entries"]} ledger entries, chain valid: {str(r["ledger_chain_valid"]).lower()} |
| `/api/v2/live` | {r["live_feed"]["events_shown"]} events shown (the latest 50), durable: {str(r["live_feed"]["durable"]).lower()}; without `X-Analyst-Key`: HTTP {r["live_feed"]["without_analyst_key_http"]} |
| `/api/v2/drift` | HTTP {dr["http"]}, status `{dr["status"]}`, window {dr["window_n"]}; reasons: {"; ".join(dr["reasons"]) or "none"} |
| re-send of every event | {s2["sent"]} sent, {s2["replayed"]} replayed, {s2["accepted"]} scored again, {r["second_pass_same_result"]} with the same action and alert; alerts {r["alerts_open"]} → {r["alerts_after_resend"]}, ledger {r["ledger_entries"]} → {r["ledger_entries_after_resend"]} |
| latency (ms) | server p50 {lat["server_p50"]}, p95 {lat["server_p95"]}; round trip p50 {lat["round_trip_p50"]}, p95 {lat["round_trip_p95"]} ({lat["note"]}) |
{budget_rows(r["fixed_budget"])}
<!-- rehearsal:end -->"""
    doc = open(DOC, encoding="utf-8").read()
    new = re.sub(r"<!-- rehearsal:begin.*?<!-- rehearsal:end -->", lambda _: block, doc, flags=re.S)
    if new == doc and block not in doc:
        sys.exit("rehearsal markers not found in docs/PILOT_REHEARSAL.md")
    open(DOC, "w", encoding="utf-8").write(new)


if __name__ == "__main__":
    main()
