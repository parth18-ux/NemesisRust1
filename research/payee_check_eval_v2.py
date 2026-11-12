"""Payee name check v2 (nicknames, surname-anchored close match): pre-registration. Committed before any v2 code.

Why: v1 (research/payee_check_eval.py, served) labels 0.2094 of different-person test pairs close_match, which shows
the payer someone else's name, and does not recognise hard nicknames (Bob for Robert). The v1 test split has been read,
so v2 is judged on a FRESH set.

v2, to be written after this commit, behind a switch in crates/rusetect/src/payee.rs (`check(..., v2)`); the scorer
stays sanctions `score_v2`, unchanged. Business accounts are scored and banded exactly as in v1. For individuals:
  a. nickname table: common English diminutives map to a canonical given name (Bob/Rob/Robbie -> Robert, ...), applied
     to given-name tokens of both names before scoring (every account token but the last; every entered token except
     one equal to the account surname). The table is written by hand in the Rust code.
  b. surname-anchored close match: close_match also needs an entered token that matches the account surname (its last
     token) with token score (`token_v2`) >= T_HI. The given name may differ or be an initial. match is unchanged.
payee_dump takes a third argument "v2" to run v2.

FRESH SET: the v1 generator (payee_check_eval.person_pairs / business_pairs / build, same categories, same 300 people
and 100 businesses, same 60/20/20 split by identity) with a new seed SEED and new name pools below, disjoint from the
v1 pools (FIRST, LAST, MIDDLE, NICK, BIZ_WORD, BIZ_TRADE; the legal suffixes are kept, they are not names).

ADOPTION RULE v2 (fixed before any change):
  Thresholds for v2 fitted on the FRESH validation split only (v1 keeps its served thresholds, 0.9352 / 0.7967):
    T_HI = the lowest candidate t (distinct validation v2 scores) such that, for t and every higher candidate, v2
           "match" precision on validation is 1.0 (the test bar below).
    T_LO = the highest candidate t <= T_HI such that the share of validation same-payee pairs labelled match or
           close_match by v2 at (T_HI, t) is >= 0.985 (test bar 0.98 plus margin).
  Adopt v2 (serve it in place of v1) only if ALL hold on the FRESH test split:
    a. match precision = 1.0 (no different-person pair labelled match);
    b. same-payee pairs labelled match or close_match >= 0.98;
    c. different-person close_match rate <= half of v1's rate on the same fresh test split.
  Reported, not gating: v1 and v2 on the fresh test and on the old (v1) test split, per-category verdict counts, and
  the close_match rate over individual accounts only.
  Otherwise v2 stays in the code behind the switch, v1 stays served, and the failure is reported. Fresh test read once.
Results: research/out/payee_check_eval_v2.json (read by the API: v2 and its thresholds only when "adopted" is true).

Known limits, stated before the run: (1) business close matches are unchanged by design; in the v1 generator
same-trade and owner-name business pairs are all close_match and make up 40 of 320 different test pairs (0.125), more
than half of v1's 0.2094, so check c may be out of reach whatever v2 does for individuals. (2) The nickname pairs in
the fresh set and the Rust table are written by the same author, so nickname recall here is not an independent
measure. (3) Ambiguous diminutives (Sam, Alex, Chris) map to one name only. (4) Names are synthetic.
"""
import json, os, random, subprocess, sys
from datetime import date

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import payee_check_eval as v1  # noqa: E402

OUT = os.path.join(v1.se.ROOT, "research", "out", "payee_check_eval_v2.json")
V1_OUT = v1.OUT
SEED = 20261114
FIRST = ["Richard", "Anthony", "David", "Samuel", "Andrew", "Stephen", "Susan", "Edward", "Patricia", "Charles",
         "Timothy", "Peter", "Frederick", "Gregory", "Abigail", "Deborah", "Lawrence", "Barbara", "Kenneth", "Donald",
         "Ronald", "Anil", "Deepa", "Hamza", "Zainab", "Kenji", "Haruka", "Tomasz", "Katarzyna", "Javier", "Carmen",
         "Emeka", "Adaeze", "Niamh", "Ciaran", "Dafydd", "Freya", "Magnus", "Leila", "Omar", "Ravi", "Sunita", "Bogdan",
         "Elena", "Kofi", "Ama", "Isla", "Callum", "Megan", "Owen"]
LAST = ["Harrison", "Cooper", "Ward", "Morris", "Bennett", "Price", "Bailey", "Kelly", "Turner", "Parker", "Collins",
        "Stewart", "Morgan", "Cook", "Rogers", "Reed", "Bell", "Watson", "Gray", "Foster", "Shah", "Iqbal", "Rahman",
        "Mistry", "Akhtar", "Liu", "Zhang", "Tanaka", "Suzuki", "Park", "Nowak", "Wisniewski", "Horvat", "Popescu",
        "Sullivan", "Byrne", "Campbell", "Fraser", "Adeyemi", "Boateng", "Lopez", "Moretti", "Schmidt", "Andersson",
        "Dubois", "Kuznetsov", "Volkova", "Yamamoto", "Fischer", "Doherty"]
MIDDLE = ["Ann", "May", "Mark", "Hugh", "Jane", "Claire", "Raj", "Noor", "Kai", "Joy", "Ray", "Lynn"]
NICK = {"Richard": "Rick", "Anthony": "Tony", "David": "Dave", "Samuel": "Sam", "Andrew": "Andy", "Stephen": "Steve",
        "Susan": "Sue", "Edward": "Ted", "Patricia": "Trish", "Charles": "Charlie", "Timothy": "Tim", "Peter": "Pete",
        "Frederick": "Fred", "Gregory": "Greg", "Abigail": "Abby", "Deborah": "Debbie", "Lawrence": "Larry",
        "Barbara": "Barb", "Kenneth": "Ken", "Donald": "Don", "Ronald": "Ron"}
BIZ_WORD = ["Ashdown", "Birchwood", "Coastline", "Dovecote", "Elmstead", "Foxglove", "Ironbridge", "Juniper",
            "Kingfisher", "Larkspur", "Moorland", "Nettlefield", "Orchard", "Primrose", "Redwood", "Saltmarsh",
            "Tidewater", "Upland", "Wharfside", "Hawthorn"]
BIZ_TRADE = ["Glazing", "Scaffolding", "Plastering", "Flooring", "Veterinary", "Bakery", "Printing", "Couriers",
             "Security", "Upholstery", "Tiling", "Haulage", "Surveyors", "Solicitors", "Opticians"]
POOLS = ("FIRST", "LAST", "MIDDLE", "NICK", "BIZ_WORD", "BIZ_TRADE", "SEED")


def build(fresh):
    """The v1 pairs (old split) or the fresh set: the v1 generator run on swapped pools."""
    old = {k: getattr(v1, k) for k in POOLS}
    if fresh:
        for k in POOLS:
            setattr(v1, k, globals()[k])
    try:
        return v1.build()
    finally:
        for k, v in old.items():
            setattr(v1, k, v)


def dump(rows, t_hi, t_lo, version):
    lines = "".join(f"{e}\t{a}\t{t}\n" for _, _, e, a, t in rows)
    r = subprocess.run([v1.BIN, str(t_hi), str(t_lo), version], input=lines, capture_output=True, text=True, check=True)
    out = [line.split("\t") for line in r.stdout.splitlines()]
    assert len(out) == len(rows)
    return [(float(s), v) for s, v in out]


def fit(rows):
    """v2 T_HI, T_LO on the fresh validation split, as pre-registered."""
    sc = [(s, v != "match", y) for (s, v), (_, y, *_) in zip(dump(rows, 0, 0, "v2"), rows)]
    cands = sorted({s for s, _, _ in sc})

    def prec(t):
        m = [y for s, mis, y in sc if s >= t and not mis]
        return sum(m) / len(m) if m else 1.0
    ok, t_hi = True, cands[-1]
    for t in reversed(cands):
        ok = ok and prec(t) == 1.0
        if ok:
            t_hi = t
    # at t_lo = 0 a pair below T_HI is close_match exactly when the surname anchor (or business banding) allows it
    anch = [(s, v != "no_match") for (s, v), (_, y, *_) in zip(dump(rows, t_hi, 0, "v2"), rows) if y]
    t_lo = max(t for t in cands if t <= t_hi and sum(a and (s >= t or s >= t_hi) for s, a in anch) / len(anch) >= 0.985)
    return t_hi, t_lo


def report(rows, verdicts):
    r = v1.report(rows, verdicts)
    ind = [v for (_, y, _, _, t), (_, v) in zip(rows, verdicts) if not y and t == "individual"]
    r["different_close_match_share_individual_accounts"] = round(sum(v == "close_match" for v in ind) / len(ind), 4)
    return r


def main():
    v1_res = json.load(open(V1_OUT))
    v1_thr = (v1_res["t_hi"], v1_res["t_lo"])
    fresh, old = build(True), build(False)
    t_hi, t_lo = fit(fresh["validation"])
    res = {}
    for name, rows in (("fresh_validation", fresh["validation"]), ("fresh_test", fresh["test"]), ("old_test", old["test"])):
        res[name] = {"v1": report(rows, dump(rows, *v1_thr, "v1")), "v2": report(rows, dump(rows, t_hi, t_lo, "v2"))}
    f1, f2 = res["fresh_test"]["v1"], res["fresh_test"]["v2"]
    checks = {"a_fresh_test_v2_match_precision_eq_1": f2["match_precision"] == 1.0,
              "b_fresh_test_v2_same_match_or_close_ge_0.98": f2["same_match_or_close"] >= 0.98,
              "c_fresh_test_v2_different_close_le_half_v1": f2["different_close_match_share"] <= f1["different_close_match_share"] / 2}
    out = {"rule": "ADOPTION RULE v2 in research/payee_check_eval_v2.py (pre-registered)", "date": date.today().isoformat(),
           "scorer": "Rust payee::check(.., v2) via the payee_dump example (sanctions score_v2 unchanged); no Python mirror",
           "seed": SEED, "old_seed": v1.SEED, "people": v1.N_PEOPLE, "businesses": v1.N_BUSINESSES,
           "threshold_rule": "v2 fitted on the fresh validation split only: T_HI lowest with match precision 1.0 (and above); T_LO highest <= T_HI with same-pair match-or-close >= 0.985",
           "v1_thresholds": {"t_hi": v1_thr[0], "t_lo": v1_thr[1]}, "t_hi": t_hi, "t_lo": t_lo, **res,
           "checks": checks, "adopted": all(checks.values())}
    with open(OUT, "w") as f:
        json.dump(out, f, indent=2)
        f.write("\n")
    json.dump({k: v for k, v in out.items() if k in ("t_hi", "t_lo", "checks", "adopted")}, sys.stdout, indent=2)


if __name__ == "__main__":
    main()
