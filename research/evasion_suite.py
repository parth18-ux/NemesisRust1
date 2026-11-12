"""Evasion suite: the attacks a launderer controls, each re-simulated through the served Rust feature code.

    python3 -I research/evasion_suite.py            (after cargo build --release --example research_dump)

Every laundering payment in the evaluation window of the HI-Small demo slice (rows 100,000+) is changed as below; the
rest of the stream is untouched. Each stream is scored by research_dump (cascade and features), then in Python by the
served hardened risk with pair aggregation (service/models/hardening.json, research/structuring_defence.py).
  cheque              payment format switched to Cheque
  amount_minus10      amount cut by 10% (moves round amounts off the round number)
  amount_jitter       amount times a random factor in [0.85, 1.15]
  delay_6h            payment sent six hours later
  split4_spread       four quarter-size parts, 30 minutes apart, same payee
  split4_new_payees   four quarter-size parts to four new accounts (mules); defeats pair aggregation by design
  cheque_split4       Cheque format and four parts, 30 minutes apart
Recall counts a laundering payment as caught if any of its parts reaches the band. Evaluation only; nothing is fitted.
hardened_with_payer (exploratory, added after the first run): the pair window widened to all of a payer's payments.
Results: research/out/evasion_suite.json
"""
import json, os, random, sys
from datetime import datetime, timedelta
import numpy as np, pandas as pd

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from make_streams import read, write, fmt_amt, OUT as ST, EVAL_FROM  # noqa: E402
from structuring_defence import pair_noisy_or, hardened, H, BANDS, L1T  # noqa: E402

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


def mutate(rows, fn):
    """fn(row, i) -> list of (row, minute offset); output re-sorted by time, stable"""
    out = []
    for i, r in enumerate(rows):
        if i >= EVAL_FROM and r["y"] == "1":
            t = datetime.strptime(r["ts"], "%Y/%m/%d %H:%M")
            for j, (s, dm) in enumerate(fn(dict(r), i)):
                s["ts"] = (t + timedelta(minutes=dm)).strftime("%Y/%m/%d %H:%M")
                out.append((s["ts"], i, j, s))
        else:
            out.append((r["ts"], i, 0, r))
    out.sort(key=lambda z: (z[0], z[1], z[2]))
    return [z[3] for z in out], [z[1] for z in out]


def scale(r, f):
    r["amt_paid"] = fmt_amt(float(r["amt_paid"]) * f); r["amt_recv"] = fmt_amt(float(r["amt_recv"]) * f)
    return r


def parts(r, k, gap, new_payee=False, fmt=None):
    out = []
    for j in range(k):
        s = scale(dict(r), 1 / k)
        if new_payee:
            s["ta"] = f"{r['ta']}M{j}"
        if fmt:
            s["fmt"] = fmt
        out.append((s, gap * j))
    return out


def main():
    header, rows = read()
    rnd = random.Random(5)
    attacks = {
        "cheque": lambda r, i: [(dict(r, fmt="Cheque"), 0)],
        "amount_minus10": lambda r, i: [(scale(r, 0.9), 0)],
        "amount_jitter": lambda r, i: [(scale(r, rnd.uniform(0.85, 1.15)), 0)],
        "delay_6h": lambda r, i: [(r, 360)],
        "split4_spread": lambda r, i: parts(r, 4, 30),
        "split4_new_payees": lambda r, i: parts(r, 4, 30, new_payee=True),
        "cheque_split4": lambda r, i: parts(r, 4, 30, fmt="Cheque"),
    }
    R = {"note": "block and verify recall, any part caught, HI-Small demo window", "attacks": {}}
    for name in ["base"] + list(attacks):
        if name != "base" and not os.path.exists(os.path.join(ST, f"ev_{name}_dump.csv")):
            out, org = mutate(rows, attacks[name])
            write(f"ev_{name}", header, out, org)
        stem = "base" if name == "base" else f"ev_{name}"
        raw = pd.read_csv(os.path.join(ST, f"{stem}.csv.gz"), usecols=["ts", "fb", "fa", "tb", "ta"], dtype=str)
        d = pd.read_csv(os.path.join(ST, f"{stem}_dump.csv"), usecols=["y", "l1", "l2x", "l2l"] + H["features"])
        o = np.loadtxt(os.path.join(ST, f"{stem}.origin"), dtype=np.int64)
        t = (pd.to_datetime(raw.ts, format="%Y/%m/%d %H:%M").astype("int64") // 60_000_000_000).values
        key = (raw.fb + "|" + raw.fa + ">" + raw.tb + "|" + raw.ta).values
        payer = (raw.fb + "|" + raw.fa).values
        s = np.where(d.l1.values < L1T, d.l1.values, (d.l2x.values + d.l2l.values) / 2)
        scores = {"cascade": (s, BANDS), "hardened": (hardened(d, s), H["thresholds"]),
                  "hardened_with_pair": (hardened(d, np.maximum(s, pair_noisy_or(t, key, s))), H["thresholds"]),
                  # exploratory: the same 24 h noisy-OR over all of a payer's payments (catches parts sent to new mules)
                  "hardened_with_payer": (hardened(d, np.maximum(s, pair_noisy_or(t, payer, s))), H["thresholds"])}
        ev = o >= EVAL_FROM; y = d.y.values
        r = {}
        for lab, (sc, thr) in scores.items():
            row = {}
            for b in ("verify", "block"):
                hit = sc >= thr[b]
                row[b] = round(float(pd.Series(hit[ev & (y == 1)]).groupby(o[ev & (y == 1)]).any().mean()), 4)
            row["genuine_blocked"] = int((sc[ev & (y == 0)] >= thr["block"]).sum())
            r[lab] = row
        R["attacks"][name] = r
        print(name, {k: (v["verify"], v["block"]) for k, v in r.items()}, flush=True)
    json.dump(R, open(os.path.join(ROOT, "research", "out", "evasion_suite.json"), "w"), indent=1)


if __name__ == "__main__":
    main()
