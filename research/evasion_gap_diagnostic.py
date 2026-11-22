"""Diagnostic D of docs/decision.md 8al: why timing_jitter and new_payee get through, and whether the proposed inputs
keep their signal. Python only: no model, map or threshold is fitted, and the engine is not run.

    python3 -I research/evasion_gap_diagnostic.py <hi_small_sorted.csv.gz>

PRE-REGISTERED with 8al (written before this script first ran). Data: rows before TEST0 only (the test part is not
read). Streams: clean, timing_jitter and new_payee, built by research/evasion_validation.py's attacks() (same rng 7),
re-sorted as there (t, original row, part). Inputs are recomputed over the whole stream (full history, as the engine)
with the engine's definitions (crates/rusetect/src/engine.rs features()): windows are (t - W, t], current included;
gaps and counts read earlier rows only. Everything reported is on the FIRST half of validation (original row in
[VAL0, mid)); the second half is kept for the decision of step 2.

Reported:
  gain_share            share of total gain of the timing inputs and of the payee-history inputs in l2_xgb and hardening
  inputs                per input: laundering-row median clean / attacked, share of laundering rows whose value changed,
                        and single-input PR-AUC over all first-half rows with a direction fixed here (counts: higher is
                        riskier; gaps: shorter is riskier, NaN = least risky)
  hour_tvd              total variation distance between the laundering and genuine hour histograms, clean / jittered
  forward_before_receipt  laundering payments whose payer received a laundering payment in the 24 h before (clean) and,
                        jittered, now pay before every such receipt: share
  pair_cover            laundering rows with an earlier same-pair payment in the window: 24 h clean, 24 h jittered,
                        72 h jittered; genuine rows at 24 h and 72 h clean (the T-pair exposure; rule D-cover)
  fresh                 payee_fresh rate of genuine and laundering rows by training decile and on first-half validation;
                        under new_payee: laundering rows with payee_fresh, with s_fresh_payees_72h >= 2 (D-cover for N);
                        genuine first-half rows with s_fresh_payees_72h >= 2 (false-positive exposure)
D-cover (8al): an arm whose mechanism touches fewer than 10% of first-half laundering payments under its attack is
dropped. T: share of laundering rows whose 72 h count differs from the 24 h count under jitter. T-pair: pair_cover
72 h jittered. N: laundering rows with payee_fresh under new_payee.
Results: research/out/evasion_gap_diagnostic.json
"""
import json, os, sys
import numpy as np, pandas as pd
from sklearn.metrics import average_precision_score as AP

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from evasion_validation import attacks, minutes, COLS, VAL0, TEST0  # noqa: E402

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
MID = VAL0 + (TEST0 - VAL0) // 2
W24, W72 = 1440, 4320
COVER = 0.10
TIMING = ["hour", "dow", "s_gap_out", "d_gap_in", "s_mins_since_in", "s_out_over_last_in", "s_fanout_24h", "s_cnt_24h",
          "s_usd_24h", "d_fanin_24h", "d_cnt_24h"]
PAYEE = ["new_pair", "d_in_n", "d_gap_in", "d_uniq_src", "d_in_usd_log", "d_cnt_24h", "d_fanin_24h", "reciprocal", "d_out_n"]


def gain_shares():
    import xgboost
    cfg = json.load(open(os.path.join(ROOT, "service", "models", "cascade_config.json")))
    hz = json.load(open(os.path.join(ROOT, "service", "models", "hardening.json")))
    out = {}
    for m, names in (("l2_xgb.json", cfg["features"]), ("hardening_xgb.json", hz["features"])):
        b = xgboost.Booster()
        b.load_model(os.path.join(ROOT, "service", "models", m))
        g = b.get_score(importance_type="total_gain")
        g = {names[int(k[1:])] if k[1:].isdigit() else k: v for k, v in g.items()}
        tot = sum(g.values())
        out[m] = {"timing_inputs": round(sum(v for k, v in g.items() if k in TIMING) / tot, 4),
                  "payee_inputs": round(sum(v for k, v in g.items() if k in PAYEE) / tot, 4)}
    return out


def windowed(g, t, w, val=None):
    """per row: sum of val (count if None) over rows of the same group g in (t - w, t], current included;
    rows are in stream order and t is non-decreasing"""
    n = len(g)
    order = np.lexsort((np.arange(n), g))
    gs, ts = g[order].astype(np.int64), t[order]
    key = gs * 10_000_000 + ts
    lo = np.searchsorted(key, key - w, side="right")
    v = np.ones(n) if val is None else val[order].astype(float)
    c = np.concatenate([[0.0], np.cumsum(v)])
    res = np.empty(n)
    res[order] = c[np.arange(n) + 1] - c[lo]
    return res


def last_before(g_query, g_event, t):
    """per row i: t of the last row j < i whose g_event equals g_query[i] (NaN if none)"""
    n = len(t)
    idx = np.arange(n)
    ev = pd.DataFrame({"g": g_event, "i": idx, "k": 1, "et": t.astype(float)})
    q = pd.DataFrame({"g": g_query, "i": idx, "k": 0, "et": np.nan})
    a = pd.concat([ev, q], ignore_index=True).sort_values(["g", "i", "k"], kind="stable")
    a["et"] = a.groupby("g")["et"].ffill()
    r = a[a.k == 0].set_index("i")["et"]
    return r.reindex(idx).values


def inputs(r, rates):
    """the recomputed inputs for stream r (columns fb fa tb ta amt_paid cur_paid t)"""
    acc, _ = pd.factorize(pd.concat([r.fb + "|" + r.fa, r.tb + "|" + r.ta], ignore_index=True))
    n = len(r)
    s, d = acc[:n].astype(np.int64), acc[n:].astype(np.int64)
    t = r.t.values.astype(np.int64)
    t = t - t.min()
    assert (np.diff(t) >= 0).all()
    usd = r.amt_paid.astype(float).values * r.cur_paid.map(rates).values
    pair = s * (acc.max() + 1) + d
    pair = pd.factorize(pair)[0].astype(np.int64)
    new_pair = (~pd.Series(pair).duplicated()).values.astype(float)
    F = {"hour": r.t.values % 1440 // 60, "new_pair": new_pair}
    ds = pd.Series(d)
    F["d_in_n"] = ds.groupby(ds).cumcount().values.astype(float)
    F["d_uniq_src"] = pd.Series(new_pair).groupby(d).cumsum().values - new_pair
    F["d_in_usd_log"] = np.log1p(pd.Series(usd).groupby(d).cumsum().values - usd)
    F["d_out_n"] = last_count(d, s)
    F["s_gap_out"] = t - pd.Series(t.astype(float)).groupby(s).shift(1).values
    F["d_gap_in"] = t - pd.Series(t.astype(float)).groupby(d).shift(1).values
    F["s_mins_since_in"] = t - last_before(s, d, t)
    for w, tag in ((W24, "24h"), (W72, "72h")):
        F[f"s_cnt_{tag}"] = windowed(s, t, w)
        F[f"d_cnt_{tag}"] = windowed(d, t, w)
        F[f"s_usd_{tag}"] = np.log1p(windowed(s, t, w, usd))
        F[f"pair_prior_{tag}"] = windowed(pair, t, w) - 1
    first = np.full(acc.max() + 1, n, np.int64)
    np.minimum.at(first, s, np.arange(n))
    np.minimum.at(first, d, np.arange(n))
    fresh = (first[d] == np.arange(n)).astype(float)
    F["payee_fresh"] = fresh
    F["s_fresh_payees_72h"] = windowed(s, t, W72, fresh)
    return pd.DataFrame(F)


def last_count(d, s):
    """payments made by the payee before this row (d_out_n)"""
    n = len(d)
    ev = pd.DataFrame({"g": s, "i": np.arange(n), "k": 1})
    q = pd.DataFrame({"g": d, "i": np.arange(n), "k": 0})
    a = pd.concat([ev, q], ignore_index=True).sort_values(["g", "i", "k"], kind="stable")
    a["c"] = a.k.groupby(a.g).cumsum()
    return a[a.k == 0].set_index("i")["c"].reindex(np.arange(n)).values.astype(float)


DIRECTION = {"s_gap_out": -1, "d_gap_in": -1, "s_mins_since_in": -1}


def single_ap(y, x, name):
    x = np.asarray(x, float)
    if DIRECTION.get(name, 1) < 0:
        x = np.where(np.isnan(x), -np.inf, -x)
    x = np.nan_to_num(x, nan=-1e18, neginf=-1e18, posinf=1e18)
    return round(float(AP(y, x)), 4)


def build(raw, val, tail):
    tail = tail.sort_values(["t", "o", "j"], kind="stable")
    return pd.concat([raw.iloc[:VAL0], tail], ignore_index=True)


def main():
    src = sys.argv[1]
    rates = json.load(open(os.path.join(ROOT, "service", "models", "cascade_config.json")))["usd_rates"]
    raw = pd.read_csv(src, nrows=TEST0, dtype=str, keep_default_na=False)[COLS]
    assert len(raw) == TEST0
    raw["t"], raw["o"], raw["j"] = minutes(raw.ts), np.arange(TEST0), 0
    val = raw.iloc[VAL0:]
    lau = val[val.y == "1"]
    A = attacks(lau)
    gen = val[val.y != "1"]
    streams = {"clean": build(raw, val, val), "timing_jitter": build(raw, val, pd.concat([gen, A["timing_jitter"]])),
               "new_payee": build(raw, val, pd.concat([gen, A["new_payee"]]))}
    X, M = {}, {}
    for k, r in streams.items():
        f = inputs(r, rates)
        f["o"], f["y"], f["t"] = r.o.values, r.y.astype(int).values, r.t.values
        X[k] = f
        h = f[(f.o >= VAL0) & (f.o < MID)].set_index("o").sort_index()
        M[k] = h
        print(k, "done", flush=True)
    c = M["clean"]
    lau_o = c.index[c.y == 1]
    R = {"script": "research/evasion_gap_diagnostic.py", "entry": "docs/decision.md 8al",
         "split": "HI-Small validation first half (original rows 3,047,007 to %d); second half and test not used" % (MID - 1),
         "laundering_payments": int(len(lau_o)), "genuine_payments": int((c.y == 0).sum()),
         "gain_share": gain_shares(), "inputs": {}}
    names = ["hour", "s_gap_out", "d_gap_in", "s_mins_since_in", "s_cnt_24h", "s_cnt_72h", "d_cnt_24h", "d_cnt_72h",
             "s_usd_24h", "s_usd_72h", "new_pair", "d_in_n", "d_uniq_src", "d_in_usd_log", "d_out_n", "payee_fresh",
             "s_fresh_payees_72h"]
    for a in ("timing_jitter", "new_payee"):
        R["inputs"][a] = {}
        m = M[a]
        for nme in names:
            cv, av = c.loc[lau_o, nme].values, m.loc[lau_o, nme].values
            same = (cv == av) | (np.isnan(cv) & np.isnan(av))
            R["inputs"][a][nme] = {"lau_median_clean": float(np.nanmedian(cv)) if not np.isnan(cv).all() else None,
                                   "lau_median_attacked": float(np.nanmedian(av)) if not np.isnan(av).all() else None,
                                   "lau_nan_clean": round(float(np.isnan(cv).mean()), 4),
                                   "lau_nan_attacked": round(float(np.isnan(av).mean()), 4),
                                   "lau_changed": round(float(1 - same.mean()), 4),
                                   "pr_auc_clean": single_ap(c.y.values, c[nme].values, nme),
                                   "pr_auc_attacked": single_ap(m.y.values, m[nme].values, nme)}

    def tvd(h):
        p = np.bincount(h[h.y == 1].hour.astype(int), minlength=24) / (h.y == 1).sum()
        q = np.bincount(h[h.y == 0].hour.astype(int), minlength=24) / (h.y == 0).sum()
        return round(float(0.5 * np.abs(p - q).sum()), 4)
    R["hour_tvd"] = {"clean": tvd(c), "timing_jitter": tvd(M["timing_jitter"])}

    # forward before receipt: laundering receipts of the payer in the 24 h before, clean stream
    xc = X["clean"]
    rr = streams["clean"]
    lrows = np.where((xc.y.values == 1) & (xc.o.values >= VAL0))[0]
    payee_of = (rr.tb + "|" + rr.ta).values
    payer_of = (rr.fb + "|" + rr.fa).values
    rec = pd.DataFrame({"acct": payee_of[lrows], "o": rr.o.values[lrows], "t": rr.t.values[lrows]})
    snd = pd.DataFrame({"acct": payer_of[lrows], "o": rr.o.values[lrows], "t": rr.t.values[lrows]})
    snd = snd[(snd.o >= VAL0) & (snd.o < MID)]
    jt = A["timing_jitter"].set_index("o").t
    jt = jt[~jt.index.duplicated()]
    pairs = snd.merge(rec, on="acct", suffixes=("", "_r"))
    pairs = pairs[(pairs.t_r <= pairs.t) & (pairs.t_r > pairs.t - W24) & (pairs.o_r != pairs.o)]
    funded = pairs.groupby("o")
    n_f = int(funded.ngroups)
    before = pairs.assign(after=jt.reindex(pairs.o_r).values >= jt.reindex(pairs.o).values).groupby("o").after.all()
    R["forward_before_receipt"] = {"funded_laundering_payments": n_f,
                                   "share_of_laundering": round(n_f / len(lau_o), 4),
                                   "share_now_paid_before_every_receipt": round(float(before.mean()), 4) if n_f else None}

    tj = M["timing_jitter"]
    R["pair_cover"] = {"lau_24h_clean": round(float((c.loc[lau_o, "pair_prior_24h"] > 0).mean()), 4),
                       "lau_24h_jitter": round(float((tj.loc[lau_o, "pair_prior_24h"] > 0).mean()), 4),
                       "lau_72h_jitter": round(float((tj.loc[lau_o, "pair_prior_72h"] > 0).mean()), 4),
                       "gen_24h_clean": round(float((c[c.y == 0].pair_prior_24h > 0).mean()), 4),
                       "gen_72h_clean": round(float((c[c.y == 0].pair_prior_72h > 0).mean()), 4)}
    t72 = (tj.loc[lau_o, "s_cnt_72h"] != tj.loc[lau_o, "s_cnt_24h"]) | (tj.loc[lau_o, "d_cnt_72h"] != tj.loc[lau_o, "d_cnt_24h"])

    tr = xc.iloc[:VAL0]
    dec = np.minimum(np.arange(VAL0) * 10 // VAL0, 9)
    np_ = M["new_payee"]
    R["fresh"] = {"train_by_decile": {"genuine": [round(float(tr.payee_fresh[(dec == k) & (tr.y.values == 0)].mean()), 5) for k in range(10)],
                                      "laundering": [round(float(tr.payee_fresh[(dec == k) & (tr.y.values == 1)].mean()), 5) for k in range(10)]},
                  "val_first_half_clean": {"genuine": round(float(c[c.y == 0].payee_fresh.mean()), 5),
                                           "laundering": round(float(c.loc[lau_o].payee_fresh.mean()), 5)},
                  "new_payee_lau_fresh": round(float(np_.loc[lau_o].payee_fresh.mean()), 4),
                  "new_payee_lau_s_fresh72_ge2": round(float((np_.loc[lau_o].s_fresh_payees_72h >= 2).mean()), 4),
                  "clean_gen_s_fresh72_ge2": {"count": int((c[c.y == 0].s_fresh_payees_72h >= 2).sum()),
                                              "rate": round(float((c[c.y == 0].s_fresh_payees_72h >= 2).mean()), 6)}}
    g0 = R["fresh"]["train_by_decile"]["genuine"][0]
    gv = R["fresh"]["val_first_half_clean"]["genuine"]
    R["d_cover"] = {"T": round(float(t72.mean()), 4), "T_pair": R["pair_cover"]["lau_72h_jitter"],
                    "N": R["fresh"]["new_payee_lau_fresh"], "min": COVER}
    R["d_cover"]["kept"] = [k for k in ("T", "T_pair", "N") if R["d_cover"][k] >= COVER]
    R["training_confound"] = {"first_decile_over_validation": round(g0 / gv, 2) if gv else None,
                              "warm_up_applies": bool(gv == 0 or g0 >= 5 * gv)}
    json.dump(R, open(os.path.join(ROOT, "research", "out", "evasion_gap_diagnostic.json"), "w"), indent=1)
    print(json.dumps(R, indent=1))


if __name__ == "__main__":
    main()
