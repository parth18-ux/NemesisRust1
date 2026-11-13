"""Do MonLAD-style fan-out (dispersion) features raise the block band? Controlled test on the full HI-Small stream.

    python3 -I research/fanout_eval.py <hismall research_dump csv.gz> <hi_small_sorted.csv.gz>

MonLAD (WSDM 2022; no code found, so this is our own code from the idea) finds money-laundering agent accounts by the
way they disperse what they receive: many small transfers to many distinct payees inside a time window. The served
features already hold the 24 hour payee count, payment count and amount of the payer (s_fanout_24h, s_cnt_24h,
s_usd_24h); they do not say how the money is spread. Four features per payer account over a sliding 24 hours, causal:
each reads only payments at or before this one in the time-sorted stream, this one included:
  fo_entropy_24h       Shannon entropy (nats) of the payer's outgoing USD over its distinct payees
  fo_small_share_24h   share of the payer's outgoing payments that are small
  fo_small_payees_24h  distinct payees paid a small amount (log1p)
  fo_through_24h       min(out, in) / max(out, in) of the account's 24 hour USD: money received and passed on
                       (a payment to oneself is not counted as received)
"Small" is a payment below the median log_usd of the training rows (rows before VAL0), a cut fitted on train only.
Every HI-Small payment is used in time order (the Medium and Large dumps were pre-sampled, so their streams are
incomplete and cannot be used).
Two models, identical except for the features, trained on HI-Small train rows (early stop on its last 10%):
  ctrl    the 31 served features
  fanout  the 31 served features + the 4 fan-out features
Arms (L2 = mean of the served XGBoost, LightGBM and the model; hardening as served, isotonic map refitted):
  current, mean3_ctrl, mean3_fanout. Calibration on the first half of HI-Small validation; judged on the second half at
  the served alert counts (k highest scores per band), as research/new_features_eval.py and research/midas_eval.py.
ADOPTION RULE (fixed here before any number is computed; second half of validation only):
  F1  mean3_fanout block recall at k >= current + 0.02
  F2  mean3_fanout block recall at k >= mean3_ctrl + 0.01   (the gain comes from the features, not the extra model)
  F3  mean3_fanout PR-AUC >= current - 0.005
  F4  mean3_fanout worst PR-AUC under the format attack >= current worst - 0.02
Also reported, not judged: each fan-out feature alone (ROC-AUC, PR-AUC on validation). If adopted, the features are
ported to Rust (engine and Redis worker) before anything is served; if not, the served model stays as it is.
Results: research/out/fanout_eval.json
"""
import json, math, os, sys, time
from collections import deque
import numpy as np, pandas as pd, xgboost as xgb, lightgbm as lgb
from sklearn.isotonic import IsotonicRegression
from sklearn.metrics import average_precision_score as AP, roc_auc_score as AUC

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from full_hi_eval import CFG, F, L1T, VAL0, TEST0  # noqa: E402
from l3_eval import PARAMS  # noqa: E402
from l3_stack_eval import at_k  # noqa: E402

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
OUT, MD = os.path.join(ROOT, "research", "out"), os.path.join(ROOT, "service", "models")
H = json.load(open(os.path.join(MD, "hardening.json")))
BANDS = ("verify", "hold", "block")
FO = ["fo_entropy_24h", "fo_small_share_24h", "fo_small_payees_24h", "fo_through_24h"]
WIN = 24 * 60


def fanout(src, dst, minute, usd, small):
    """Sliding 24 hour state per account: outgoing (minute, payee, usd, small) and incoming (minute, usd). The entropy is
    kept in O(1) per payment from S = sum v and Q = sum v ln v over payees: H = ln S - Q / S."""
    out = np.zeros((len(src), 4), np.float32)
    A = {}  # account -> [out deque, in deque, payee->usd, payee->small count, S, Q, n_small, in_usd]
    xlogx = lambda v: v * math.log(v) if v > 0 else 0.0

    def expire(st, now):
        o, inn, pay, sp = st[0], st[1], st[2], st[3]
        while o and o[0][0] <= now - WIN:
            _, p, v, sm = o.popleft()
            old = pay[p]; new = old - v
            st[5] -= xlogx(old); st[4] -= v
            if new > 1e-9 * old:
                pay[p] = new; st[5] += xlogx(new)
            else:
                del pay[p]
            if sm:
                st[6] -= 1; sp[p] -= 1
                if not sp[p]:
                    del sp[p]
        if not o:
            st[4] = st[5] = 0.0  # no drift carried over an empty window
        while inn and inn[0][0] <= now - WIN:
            st[7] -= inn.popleft()[1]
        if not inn:
            st[7] = 0.0

    new_state = lambda: [deque(), deque(), {}, {}, 0.0, 0.0, 0, 0.0]
    for i in range(len(src)):
        s, d, t, v, sm = src[i], dst[i], minute[i], usd[i], small[i]
        st = A.get(s)
        if st is None:
            st = A[s] = new_state()
        expire(st, t)
        pay = st[2]; old = pay.get(d, 0.0)
        st[5] += xlogx(old + v) - xlogx(old); st[4] += v; pay[d] = old + v
        st[0].append((t, d, v, sm))
        if sm:
            st[6] += 1; st[3][d] = st[3].get(d, 0) + 1
        S = st[4]
        o_usd, i_usd = S, st[7]
        out[i] = (max(0.0, math.log(S) - st[5] / S) if S > 0 else 0.0, st[6] / len(st[0]), math.log1p(len(st[3])),
                  min(o_usd, i_usd) / max(o_usd, i_usd) if max(o_usd, i_usd) > 0 else 0.0)
        if d != s:
            sd = A.get(d)
            if sd is None:
                sd = A[d] = new_state()
            sd[1].append((t, v)); sd[7] += v
    return out


def main():
    t0 = time.time()
    raw = pd.read_csv(sys.argv[2], usecols=["ts", "fb", "fa", "tb", "ta"], dtype=str)
    minute = pd.to_datetime(raw.ts, format="%Y/%m/%d %H:%M").values.astype("datetime64[m]").astype("int64")
    ids = {}
    node = lambda a: ids.setdefault(a, len(ids))
    src = [node(x) for x in (raw.fb + "|" + raw.fa).values]
    dst = [node(x) for x in (raw.tb + "|" + raw.ta).values]
    del raw
    d = pd.read_csv(sys.argv[1], usecols=["row", "y"] + F, dtype=np.float32)
    assert len(d) == len(src) == 5_078_345
    lu = d.log_usd.values.astype(np.float64)
    cut = float(np.median(lu[:VAL0]))
    m = fanout(src, dst, minute.tolist(), np.maximum(np.expm1(lu), 1e-6).tolist(), (lu < cut).tolist())
    del src, dst
    print("fanout", round(time.time() - t0), "s", flush=True)
    y = d.y.values.astype(np.int8)
    X = np.hstack([d[F].values.astype(np.float32), m])
    FM = F + FO
    del d
    rng = np.random.default_rng(43)
    es0 = int(VAL0 * 0.9)
    k = (y[:es0] == 1) | (rng.random(es0) < 0.25)
    w = np.where(y[:es0][k] == 1, 1.0, 4.0).astype(np.float32)
    models = {}
    for arm, cols in (("ctrl", F), ("fanout", FM)):
        ix = [FM.index(c) for c in cols]
        b = xgb.train(PARAMS, xgb.DMatrix(X[:es0][k][:, ix], label=y[:es0][k], weight=w, missing=np.nan, feature_names=cols), 3000,
                      evals=[(xgb.DMatrix(X[es0:VAL0][:, ix], label=y[es0:VAL0], missing=np.nan, feature_names=cols), "es")],
                      early_stopping_rounds=100, verbose_eval=False)
        models[arm] = (b[: b.best_iteration + 1], ix, cols)
        print(arm, "rounds", b.best_iteration + 1, flush=True)
    b1 = xgb.Booster(); b1.load_model(os.path.join(MD, "l1_gatekeeper.json"))
    bx = xgb.Booster(); bx.load_model(os.path.join(MD, "l2_xgb.json"))
    bl = lgb.Booster(model_file=os.path.join(ROOT, "ml", "exported", "l2_lgbm.txt"))
    bf = xgb.Booster(); bf.load_model(os.path.join(MD, H["model"]))
    fcols, fi, base = [FM.index(f) for f in H["features"]], FM.index("fmt_code"), [FM.index(f) for f in F]

    def members(Xm):
        dm = xgb.DMatrix(Xm[:, base], missing=np.nan, feature_names=F)
        z = {"l1": b1.predict(dm), "sx": bx.predict(dm), "sl": bl.predict(Xm[:, base].astype(np.float64)),
             "free": np.interp(bf.predict(xgb.DMatrix(Xm[:, fcols], missing=np.nan, feature_names=H["features"])), H["iso_free"]["x"], H["iso_free"]["y"])}
        for arm, (b, ix, cols) in models.items():
            z[arm] = b.predict(xgb.DMatrix(Xm[:, ix], missing=np.nan, feature_names=cols))
        return z

    def cascade(z, arm):
        l2 = (z["sx"] + z["sl"]) / 2 if arm == "current" else (z["sx"] + z["sl"] + z[arm.split("_")[1]]) / 3
        return np.where(z["l1"] < L1T, z["l1"], l2).astype(np.float64)

    def attacked(Xm, yy, z, code):
        Xa = Xm[yy == 1].copy(); Xa[:, fi] = code
        za = members(Xa); out = {c: v.copy() for c, v in z.items()}
        for c in out:
            out[c][yy == 1] = za[c]
        return out

    mid = VAL0 + (TEST0 - VAL0) // 2
    z1, z2, y1, y2 = members(X[VAL0:mid]), members(X[mid:TEST0]), y[VAL0:mid], y[mid:TEST0]
    served2 = np.maximum(np.interp(cascade(z2, "current"), H["iso_served"]["x"], H["iso_served"]["y"]), z2["free"])
    ks = {b: int((served2 >= H["thresholds"][b]).sum()) for b in BANDS}
    codes = {n: c for n, c in CFG["fmt_codes"].items() if n not in ("ACH", "Reinvestment")}
    yv = y[VAL0:TEST0]
    R = {"rule": {"F1_min_gain_vs_current": 0.02, "F2_min_gain_vs_ctrl": 0.01, "F3_max_pr_auc_drop": 0.005, "F4_max_attack_drop": 0.02},
         "fanout": {"window_minutes": WIN, "small_cut_log_usd_train_median": round(cut, 4), "features": FO},
         "features_alone_validation": {c: {"roc_auc": round(float(AUC(yv, X[VAL0:TEST0, FM.index(c)])), 4),
                                           "pr_auc": round(float(AP(yv, X[VAL0:TEST0, FM.index(c)])), 4)} for c in FO},
         "rounds": {a: int(v[0].num_boosted_rounds()) for a, v in models.items()}, "alert_counts_second_half": ks, "validation_second_half": {}}
    for arm in ("current", "mean3_ctrl", "mean3_fanout"):
        iso = IsotonicRegression(out_of_bounds="clip", y_min=0, y_max=1).fit(cascade(z1, arm), y1)
        score = lambda z: np.maximum(iso.predict(cascade(z, arm)), z["free"])
        s = score(z2)
        att = {n: round(float(AP(y2, score(attacked(X[mid:TEST0], y2, z2, c)))), 4) for n, c in codes.items()}
        R["validation_second_half"][arm] = {"pr_auc": round(float(AP(y2, s)), 4), "attack_worst": min(att.values()),
                                            "bands": {b: at_k(y2, s, kk) for b, kk in ks.items()}}
        print(arm, R["validation_second_half"][arm]["pr_auc"], R["validation_second_half"][arm]["bands"]["block"], flush=True)
    V = R["validation_second_half"]; c, n, ct = V["current"], V["mean3_fanout"], V["mean3_ctrl"]
    rb = lambda v: v["bands"]["block"]["recall"]
    checks = {"F1": rb(n) >= rb(c) + 0.02, "F2": rb(n) >= rb(ct) + 0.01, "F3": n["pr_auc"] >= c["pr_auc"] - 0.005,
              "F4": n["attack_worst"] >= c["attack_worst"] - 0.02}
    R["decision"] = {"checks": checks, "adopt": all(checks.values()), "seconds": round(time.time() - t0),
                     "note": "decided on the second half of validation only; the test part is not read by this script"}
    R["fanout_model_gain"] = {k2: round(float(v), 2) for k2, v in sorted(models["fanout"][0].get_score(importance_type="gain").items(), key=lambda kv: -kv[1])[:12]}
    json.dump(R, open(os.path.join(OUT, "fanout_eval.json"), "w"), indent=1)
    print(json.dumps(R["decision"]), flush=True)


if __name__ == "__main__":
    main()
