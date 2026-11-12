"""Sanctions name screening, matcher v2: pre-registration. No matcher code existed when this file was committed.

ADOPTION RULE v2 (fixed before any change)

Why: the v1 matcher (research/out/sanctions_eval.json) misses its recall target on test, and the misses are the
transliteration and dropped-middle-name variants. v1's test split has now been read, so it cannot judge a matcher
designed after reading it. v2 is therefore judged on a FRESH test set defined below, built from entities and common
names that no earlier run has used.

Candidate (v2 scorer, to be written after this commit in crates/rusetect/src/sanctions.rs; v1 stays callable):
  1. Transliteration folding. Each normalised token also gets a folded key; a token pair scores
     max(Jaro-Winkler(raw), CAP * Jaro-Winkler(folded)), so an exact spelling always outranks a folded one.
     Fold, in this order: suffix off -> ov; sch -> sh, ch -> sh, ph -> f, kh -> h, gh -> g, ck -> k, ou -> u, oo -> u,
     ee -> i, ei -> ai, ay -> ai, iy -> i, x -> ks; then w -> v, z -> s, c -> k, q -> k, y -> i, j -> i, o -> u,
     e -> a; then collapse doubled letters (mm -> m).
  2. Subset match. When the query has at least two tokens and fewer tokens than the list name, align every query token
     one-to-one to a distinct list token; if each aligned pair scores at least FLOOR, the subset score is the mean of
     those pairs minus PENALTY per unmatched list token, and the final score is max(v1 symmetric score, subset score).
     Single-token queries never use the subset rule.
  Starting constants CAP = 0.96, FLOOR = 0.90, PENALTY = 0.06. They may be changed using the v1 train and validation
  splits only, never the v1 test split and never the fresh set.

Threshold: fitted on the v1 validation split only, with the unchanged v1 rule (the score with validation recall >= 0.95
and the highest precision; ties: the higher threshold), on v2 scores against the original list.

FRESH test set (everything below is frozen in this file):
  - entities: FRESH_ENTITIES, 40 new fictitious names (28 individuals, 12 entities), none on the v1 list. All 40 are
    test; nothing is fitted on them.
  - list: the original list plus FRESH_ENTITIES, written to research/out/sanctions_fresh_list.csv by the script.
  - variants: sanctions_eval.variants() unchanged (exact, reordered, noise, typo1, typo2, translit, dropped) with
    random.Random(FRESH_SEED) and TRANSLIT replaced by FRESH_TRANSLIT (the v1 rewrites plus further romanisation
    variants).
  - positives: (variant, its own entry). Negatives: (variant, the highest-scoring other entry of the fresh list) and
    (common name, the highest-scoring entry) for 150 names sampled with the same rng from FRESH_FIRST x FRESH_LAST,
    pools that share no name with the v1 pools.
  - both matchers are scored on it once: v1 at the v1 threshold, v2 at the v2 threshold.

Adopt v2 (serve it, with its threshold) only if ALL hold; otherwise keep v1 and report the failed attempt:
  a. fresh test: recall >= 0.95 and precision >= 0.98;
  b. v1 test split, original list: recall is not below the v1 test recall in research/out/sanctions_eval.json, and
     precision >= 0.98.
The fresh set is read once. If v2 fails, the fresh set is spent as well: a further attempt needs another fresh set.
Results: research/out/sanctions_eval_v2.json (both matchers, both test sets, recall by variant, the decision).

Known limit, stated before the run: variants are synthetic and FRESH_TRANSLIT overlaps the folding table, so the fresh
set measures generalisation to new names more than to unseen romanisation habits; the list is fictitious.
"""
FRESH_SEED = 20261112
FRESH_ENTITIES = [  # (name, type)
    ("Leontiy Barsukhov", "individual"), ("Yevgeniya Chornovil", "individual"),
    ("Khalid Mansour Al-Dhaheri", "individual"), ("Joaquim Estrelado", "individual"),
    ("Nguyen Thanh Phuoc", "individual"), ("Mahmoud Youssef El-Sherbini", "individual"),
    ("Stanislav Dobrovolskiy", "individual"), ("Henrietta Wolfsbane", "individual"),
    ("Park Jae-Yoon Seolhwa", "individual"), ("Cyprian Ndukwechi", "individual"),
    ("Farrukh Sodiqjonov", "individual"), ("Marguerite Delacroissant", "individual"),
    ("Sergey Pavlovich Tsybulenko", "individual"), ("Ibrahim Chaudhary Lakhvani", "individual"),
    ("Thaddeus Okonjovwe", "individual"), ("Zeynep Ayla Korkmazoglu", "individual"),
    ("Luciano Ferrandelli", "individual"), ("Jamshid Ghaffarinejad", "individual"),
    ("Wojciech Skrzypinski", "individual"), ("Chen Xiaoyu Langford", "individual"),
    ("Babajide Olumuyiwa Fashekun", "individual"), ("Ekaterina Zhuravlyova", "individual"),
    ("Phoebe Castellmere", "individual"), ("Hussein Jawad Al-Moussawi", "individual"),
    ("Rodrigo Villaseca", "individual"), ("Shohreh Kazemipour", "individual"),
    ("Vuk Jovanetic", "individual"), ("Akira Fujinomori", "individual"),
    ("Brightwater Offshore Ventures", "entity"), ("Cobaltine Resources Ltd", "entity"),
    ("Juniper Quay Freight", "entity"), ("Morozhko Energy Trading House", "entity"),
    ("Sandpiper Aviation Leasing", "entity"), ("Thornfield Chemical Works", "entity"),
    ("Ulvik Fjord Shipbrokers AS", "entity"), ("Whitecliff Phosphate Mining", "entity"),
    ("Xanthe Global Commodities", "entity"), ("Yarrowgate Investment Fund", "entity"),
    ("Kolyvan Machine Export", "entity"), ("Dharavi Textile Mills", "entity"),
]
FRESH_TRANSLIT = [
    ("ph", "f"), ("ou", "u"), ("ee", "i"), ("kh", "h"), ("y", "i"), ("mm", "m"), ("v", "w"), ("c", "k"), ("z", "s"),
    ("sh", "ch"), ("oo", "u"), ("ei", "ai"), ("o", "u"), ("a", "e"), ("j", "y"), ("ov", "off"), ("iy", "y"),
    ("x", "ks"), ("q", "k"), ("u", "ou"), ("ai", "ay"), ("f", "ph"), ("ck", "k"), ("ss", "s"),
]
FRESH_FIRST = [
    "Daniel", "Sofia", "Chen", "Ananya", "Mustafa", "Natalia", "William", "Zainab", "Miguel", "Julia", "Sergei",
    "Lin", "Michael", "Noor", "Diego", "Emma", "Karim", "Irina", "Vikram", "Hannah",
]
FRESH_LAST = [
    "Jones", "Martinez", "Li", "Gupta", "Hussain", "Sokolova", "Taylor", "Rahman", "Costa", "Schmidt", "Volkov",
    "Chen", "Williams", "Ibrahim", "Fernandez", "Kowalski", "Park", "Gomez", "Singh", "Tran",
]

# ---- evaluation (written after the pre-registration commit; the rule and frozen set above are unchanged) ----
# Scores come from the Rust scorer itself (crates/rusetect/examples/sanctions_dump, `--v2` for score_v2), so no Python
# mirror and no parity check is needed.
#     cargo build --release -p rusetect --example sanctions_dump && python3 -I research/sanctions_eval_v2.py
import csv, json, os, random, subprocess, sys
from datetime import date

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import sanctions_eval as se  # noqa: E402

FRESH_LIST = os.path.join(se.ROOT, "research", "out", "sanctions_fresh_list.csv")
OUT = os.path.join(se.ROOT, "research", "out", "sanctions_eval_v2.json")


def dump(names, path, v2):
    args = [se.BIN, path] + (["--v2"] if v2 else [])
    r = subprocess.run(args, input="\n".join(names) + "\n", capture_output=True, text=True, check=True)
    return [[float(x) for x in line.split("\t")] for line in r.stdout.splitlines()]


def rows_of(queries, sc):
    """Per split: positive (variant, own entry) and negatives (variant, best other entry), (common name, best entry)."""
    rows, by_kind = {}, {}
    for (part, kind, _, i), s in zip(queries, sc):
        r = rows.setdefault(part, [])
        if i is None:
            r.append((max(s), 0))
        else:
            r.append((s[i], 1))
            r.append((max(x for k, x in enumerate(s) if k != i), 0))
            by_kind.setdefault(part, {}).setdefault(kind, []).append(s[i])
    return rows, by_kind


def recall_by_kind(by_kind, t):
    return {k: round(sum(x >= t for x in v) / len(v), 4) for k, v in sorted(by_kind.items())}


def v1_queries():
    """The v1 splits and queries, rebuilt with the same rng sequence as sanctions_eval.main()."""
    rng = random.Random(se.SEED)
    with open(se.LIST, newline="") as f:
        entries = list(csv.DictReader(f))
    common = rng.sample(sorted({f"{a} {b}" for a in se.FIRST for b in se.LAST}), 300)
    ent_split, com_split = se.split3(range(len(entries)), rng), se.split3(common, rng)
    queries = []
    for part, idx in ent_split.items():
        for i in idx:
            queries += [(part, kind, v, i) for kind, v in se.variants(entries[i]["name"], rng).items()]
    for part, names in com_split.items():
        queries += [(part, "common", n, None) for n in names]
    return entries, queries


def fresh_queries(entries):
    """The frozen fresh set: original list + FRESH_ENTITIES, variants with FRESH_TRANSLIT, 150 fresh common names."""
    rng = random.Random(FRESH_SEED)
    se.TRANSLIT = FRESH_TRANSLIT
    common = rng.sample(sorted({f"{a} {b}" for a in FRESH_FIRST for b in FRESH_LAST}), 150)
    rows = [dict(e) for e in entries] + [{"id": f"FRESH-{k + 1:04d}", "name": n, "type": t, "program": "FICTITIOUS-TEST"}
                                         for k, (n, t) in enumerate(FRESH_ENTITIES)]
    with open(FRESH_LIST, "w", newline="") as f:
        w = csv.DictWriter(f, fieldnames=["id", "name", "type", "program"])
        w.writeheader()
        w.writerows(rows)
    queries = []
    for i in range(len(entries), len(rows)):
        queries += [("fresh", kind, v, i) for kind, v in se.variants(rows[i]["name"], rng).items()]
    queries += [("fresh", "common", n, None) for n in common]
    return queries


def fit(val):
    ok = [t for t in sorted({s for s, _ in val}) if se.metrics(val, t)["recall"] >= 0.95]
    return max(ok, key=lambda t: (se.metrics(val, t)["precision"], t))


def main():
    v1_res = json.load(open(os.path.join(se.ROOT, "research", "out", "sanctions_eval.json")))
    t1 = v1_res["threshold"]
    entries, q_old = v1_queries()
    names_old = [q[2] for q in q_old]
    old1, kind_old1 = rows_of(q_old, dump(names_old, se.LIST, False))
    old2, kind_old2 = rows_of(q_old, dump(names_old, se.LIST, True))
    assert se.metrics(old1["test"], t1) == v1_res["test"], "v1 splits must reproduce research/out/sanctions_eval.json"
    t2 = round(fit(old2["validation"]), 6)

    q_fresh = fresh_queries(entries)
    names_fresh = [q[2] for q in q_fresh]
    fr1, kind_fr1 = rows_of(q_fresh, dump(names_fresh, FRESH_LIST, False))
    fr2, kind_fr2 = rows_of(q_fresh, dump(names_fresh, FRESH_LIST, True))

    m = {"v1": {"threshold": t1, "validation": se.metrics(old1["validation"], t1), "old_test": se.metrics(old1["test"], t1),
                "fresh_test": se.metrics(fr1["fresh"], t1),
                "old_test_recall_by_variant": recall_by_kind(kind_old1["test"], t1),
                "fresh_test_recall_by_variant": recall_by_kind(kind_fr1["fresh"], t1)},
         "v2": {"threshold": t2, "validation": se.metrics(old2["validation"], t2), "old_test": se.metrics(old2["test"], t2),
                "fresh_test": se.metrics(fr2["fresh"], t2),
                "old_test_recall_by_variant": recall_by_kind(kind_old2["test"], t2),
                "fresh_test_recall_by_variant": recall_by_kind(kind_fr2["fresh"], t2)}}
    fresh, old = m["v2"]["fresh_test"], m["v2"]["old_test"]
    checks = {"a_fresh_recall_ge_0.95": fresh["recall"] >= 0.95, "a_fresh_precision_ge_0.98": fresh["precision"] >= 0.98,
              "b_old_recall_not_below_v1": old["recall"] >= v1_res["test"]["recall"],
              "b_old_precision_ge_0.98": old["precision"] >= 0.98}
    res = {"rule": "ADOPTION RULE v2 in research/sanctions_eval_v2.py (pre-registered)", "date": date.today().isoformat(),
           "scorer": "Rust (crates/rusetect/src/sanctions.rs via the sanctions_dump example; no Python mirror)",
           "constants": {"cap": 0.96, "floor": 0.90, "penalty": 0.06},
           "threshold_rule": "v2 threshold fitted on the v1 validation split only: highest precision with recall >= 0.95",
           "fresh_list": os.path.relpath(FRESH_LIST, se.ROOT), "fresh_seed": FRESH_SEED,
           "fresh_entities": len(FRESH_ENTITIES), "fresh_common_names": 150,
           **m, "checks": checks, "adopted": all(checks.values())}
    with open(OUT, "w") as f:
        json.dump(res, f, indent=2)
        f.write("\n")
    json.dump(res, sys.stdout, indent=2)


if __name__ == "__main__":
    main()
