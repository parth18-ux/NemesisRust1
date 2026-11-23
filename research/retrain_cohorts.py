"""Frozen-data design of decision entry 8ao (amendment to 8an): which rows screen, which freeze, which confirm.

    python3 -I research/retrain_cohorts.py

Reads research/out/retrain_data_manifest.json (file paths, hashes, row counts, date ranges) and, from each time-sorted
file it names, the timestamp column only (first field). No label, feature, score or model is read. Row ids are the
0-based data-line index of the time-sorted file, the `row` of research_dump. Prior use of HI-Medium and SAML-D intervals
is checked against the committed results in research/out/. Writes research/out/retrain_cohorts.json.
"""
import gzip, hashlib, json, os

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
OUT = os.path.join(ROOT, "research", "out")
VAL0, TEST0 = 3_047_007, 4_062_676          # HI-Small boundaries of 8al / research/full_hi_eval.py
MID = VAL0 + (TEST0 - VAL0) // 2            # validation half 1 | half 2 (8al)


def sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for b in iter(lambda: f.read(1 << 22), b""):
            h.update(b)
    return h.hexdigest()


def timestamps(path):
    """timestamp of every data row, in file order (header skipped); nothing else of the row is kept"""
    with gzip.open(path, "rt") as f:
        next(f)
        return [line[:16] for line in f]


def span(ts, a, b):
    return {"rows": [a, b], "count": b - a, "ts_first": ts[a], "ts_last": ts[b - 1]} if b > a else None


def main():
    M = json.load(open(os.path.join(OUT, "retrain_data_manifest.json")))
    files = {"hi_small": M["hi_small"]["sorted"], "hi_medium": M["hi_medium"]["sorted"], "saml_d": M["saml_d"]["converted"]}
    ts = {}
    for k, f in files.items():
        assert sha256(f["path"]) == f["sha256"], k
        ts[k] = timestamps(f["path"])
        assert len(ts[k]) == f["rows"] and all(a <= b for a, b in zip(ts[k], ts[k][1:])), k
        assert (ts[k][0], ts[k][-1]) == (f["ts_min"], f["ts_max"]), k
    hs, n_hs = ts["hi_small"], len(ts["hi_small"])
    hs_end = hs[-1]

    # prior use, from committed results only (dataset key -> where each chronological part was used)
    load = lambda n: json.load(open(os.path.join(OUT, n)))
    aml, pool, uni, htr = load("aml_hardening.json"), load("pooled_hardening.json"), load("unified_train.json"), load("hardened_transfer.json")

    def uses(key):
        u = {"train_60": [], "val_60_80": [], "test_80_100": []}
        if key in aml["trained_on"]:
            u["train_60"].append("aml_hardening.json trained_on (training part, research/aml_hardening.py)")
        if key in uni["datasets"]:
            u["train_60"].append("unified_train.json datasets (training part of the pooled model served as service/models/hardening.json via research/pooled_hardening.py)")
        if key in aml["validation"]:
            u["val_60_80"].append("aml_hardening.json validation (scored; decided B1/B2)")
        if key in pool["validation"]:
            u["val_60_80"].append("pooled_hardening.json validation (scored; decided G3, adopted)")
        if key in aml["test"]:
            u["test_80_100"].append("aml_hardening.json test")
        if key in pool["test"]:
            u["test_80_100"].append("pooled_hardening.json test")
        if key in uni["test"]:
            u["test_80_100"].append("unified_train.json test")
        if key in htr:
            u["test_80_100"].append(f"hardened_transfer.json {key} (test_positives {htr[key]['test_positives']})")
        return u

    def candidate(name, key, extra=None):
        t, n = ts[name], len(ts[name])
        a, b = int(n * 0.6), int(n * 0.8)
        first_after = next((i for i, x in enumerate(t) if x > hs_end), n)
        u = uses(key)
        parts = {"train_60": span(t, 0, a), "val_60_80": span(t, a, b), "test_80_100": span(t, b, n)}
        for p in parts:
            parts[p]["prior_use"] = u[p]
        later = span(t, first_after, n)
        if later:
            later["overlaps_used_parts"] = [p for p, s in parts.items() if s["prior_use"] and s["rows"][0] < n and first_after < s["rows"][1]]
            later["eligible"] = not later["overlaps_used_parts"]
        r = {"rows": n, "parts": parts, "rows_with_ts_le_hi_small_end": first_after, "time_later_than_hi_small": later,
             "row_overlap_with_hi_small": "none: a separate file with its own row ids",
             "eligible_interval": bool(later and later["eligible"])}
        if extra:
            r.update(extra)
        return r

    hm = candidate("hi_medium", "himed", {"positives_kept_in_scored_test": M["hi_medium"]["checks"]["last_20pct_vs_research/out/hardened_transfer.json_himed"]})
    sd = candidate("saml_d", "samld")
    eligible = [k for k, c in (("hi_medium", hm), ("saml_d", sd)) if c["eligible_interval"]]

    R = {"entry": "docs/decision.md 8ao (amendment to 8an)",
         "source": "research/out/retrain_data_manifest.json; timestamps read from the files it names (sha256 checked)",
         "row_id": "0-based data-line index of the time-sorted file (research_dump `row`)",
         "hi_small": {"rows": n_hs, "ts_first": hs[0], "ts_last": hs_end,
                      "train": span(hs, 0, VAL0),
                      "half1_freeze": span(hs, VAL0, MID),
                      "half2_screen": span(hs, MID, TEST0),
                      "test_descriptive_locked": span(hs, TEST0, n_hs)},
         "confirmation_candidates": {"hi_medium": hm, "saml_d": sd},
         "confirmation_cohort": None if not eligible else eligible,
         "confirmation_reason": ("none eligible: every row of HI-Medium and SAML-D lies in a part already used for training "
                                 "the served hardening model or scored by an earlier study (see prior_use); HI-Small has no "
                                 "rows after its own test part. The incumbent S is kept.") if not eligible else "see eligible",
         "li_small": "unmeasured; outside the replacement claim"}
    json.dump(R, open(os.path.join(OUT, "retrain_cohorts.json"), "w"), indent=1)
    print(json.dumps({"confirmation_cohort": R["confirmation_cohort"], "hi_medium_later": hm["time_later_than_hi_small"]}, indent=1))


if __name__ == "__main__":
    main()
