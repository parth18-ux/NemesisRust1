"""Step 2 of docs/decision.md 8al (with the amendment 8am): fixes for the timing_jitter and new_payee evasion gaps.

    python3 -I research/evasion_fix.py <hi_small_sorted.csv.gz> <scratch dir>
    (after cargo build --release -p rusetect --example research_dump)

Committed before it is first run; the only code that reads the second half of HI-Small validation. The protocol
(arms, inputs, budget rule, margins, cohort minimum, bootstrap, stop rule) is 8al and 8am in docs/decision.md; this
docstring only records how the script reads it where the text leaves a mechanical choice open.

Streams. raw rows before TEST0 only (test is not read). Rows before VAL0 are kept; the 8ae attacks of
research/evasion_validation.attacks (default_rng(7)) change validation laundering rows, the stream is re-sorted
(t, original row, part) and re-scored by research_dump, which writes the served features, the served L1 / L2 scores
and the research-only inputs of the arms (crates/rusetect/examples/research_dump.rs EXTRA). One more stream, secondary
and not judged: timing_feasible (8am), the timing_jitter draws with each laundering payment whose payer received a
laundering payment in the 24 h before it (clean stream, as diagnostic D's forward_before_receipt) moved no earlier
than the latest jittered time of those receipts.

Arms (8al step 2, 8am).
  S       served: research/evasion_validation.run's score (cascade raised to the 24 h pair noisy-OR, then
          max(iso_served, iso_free) of service/models/hardening.json); block threshold hardening.json's.
  T-pair  S with the pair noisy-OR window at 4,320 minutes; no retrain; S's maps and thresholds.
  C, T, N, T2  the served recipe retrained: one XGBoost on the inputs (C: the 31 served inputs; T: + s_cnt_72h,
          s_usd_72h, d_cnt_72h, d_fanin_72h; N: + payee_fresh, s_fresh_payees_72h; T2: + s_in_usd_72h,
          s_out_over_in_72h), the params of research/full_hi_eval.py's 31-input control, all laundering rows plus 10%
          of genuine rows (as service/models was trained, docs/RETRAINING.md), fitting rows [VAL0 // 10 ... 0.9 x VAL0)
          (the first training decile is dropped: 8al-D's training_confound applies the warm-up), early stopping
          (aucpr, 100 rounds, at most 3,000) on all rows [0.9 x VAL0, VAL0). Three seeds (0, 1, 2; the seed draws the
          genuine sample, numpy default_rng(seed), and is xgboost's seed), the three probabilities averaged. The seeds
          and samples are the same for every arm (paired). The averaged model takes the served cascade's place in the
          served score: raised to the 24 h pair noisy-OR, mapped by an isotonic map fitted on the clean first half of
          validation, then max with the served iso_free(hardening) as in finish_risk. No L1 gate.
Half 1 (original rows [VAL0, mid)) fits the isotonic maps and each retrained arm's block threshold (the score
quantile that gives the arm S's half-1 block rate r1; secondary use only), and r1 itself. Freeze (enforced in code
since the correction after the run, see below): the script first reads raw rows [0, mid) only, builds and scores the
clean stream up to mid, fits the models, maps, r1, B and thresholds, writes them (maps included) to
research/out/evasion_fix_half1.json, then checks that file (load_checkpoint) and only then reads rows [mid, TEST0),
builds the attack streams and scores anything of half 2; the half-2 stage uses the maps and B from the file.
Correction after the one run (results unchanged, not re-run): the run at commit 0cd81c6 dumped and scored the whole
clean stream, half 2 included, before it wrote the checkpoint, and built the attack streams (raw rows only) before it.
Only half-1 rows (a mask on the original row) entered r1, B, the maps and the thresholds; features, the pair
noisy-OR and the models (trained on rows before VAL0) are causal, so no half-2 row could change a half-1 value.
Before the checkpoint, half-2 rows were *written* only to temporary files in the scratch directory (the clean
stream's input CSV and its research_dump output, half 2 included; both deleted by dump() once loaded); half-2 scores
were *kept* in memory only (used after the checkpoint for the clean stream's half-2 numbers); nothing of half 2 was
*printed* (the run log before the checkpoint holds only training rounds) and nothing of half 2 was *inspected*. The
ordering was changed afterwards so that a future run cannot score half 2 before the freeze.
Self-test of the guard: python3 -I research/evasion_fix.py --self-test
Half 2 (original rows [mid, TEST0)): B = round(r1 x half-2 payments); per stream each arm alerts its B highest half-2
rows (stable sort on the stream order, so ties go to the earlier row). Cases, recall, precision, the fresh-payee
cohort G (genuine clean half-2 payments with payee_fresh = 1), FPR_G, the paired block bootstrap ((payer, 24 h block
from VAL0's minute) of the original payment; 2,000 resamples; numpy default_rng(20261122); the same block draws for
every arm; alert sets fixed) and R1 to R4 are as 8al. R3 for a timing arm checks every 8ae attack but timing_jitter
(new_payee included); for N every attack but new_payee. R2 and R4 compare with C for every arm (T-pair included; 8al
replaces C by S only in R1's first part for T-pair).
Results: research/out/evasion_fix.json (half-2 numbers and the decision) and research/out/evasion_fix_half1.json.
"""
import json, os, subprocess, sys, time
from concurrent.futures import ThreadPoolExecutor
import numpy as np, pandas as pd, xgboost as xgb
from sklearn.isotonic import IsotonicRegression
from sklearn.metrics import average_precision_score as AP

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from evasion_validation import attacks, minutes, COLS, VAL0, TEST0, BIN  # noqa: E402
from structuring_defence import pair_noisy_or, H, L1T, CFG  # noqa: E402

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
OUT = os.path.join(ROOT, "research", "out")
MID = VAL0 + (TEST0 - VAL0) // 2
F31 = CFG["features"]
NEWIN = {"C": [], "T": ["s_cnt_72h", "s_usd_72h", "d_cnt_72h", "d_fanin_72h"], "N": ["payee_fresh", "s_fresh_payees_72h"],
         "T2": ["s_in_usd_72h", "s_out_over_in_72h"]}
EXTRA_USED = sorted({c for v in NEWIN.values() for c in v})
PARAMS = {"objective": "binary:logistic", "max_depth": 6, "eta": 0.05, "subsample": 0.8, "colsample_bytree": 0.8,
          "min_child_weight": 1, "tree_method": "hist", "eval_metric": "aucpr", "nthread": 4}
SEEDS, NEG, ES_ROUNDS, MAX_ROUNDS = (0, 1, 2), 0.10, 100, 3000
ARMS = ["S", "T-pair", "C", "T", "N", "T2"]
CANDS = ["T", "T-pair", "N", "T2"]
TARGET = {"T": "timing_jitter", "T-pair": "timing_jitter", "T2": "timing_jitter", "N": "new_payee"}
BOOT, BOOT_SEED = 2000, 20261122
R1_MIN, R2_MIN, R2_LO, R2_S, R3_MARGIN = 0.05, -0.01, -0.02, -0.01, 0.02
R4_CAP, R4_INC, R4_INC_HI, G_MIN = 0.02, 0.005, 0.01, 300


def feasible(lau, jit):
    """timing_feasible (8am, secondary): a forward of a laundering receipt (payer received it in the 24 h before, clean
    stream) is moved no earlier than the latest jittered time of those receipts"""
    payer, payee = (lau.fb + "|" + lau.fa).values, (lau.tb + "|" + lau.ta).values
    a = pd.DataFrame({"acct": payer, "o": lau.o.values, "t": lau.t.values})
    r = pd.DataFrame({"acct": payee, "o_r": lau.o.values, "t_r": lau.t.values})
    p = a.merge(r, on="acct")
    p = p[(p.t_r <= p.t) & (p.t_r > p.t - 1440) & (p.o_r != p.o)]
    jt = pd.Series(jit.t.values, index=jit.o.values)
    need = p.assign(tr=jt.reindex(p.o_r).values).groupby("o").tr.max()
    t = jt.copy()
    t.loc[need.index] = np.maximum(t.loc[need.index].values, need.values)
    return jit.assign(t=t.reindex(jit.o.values).values), int(len(need))


def dump(name, tail, prefix, first, work):
    """research_dump of raw[:VAL0] + tail, rows from `first`; returns the dump frame and the scored stream rows"""
    tail = tail.sort_values(["t", "o", "j"], kind="stable").copy()
    ts = pd.to_datetime(tail.t, unit="m").dt.strftime("%Y/%m/%d %H:%M")
    if name == "clean":
        assert (ts.values == tail.ts.values).all(), "minute round trip"
    tail["ts"] = ts
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
    cols = ["row", "y", "l1", "l2x", "l2l"] + sorted(set(F31) | set(H["features"])) + EXTRA_USED
    d = pd.read_csv(out, usecols=cols, dtype={c: np.float32 for c in cols if c not in ("row", "y")})
    os.remove(out)
    return d, tail


class Models:
    def __init__(self, d):
        """d: clean dump from row 0 (training rows are the same in every stream)"""
        y = d.y.values[:VAL0].astype(np.int8)
        es0, w0 = int(VAL0 * 0.9), -(-VAL0 // 10)  # first row of decile 1: i * 10 // VAL0 >= 1
        self.m, self.rounds = {}, {}
        for arm, extra in NEWIN.items():
            cols = F31 + extra
            X = d[cols].values[:VAL0]
            dv = xgb.DMatrix(X[es0:], label=y[es0:], missing=np.nan, feature_names=cols)
            bs = []
            for sd in SEEDS:
                rng = np.random.default_rng(sd)
                idx = np.arange(w0, es0)
                keep = idx[(y[w0:es0] == 1) | (rng.random(es0 - w0) < NEG)]
                b = xgb.train(dict(PARAMS, seed=sd), xgb.DMatrix(X[keep], label=y[keep], missing=np.nan, feature_names=cols),
                              MAX_ROUNDS, evals=[(dv, "es")], early_stopping_rounds=ES_ROUNDS, verbose_eval=False)
                bs.append(b[: b.best_iteration + 1])
                print(arm, "seed", sd, "rounds", b.best_iteration + 1, flush=True)
            self.m[arm] = (bs, cols)
            self.rounds[arm] = [int(b.num_boosted_rounds()) for b in bs]

    def p(self, arm, d):
        bs, cols = self.m[arm]
        dm = xgb.DMatrix(d[cols].values, missing=np.nan, feature_names=cols)
        return np.mean([b.predict(dm) for b in bs], axis=0).astype(np.float64)


def raw_scores(d, r, models):
    """per stream row: the pre-map scores of every arm, the hardening term, and stream identifiers"""
    key = (r.fb + "|" + r.fa + ">" + r.tb + "|" + r.ta).values
    t = r.t.values
    s = np.where(d.l1.values < L1T, d.l1.values, (d.l2x.values + d.l2l.values) / 2).astype(np.float64)
    bf = xgb.Booster(); bf.load_model(os.path.join(ROOT, "service", "models", H["model"]))
    free = np.interp(bf.predict(xgb.DMatrix(d[H["features"]].values, missing=np.nan, feature_names=H["features"])),
                     H["iso_free"]["x"], H["iso_free"]["y"])
    z = {"free": free,
         "S": np.maximum(s, pair_noisy_or(t, key, s, 1440)),
         "T-pair": np.maximum(s, pair_noisy_or(t, key, s, 4320))}
    for arm in NEWIN:
        p = models.p(arm, d)
        z[arm] = np.maximum(p, pair_noisy_or(t, key, p, 1440))
    z["payee_fresh"] = d.payee_fresh.values.astype(np.float64)
    return z


def finish(z, iso):
    """final risk per arm: the served maps for S and T-pair, the half-1 isotonic map for the retrained arms"""
    out = {}
    for arm in ARMS:
        m = np.interp(z[arm], H["iso_served"]["x"], H["iso_served"]["y"]) if arm in ("S", "T-pair") else iso[arm].predict(z[arm])
        out[arm] = np.maximum(m, z["free"])
    return out


CHECKPOINT = "evasion_fix_half1.json"


def load_checkpoint(path):
    """the half-1 freeze; half 2 may be read only after this returns"""
    if not os.path.exists(path):
        raise RuntimeError(f"half-1 checkpoint {path} missing: half 2 stays closed")
    h = json.load(open(path))
    need = ("r1", "B", "block_thresholds_half1", "iso_maps", "half1_rows")
    if any(k not in h for k in need) or h["half1_rows"][1] >= MID or set(h["iso_maps"]) != set(NEWIN):
        raise RuntimeError(f"half-1 checkpoint {path} incomplete or not from half 1: half 2 stays closed")
    return h


def read_raw(src, n):
    raw = pd.read_csv(src, nrows=n, dtype=str, keep_default_na=False)[COLS]
    assert len(raw) == n
    raw["t"], raw["o"], raw["j"] = minutes(raw.ts), np.arange(n), 0
    return raw


class Interp:
    """an isotonic map read back from the checkpoint (sklearn's clip interpolation)"""
    def __init__(self, m):
        self.x, self.y = np.asarray(m["x"]), np.asarray(m["y"])

    def predict(self, v):
        return np.interp(v, self.x, self.y)


def self_test():
    import tempfile
    with tempfile.TemporaryDirectory() as d:
        p = os.path.join(d, CHECKPOINT)
        for bad in (None, {"r1": 0.1}, {"r1": 0.1, "B": 1, "block_thresholds_half1": {}, "half1_rows": [VAL0, MID],
                                         "iso_maps": {a: {} for a in NEWIN}}):
            if bad is not None:
                json.dump(bad, open(p, "w"))
            try:
                load_checkpoint(p)
                raise AssertionError(f"checkpoint {bad} opened half 2")
            except RuntimeError:
                pass
        ok = {"r1": 0.1, "B": 1, "block_thresholds_half1": {}, "half1_rows": [VAL0, MID - 1], "iso_maps": {a: {} for a in NEWIN}}
        json.dump(ok, open(p, "w"))
        assert load_checkpoint(p)["B"] == 1
    m = IsotonicRegression(out_of_bounds="clip", y_min=0, y_max=1).fit([0.1, 0.2, 0.3, 0.4], [0, 1, 0, 1])
    v = np.array([-1, 0.15, 0.25, 0.35, 2.0])
    assert np.allclose(Interp({"x": m.X_thresholds_, "y": m.y_thresholds_}).predict(v), m.predict(v))
    print("self-test ok")


def main():
    t0 = time.time()
    src, work = sys.argv[1:3]
    os.makedirs(work, exist_ok=True)
    ck = os.path.join(OUT, CHECKPOINT)
    if os.path.exists(ck):
        os.remove(ck)  # a stale checkpoint must not open half 2
    # ---------------------------------------------------------------- training and half 1: rows [0, MID) only
    raw = read_raw(src, MID)
    prefix = os.path.join(work, "prefix.csv")
    if not os.path.exists(prefix):
        raw.iloc[:VAL0][COLS].to_csv(prefix, index=False)
    first = int(np.searchsorted(raw.t.values, raw.t.values[VAL0] - 4320, side="right"))  # the 72 h pair window
    dc, rc = dump("clean", raw.iloc[VAL0:MID], prefix, 0, work)
    assert rc.o.max() < MID
    models = Models(dc)
    dc, rc = dc.iloc[first:].reset_index(drop=True), pd.concat([raw.iloc[first:VAL0], rc], ignore_index=True)
    assert (dc.row.values == np.arange(first, MID)).all()
    zc = raw_scores(dc, rc, models)
    h1 = rc.o.values >= VAL0
    y1 = rc.y.values[h1].astype(int)
    iso = {a: IsotonicRegression(out_of_bounds="clip", y_min=0, y_max=1).fit(zc[a][h1], y1) for a in NEWIN}
    fin1 = {a: v[h1] for a, v in finish(zc, iso).items()}
    r1 = float((fin1["S"] >= H["thresholds"]["block"]).mean())
    n2 = TEST0 - MID
    B = int(round(r1 * n2))
    thr = {"S": H["thresholds"]["block"], "T-pair": H["thresholds"]["block"]}
    for a in NEWIN:
        thr[a] = float(np.quantile(fin1[a], 1 - r1))
    H1 = {"script": "research/evasion_fix.py", "entry": "docs/decision.md 8al, 8am",
          "written": "before any half-2 row is read (raw rows [0, mid) only)",
          "half1_rows": [VAL0, MID - 1], "half1_payments": int(h1.sum()), "r1": r1, "half2_payments": n2, "B": B,
          "block_thresholds_half1": thr, "rounds": models.rounds, "fitting_rows": [-(-VAL0 // 10), int(VAL0 * 0.9) - 1],
          "early_stopping_rows": [int(VAL0 * 0.9), VAL0 - 1],
          "half1_block_alerts": {a: int((fin1[a] >= thr[a]).sum()) for a in ARMS},
          "iso_maps": {a: {"x": [float(v) for v in m.X_thresholds_], "y": [float(v) for v in m.y_thresholds_]} for a, m in iso.items()},
          "seconds": round(time.time() - t0)}
    json.dump(H1, open(ck, "w"), indent=1)
    print(json.dumps({k: v for k, v in H1.items() if k != "iso_maps"}), flush=True)
    del dc, rc, zc, fin1, iso, raw

    # ---------------------------------------------------------------- half 2: opened only through the checkpoint
    H1 = load_checkpoint(ck)
    r1, B, thr = H1["r1"], H1["B"], H1["block_thresholds_half1"]
    iso = {a: Interp(m) for a, m in H1["iso_maps"].items()}
    raw = read_raw(src, TEST0)
    val = raw.iloc[VAL0:]
    lau = val[val.y == "1"]
    gen = val[val.y != "1"]
    A = attacks(lau)
    A["timing_feasible"], n_moved = feasible(lau, A["timing_jitter"])

    # ---------------------------------------------------------------- half 2
    def keep(r, fin, z):
        ev = r.o.values >= MID
        f = pd.DataFrame({a: fin[a][ev] for a in ARMS})
        f["o"], f["y"] = r.o.values[ev], r.y.values[ev].astype(int)
        f["fresh"] = z["payee_fresh"][ev]
        return f

    def one(name, a):
        d, r = dump(name, val if a is None else pd.concat([gen, a]), prefix, first, work)
        r = pd.concat([raw.iloc[first:VAL0], r], ignore_index=True)
        assert len(d) == len(r)
        z = raw_scores(d, r, models)
        print(name, "scored", round(time.time() - t0), flush=True)
        return keep(r, finish(z, iso), z)

    V = {}
    with ThreadPoolExecutor(2) as ex:
        jobs = {n: ex.submit(one, n, a) for n, a in {"clean": None, **A}.items()}
        for n, j in jobs.items():
            V[n] = j.result()

    # cases, blocks, cohort
    h2 = raw.iloc[MID:TEST0]
    o2 = h2.o.values
    payer = pd.factorize((h2.fb + "|" + h2.fa).values)[0].astype(np.int64)
    day = (h2.t.values - raw.t.values[VAL0]) // 1440
    block = pd.factorize(payer * 100_000 + day)[0]
    nb = int(block.max() + 1)
    blk = pd.Series(block, index=o2)
    yo = pd.Series(h2.y.astype(int).values, index=o2)
    cases = o2[yo.values == 1]
    c = V["clean"]
    assert (np.sort(c.o.values) == o2).all() and (c.y.values == yo.reindex(c.o.values).values).all()
    G = c.o.values[(c.y.values == 0) & (c.fresh.values == 1)]

    def alerts(v, arm):
        return v.o.values[np.argsort(-v[arm].values, kind="stable")[:B]]

    AL = {(n, a): alerts(v, a) for n, v in V.items() for a in ARMS}
    hit = {k: np.isin(cases, al) for k, al in AL.items()}

    # only blocks holding a case, a member of G or a clean alert enter any statistic; every block is still drawn
    rel = np.unique(np.concatenate([blk.reindex(x).values for x in [cases, G] + [AL[("clean", a)] for a in ARMS]]))
    rng = np.random.default_rng(BOOT_SEED)
    draws = np.stack([np.bincount(rng.integers(0, nb, nb), minlength=nb)[rel] for _ in range(BOOT)]).astype(np.float32)

    def per_block(o, val):
        return np.bincount(blk.reindex(o).values, weights=val, minlength=nb)[rel].astype(np.float32)

    cb = per_block(cases, np.ones(len(cases)))
    gb = per_block(G, np.ones(len(G))) if len(G) else np.zeros(nb, np.float32)

    def boot_recall(n, a):
        return (draws @ per_block(cases, hit[(n, a)].astype(float))) / (draws @ cb)

    def boot_prec(a):
        al = AL[("clean", a)]
        return (draws @ per_block(al, yo.reindex(al).values.astype(float))) / (draws @ per_block(al, np.ones(B)))

    def boot_fpr(a):
        return (draws @ per_block(G, np.isin(G, AL[("clean", a)]).astype(float))) / (draws @ gb) if len(G) else None

    def ci(x):
        return [round(float(np.nanpercentile(x, 2.5)), 4), round(float(np.nanpercentile(x, 97.5)), 4)]

    rec = {n: {a: float(hit[(n, a)].mean()) for a in ARMS} for n in V}
    prec = {a: float(yo.reindex(AL[("clean", a)]).mean()) for a in ARMS}
    fpr = {a: (float(np.isin(G, AL[("clean", a)]).mean()) if len(G) else None) for a in ARMS}
    secondary = {}
    for n, v in V.items():
        g = v.groupby("o").agg(**{"y": ("y", "max")}, **{a: (a, "max") for a in ARMS})
        thr_a = thr
        secondary[n] = {a: {"pr_auc": round(float(AP(g.y, g[a])), 4),
                            "block_alerts_half1_threshold": int((v[a].values >= thr_a[a]).sum()),
                            "block_recall_half1_threshold": round(float((g[a][g.y == 1] >= thr_a[a]).mean()), 4)} for a in ARMS}

    others = [n for n in V if n not in ("clean", "timing_feasible")]
    R = {"script": "research/evasion_fix.py", "entry": "docs/decision.md 8al, 8am",
         "split": "HI-Small validation second half (original rows %d to %d); test not read" % (MID, TEST0 - 1),
         "half1": "research/out/evasion_fix_half1.json", "timing_feasible_moved_payments": n_moved, "r1": r1, "B": B, "half2_payments": n2, "cases": int(len(cases)),
         "blocks": nb, "fresh_payee_cohort_G": int(len(G)), "bootstrap": {"resamples": BOOT, "seed": BOOT_SEED, "block": "(payer, 24 h from VAL0)"},
         "recall_at_B": {n: {a: round(x, 4) for a, x in r.items()} for n, r in rec.items()},
         "precision_clean_at_B": {a: round(x, 4) for a, x in prec.items()},
         "fpr_G": {a: (round(x, 5) if x is not None else None) for a, x in fpr.items()},
         "secondary": secondary, "arms": {}}
    Sb_clean = rec["clean"]["S"]
    pc, fc = boot_prec("C"), boot_fpr("C")
    for x in CANDS:
        tg = TARGET[x]
        comp = "S" if x == "T-pair" else "C"
        d1 = boot_recall(tg, x) - boot_recall(tg, comp)
        delta = rec[tg][x] - rec[tg][comp]
        s_drop = Sb_clean - rec[tg]["S"]
        gain_s = rec[tg][x] - rec[tg]["S"]
        r1c = {"delta_vs": comp, "delta": round(delta, 4), "delta_ci95": ci(d1), "gain_over_S": round(gain_s, 4),
               "half_S_drop": round(0.5 * s_drop, 4)}
        r1c["pass"] = bool(delta >= R1_MIN and ci(d1)[0] > 0 and gain_s >= 0.5 * s_drop)
        dp = boot_prec(x) - pc
        r2c = {"delta_vs_C": round(prec[x] - prec["C"], 4), "delta_ci95": ci(dp), "vs_S": round(prec[x] - prec["S"], 4)}
        r2c["pass"] = bool(prec[x] - prec["C"] >= R2_MIN and ci(dp)[0] > R2_LO and prec[x] >= prec["S"] + R2_S)
        oth = [a for a in others if a != tg]
        r3c = {a: round(rec[a][x] - rec[a]["S"], 4) for a in oth}
        r3 = {"recall_minus_S": r3c, "worst": min(r3c, key=r3c.get), "pass": bool(all(rec[a][x] >= rec[a]["S"] - R3_MARGIN for a in oth))}
        if len(G) < G_MIN:
            r4 = {"G": int(len(G)), "status": "inconclusive", "pass": False}
        else:
            df = boot_fpr(x) - fc
            r4 = {"G": int(len(G)), "fpr": round(fpr[x], 5), "delta_vs_C": round(fpr[x] - fpr["C"], 5), "delta_ci95": ci(df)}
            ok = fpr[x] <= R4_CAP and fpr[x] - fpr["C"] <= R4_INC and ci(df)[1] <= R4_INC_HI
            r4.update(status="pass" if ok else "fail", **{"pass": bool(ok)})
        if len(G):
            r4["fpr_observed"], r4["fpr_C_observed"] = round(fpr[x], 5), round(fpr["C"], 5)
        arm = {"target": tg, "R1": r1c, "R2": r2c, "R3": r3, "R4": r4}
        arm["outcome"] = "pass" if all(arm[k]["pass"] for k in ("R1", "R2", "R3", "R4")) else (
            "inconclusive" if r4.get("status") == "inconclusive" and all(arm[k]["pass"] for k in ("R1", "R2", "R3")) else "fail")
        if x == "T2":
            dfz = rec["timing_feasible"]["T2"] - rec["timing_feasible"]["C"]
            arm["secondary_feasible_order_R1"] = {"recall_T2": round(rec["timing_feasible"]["T2"], 4),
                                                  "recall_C": round(rec["timing_feasible"]["C"], 4), "delta": round(dfz, 4),
                                                  "delta_ci95": ci(boot_recall("timing_feasible", "T2") - boot_recall("timing_feasible", "C"))}
        R["arms"][x] = arm
    timing_pass = [x for x in ("T", "T-pair", "T2") if R["arms"][x]["outcome"] == "pass"]
    R["served"] = {"timing": max(timing_pass, key=lambda x: R["arms"][x]["R1"]["delta"]) if timing_pass else None,
                   "new_payee": "N" if R["arms"]["N"]["outcome"] == "pass" else None}
    R["seconds"] = round(time.time() - t0)
    json.dump(R, open(os.path.join(OUT, "evasion_fix.json"), "w"), indent=1)
    print(json.dumps({k: R[k] for k in ("r1", "B", "cases", "fresh_payee_cohort_G", "served")}, indent=1))
    print(json.dumps({x: {k: a[k]["pass"] for k in ("R1", "R2", "R3", "R4")} | {"outcome": a["outcome"]} for x, a in R["arms"].items()}, indent=1))


if __name__ == "__main__":
    self_test() if sys.argv[1:] == ["--self-test"] else main()
