"""Evasion suite on HI-Small validation: what the served score loses when launderers change their own payments.

    python3 -I research/evasion_validation.py <hi_small_sorted.csv.gz> <scratch dir>
    (after cargo build --release -p rusetect --example research_dump)

PRE-REGISTERED (this docstring is committed before the script is first run; nothing below changes after results).
Measurement only: no model, map or threshold is fitted or changed, nothing served changes.

Data: the time-sorted HI-Small file. Only rows before TEST0 (4,062,676) are read; the test part is never opened past it.
Rows before VAL0 (3,047,007) build engine state and are never changed. Each attack changes only laundering payments
(y = 1) of the validation part (VAL0 to TEST0); genuine payments and every other row stay as they are. The changed
stream is re-sorted by time (stable: original order, then part order) and every payment is re-scored by the served Rust
feature code and models (research_dump), so features are recomputed, never patched.

Attacks (random draws from numpy default_rng(7), one draw per laundering payment):
  structuring       a laundering payment worth >= 10,000 USD (the US currency transaction report threshold) is split
                    into k = min(10, ceil(usd / 9,500)) equal parts, 30 minutes apart, same payee; below 10,000 it is
                    left alone. Parts of amount/k are generally off any round number.
  timing_jitter     each laundering payment is sent later by a uniform whole number of minutes in [0, 2880] (two days)
  amount_jitter     each laundering amount (paid and received) times a uniform factor in [0.90, 1.10] (+-10%)
  new_payee         each laundering payment goes to a fresh account at the same bank (a new mule, never seen before);
                    the mule's onward payments are not simulated (the original chain keeps its accounts)
  format_<f>        every laundering payment re-sent in format f, for each of the 7 formats; the worst is reported
                    (the existing format attack of research/full_hi_eval.py, here re-simulated through the engine)
  currency_<c>      every laundering payment paid and received in currency c (US Dollar, Euro), amounts converted at
                    the engine's own USD rates (service/models/cascade_config.json)
Served score, exactly as crates/rusetect/src/engine.rs finish_risk: cascade risk, raised to the 24 h noisy-OR of the
cascade risks of the same payer-payee pair (research/structuring_defence.py pair_noisy_or; the engine state of the
24 h before validation is included), then max(iso_served, iso_free) from service/models/hardening.json.

REPORTING RULE (fixed):
  Per attack and for the clean stream, on HI-Small validation:
    pr_auc            average precision over the original validation payments, a payment's score being the maximum
                      over its parts (genuine payments have one part)
    block recall      share of laundering payments with any part at or above the served block threshold, and the
                      number of block alerts that raises
    block recall at the served alert count: k = block alerts of the clean stream at the served threshold; the k
                      highest scored validation rows of the attacked stream are alerted; share of laundering payments
                      with any part among them (same analyst workload as clean)
  KNOWN WEAKNESS: an attack is flagged when its PR-AUC is more than 0.05 below the clean PR-AUC (absolute).
  For the format and currency families each variant is reported and flagged on its own; the family's worst is named.
Results: research/out/evasion_validation.json (research/out/evasion_suite.json is the earlier demo-window suite).
"""
import json, os, subprocess, sys
from concurrent.futures import ThreadPoolExecutor
import numpy as np, pandas as pd
from sklearn.metrics import average_precision_score as AP

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from structuring_defence import pair_noisy_or, hardened, H, L1T, CFG  # noqa: E402

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BIN = os.path.join(ROOT, "target", "release", "examples", "research_dump")
VAL0, TEST0 = 3_047_007, 4_062_676
MARGIN = 0.05
CTR, PART, KMAX = 10_000.0, 9_500.0, 10
COLS = ["ts", "fb", "fa", "tb", "ta", "amt_recv", "cur_recv", "amt_paid", "cur_paid", "fmt", "y"]
RATES = CFG["usd_rates"]


def minutes(ts):
    return (pd.to_datetime(ts, format="%Y/%m/%d %H:%M").astype("int64") // 60_000_000_000).values


def amt(x):
    return [f"{v:.8f}" for v in x]


def attacks(lau):
    """name -> frame of replacement rows (columns COLS plus o = original row, j = part, t = minute)"""
    rng = np.random.default_rng(7)
    n = len(lau)
    ap, ar = lau.amt_paid.astype(float).values, lau.amt_recv.astype(float).values
    usd = ap * lau.cur_paid.map(RATES).values
    A = {}
    k = np.where(usd >= CTR, np.minimum(KMAX, np.ceil(usd / PART)), 1).astype(int)
    s = lau.loc[lau.index.repeat(k)].copy()
    j = np.concatenate([np.arange(m) for m in k])
    kk = np.repeat(k, k)
    s["amt_paid"], s["amt_recv"] = amt(np.repeat(ap, k) / kk), amt(np.repeat(ar, k) / kk)
    s["j"], s["t"] = j, s.t.values + 30 * j
    A["structuring"] = s
    A["timing_jitter"] = lau.assign(t=lau.t.values + rng.integers(0, 2881, n))
    f = rng.uniform(0.90, 1.10, n)
    A["amount_jitter"] = lau.assign(amt_paid=amt(ap * f), amt_recv=amt(ar * f))
    A["new_payee"] = lau.assign(ta=[f"{a}EV{i}" for i, a in enumerate(lau.ta.values)])
    for fm in CFG["fmt_codes"]:
        A[f"format_{fm}"] = lau.assign(fmt=fm)
    for c in ("US Dollar", "Euro"):
        A[f"currency_{c}"] = lau.assign(cur_paid=c, cur_recv=c, amt_paid=amt(usd / RATES[c]),
                                        amt_recv=amt(ar * lau.cur_recv.map(RATES).values / RATES[c]))
    return A


def run(name, tail, raw, prefix, first, work):
    """score raw[:VAL0] + tail (the changed validation part) with the engine from row `first`; returns validation rows"""
    tail = tail.sort_values(["t", "o", "j"], kind="stable")
    tail["ts"] = pd.to_datetime(tail.t * 60_000_000_000).dt.strftime("%Y/%m/%d %H:%M")
    tp, out = os.path.join(work, f"{name}.tail.csv"), os.path.join(work, f"{name}.dump.csv")
    tail[COLS].to_csv(tp, header=False, index=False)
    with open(prefix, "rb") as a, open(tp, "rb") as b:
        p = subprocess.Popen([BIN, "/dev/stdin", out, "1.0", str(first)], stdin=subprocess.PIPE)
        for f in (a, b):
            while chunk := f.read(1 << 22):
                p.stdin.write(chunk)
        p.stdin.close()
        assert p.wait() == 0, name
    os.remove(tp)
    d = pd.read_csv(out, usecols=["row", "y", "l1", "l2x", "l2l"] + H["features"])
    os.remove(out)
    r = pd.concat([raw.iloc[first:VAL0], tail], ignore_index=True)
    assert len(d) == len(r) and (d.row.values == np.arange(first, first + len(r))).all()
    key = (r.fb + "|" + r.fa + ">" + r.tb + "|" + r.ta).values  # new_payee rows carry their new payee
    s = np.where(d.l1.values < L1T, d.l1.values, (d.l2x.values + d.l2l.values) / 2)
    risk = hardened(d, np.maximum(s, pair_noisy_or(r.t.values, key, s)))
    ev = r.o.values >= VAL0
    assert (d.y.values[ev].astype(int) == r.y.values[ev].astype(int)).all()
    return pd.DataFrame({"o": r.o.values[ev], "y": r.y.values[ev].astype(int), "risk": risk[ev]})


def metrics(v, clean_k):
    thr = H["thresholds"]["block"]
    g = v.groupby("o").agg(y=("y", "max"), risk=("risk", "max"))
    lau = g.index[g.y == 1]
    top = v.o.values[np.argsort(-v.risk.values, kind="stable")[:clean_k]] if clean_k else np.array([], int)
    return {"pr_auc": round(float(AP(g.y, g.risk)), 4),
            "block_recall": round(float((g.risk[lau] >= thr).mean()), 4),
            "block_alerts": int((v.risk.values >= thr).sum()),
            "block_recall_at_clean_alert_count": round(float(np.isin(lau, top).mean()), 4),
            "rows": int(len(v)), "laundering_payments": int(len(lau))}


def main():
    src, work = sys.argv[1:3]
    os.makedirs(work, exist_ok=True)
    raw = pd.read_csv(src, nrows=TEST0, dtype=str, keep_default_na=False)[COLS]  # stops before the test part
    assert len(raw) == TEST0
    raw["t"], raw["o"], raw["j"] = minutes(raw.ts), np.arange(TEST0), 0
    prefix = os.path.join(work, "prefix.csv")
    if not os.path.exists(prefix):
        raw.iloc[:VAL0][COLS].to_csv(prefix, index=False)
    first = int(np.searchsorted(raw.t.values, raw.t.values[VAL0] - 1440, side="right"))  # the 24 h pair window
    val = raw.iloc[VAL0:]
    lau = val[val.y == "1"]
    A = {"clean": None, **attacks(lau)}
    gen = val[val.y != "1"]
    with ThreadPoolExecutor(3) as ex:
        jobs = {n: ex.submit(run, n, val if a is None else pd.concat([gen, a]), raw, prefix, first, work) for n, a in A.items()}
        res = {n: j.result() for n, j in jobs.items()}
    clean_k = int((res["clean"].risk.values >= H["thresholds"]["block"]).sum())
    R = {"script": "research/evasion_validation.py", "split": "HI-Small validation (rows 3,047,007 to 4,062,675); test not read",
         "margin": MARGIN, "block_threshold": H["thresholds"]["block"], "clean_block_alerts": clean_k, "attacks": {}}
    for n, v in res.items():
        R["attacks"][n] = metrics(v, clean_k)
    c = R["attacks"]["clean"]["pr_auc"]
    for n, m in R["attacks"].items():
        m["pr_auc_drop"] = round(c - m["pr_auc"], 4)
        m["known_weakness"] = n != "clean" and m["pr_auc_drop"] > MARGIN
    fam = lambda p: min((n for n in R["attacks"] if n.startswith(p)), key=lambda n: R["attacks"][n]["pr_auc"])  # noqa: E731
    R["worst"] = {"format": fam("format_"), "currency": fam("currency_")}
    R["known_weaknesses"] = [n for n, m in R["attacks"].items() if m["known_weakness"]]
    json.dump(R, open(os.path.join(ROOT, "research", "out", "evasion_validation.json"), "w"), indent=1)
    print(json.dumps(R, indent=1))


if __name__ == "__main__":
    main()
