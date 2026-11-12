"""Payee name check (confirmation-of-payee style): pre-registration. No payee check code existed when this was committed.

What is checked: the name a payer typed for the payee (entered) against the account holder name the bank holds
(account), with the account type (individual or business). The verdict is "match", "close_match" (the payer is shown the
account name) or "no_match", aimed at authorised push payment scams where the payer is steered to the wrong account.

Scorer: the served sanctions name scorer, matcher v2 (crates/rusetect/src/sanctions.rs `score_v2`: transliteration
folding + subset rule for dropped middle names), unchanged. No second matcher is written. The score is rounded to 4
decimals (as the API returns it) before any comparison. The verdict, to be written after this commit:
  match        score >= T_HI and no type mismatch
  close_match  score >= T_LO, or score >= T_HI with a type mismatch
  no_match     otherwise
Type mismatch (only when the account type is given): a business account and an entered name with no business marker
word, or an individual account and an entered name with one. The marker words are fixed in the Rust code.
Scores and verdicts come from the Rust code itself via the payee_dump example (no Python mirror):
    payee_dump T_HI T_LO < lines of "entered<TAB>account<TAB>type"  ->  lines of "score<TAB>verdict"

LABELLED PAIRS (frozen here; seed SEED). Identities: N_PEOPLE individuals (first from FIRST, last from LAST, unique
first+last; a middle name from MIDDLE with probability 0.4, then part of the account name) and N_BUSINESSES businesses
("<word> <trade> <suffix>" from BIZ_WORD, BIZ_TRADE, BIZ_SUFFIX, unique). Per individual, account = full name, type
individual:
  same      exact (full name); noise ("Mr <full>" and "<LAST UPPER>, <first>"); initial ("<f> <last>", "<f>. <last>");
            typo (sanctions_eval.typo on the full name); translit (sanctions_eval.translit); middle_dropped
            ("<first> <last>", only with a middle name); nickname (NICK[first] + last, only when first is in NICK).
  different same_surname (another FIRST name, not first and not its nickname, + last); same_first (first + another
            LAST); unrelated (another individual's full name); business_entered ("<last> <trade> <suffix>", a business
            name typed for this individual account).
Per business, account = business name, type business:
  same      exact; suffix_dropped ("<word> <trade>"); typo; noise (upper case).
  different same_trade ("<other word> <trade> <suffix>"); unrelated (another business's name); person_entered
            ("<first> <last>" of a random individual typed for this business account); owner_name (entered
            "<first> <last>", account "<first> <last> Trading Ltd", type business: an individual's name typed for a
            business account carrying that name).
Split: identities (people and businesses separately) 60/20/20 train/validation/test with random.Random(SEED); every
pair stays with its identity. Train is not used (the scorer has no learned parameters) and is reported for completeness.

THRESHOLDS, fitted on validation only (candidates: the distinct validation scores):
  T_HI = the lowest candidate t such that, for t and every higher candidate, the precision of "match" on validation is
         >= 0.995 (match = score >= t and no type mismatch; precision = same pairs / all pairs labelled match).
         (0.995, stricter than the test bar, leaves margin.)
  T_LO = the highest candidate t <= T_HI with validation same-pair coverage (score >= t, or score >= T_HI) >= 0.97.
Verdicts on validation and test are then produced by the Rust code at (T_HI, T_LO).

ADOPTION RULE (fixed before any code): serve the endpoint only if BOTH hold on test:
  a. precision of "match" >= 0.99 (different pairs are not labelled match);
  b. same pairs labelled "match" or "close_match" >= 0.95.
Otherwise the evaluation and docs are kept, the endpoint is not exposed, and the failure is reported. Test is read once.
Reported but not gating: the share of different pairs labelled close_match (how often a payer would be warned with a
suggested name that is someone else), and verdict counts per category.
Results: research/out/payee_check_eval.json (also read by the API for T_HI, T_LO and the adoption decision).

Known limits, stated before the run: names and variants are synthetic and drawn from fixed pools; NICK mixes nicknames
the scorer can see (prefix forms) with ones it cannot (Bob/Robert), so hard nicknames are expected to fall to no_match;
same-surname people whose first names differ by one letter are indistinguishable from typos by any name-only check;
there is no open-source Pay.UK CoP matching rule set to compare against (it is published only as rulebooks).
"""
import json, os, random, subprocess, sys
from datetime import date

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import sanctions_eval as se  # noqa: E402  (typo, translit, split3, ROOT)

BIN = os.path.join(se.ROOT, "target", "release", "examples", "payee_dump")
OUT = os.path.join(se.ROOT, "research", "out", "payee_check_eval.json")
SEED = 20261113
N_PEOPLE, N_BUSINESSES = 300, 100
FIRST = ["James", "Robert", "William", "Margaret", "Elizabeth", "Christopher", "Alexander", "Daniel", "Matthew",
         "Katherine", "Samantha", "Jennifer", "Nicholas", "Benjamin", "Jonathan", "Victoria", "Thomas", "Michael",
         "Rebecca", "Joseph", "Olivia", "Amelia", "Priya", "Arjun", "Fatima", "Mohammed", "Yusuf", "Aisha", "Wei", "Mei",
         "Hiroshi", "Yuki", "Olga", "Dmitri", "Sofia", "Mateo", "Lucia", "Kwame", "Ngozi", "Siobhan", "Eoin", "Rhys",
         "Bronwen", "Agnieszka", "Piotr", "Ingrid", "Lars", "Chloe", "Harry", "Grace"]
LAST = ["Smith", "Jones", "Taylor", "Brown", "Williams", "Wilson", "Johnson", "Davies", "Robinson", "Wright", "Thompson",
        "Evans", "Walker", "White", "Roberts", "Green", "Hall", "Wood", "Jackson", "Clarke", "Patel", "Khan", "Ahmed",
        "Singh", "Begum", "Chen", "Wong", "Nguyen", "Kowalski", "Novak", "Murphy", "O'Brien", "MacDonald", "Okafor",
        "Mensah", "Garcia", "Rossi", "Muller", "Ivanov", "Petrova", "Sato", "Kim", "Hughes", "Edwards", "Fitzgerald",
        "Abernathy", "Chaudhry", "Hussain", "Lindqvist", "Fernandes"]
MIDDLE = ["Anne", "Marie", "John", "Paul", "Louise", "James", "Kumar", "Ali", "Rose", "Lee", "Elizabeth", "Edward"]
NICK = {"James": "Jim", "Robert": "Bob", "William": "Bill", "Margaret": "Peggy", "Elizabeth": "Liz",
        "Christopher": "Chris", "Alexander": "Alex", "Daniel": "Dan", "Matthew": "Matt", "Katherine": "Kate",
        "Samantha": "Sam", "Jennifer": "Jen", "Nicholas": "Nick", "Benjamin": "Ben", "Jonathan": "Jon",
        "Victoria": "Vicky", "Thomas": "Tom", "Michael": "Mike", "Rebecca": "Becky", "Joseph": "Joe"}
BIZ_WORD = ["Acorn", "Beacon", "Cedar", "Harbour", "Kestrel", "Meridian", "Northgate", "Oakfield", "Pennine", "Quayside",
            "Riverside", "Summit", "Thistle", "Valley", "Willow", "Granite", "Bluebell", "Copper", "Falcon", "Lantern"]
BIZ_TRADE = ["Plumbing", "Builders", "Logistics", "Electrical", "Roofing", "Accountants", "Catering", "Motors",
             "Landscaping", "Joinery", "Dental", "Cleaning", "Consulting", "Removals", "Lettings"]
BIZ_SUFFIX = ["Ltd", "Limited", "LLP", "PLC", "& Co", "Services Ltd"]


def identities(rng):
    people, seen = [], set()
    while len(people) < N_PEOPLE:
        f, l = rng.choice(FIRST), rng.choice(LAST)
        if (f, l) in seen:
            continue
        seen.add((f, l))
        people.append({"first": f, "last": l, "middle": rng.choice(MIDDLE) if rng.random() < 0.4 else None})
    biz, seen = [], set()
    while len(biz) < N_BUSINESSES:
        w, t = rng.choice(BIZ_WORD), rng.choice(BIZ_TRADE)
        if (w, t) in seen:
            continue
        seen.add((w, t))
        biz.append({"word": w, "trade": t, "suffix": rng.choice(BIZ_SUFFIX)})
    return people, biz


def full(p):
    return " ".join(x for x in (p["first"], p["middle"], p["last"]) if x)


def biz_name(b):
    return f"{b['word']} {b['trade']} {b['suffix']}"


def person_pairs(p, people, rng):
    """(category, label, entered, account, type) for one individual; label 1 = same payee."""
    acc, f, l = full(p), p["first"], p["last"]
    same = [("exact", full(p)), ("noise", "Mr " + acc), ("noise", f"{l.upper()}, {f}"), ("initial", f"{f[0]} {l}"),
            ("initial", f"{f[0]}. {l}"), ("typo", se.typo(acc, rng)), ("translit", se.translit(acc, rng))]
    if p["middle"]:
        same.append(("middle_dropped", f"{f} {l}"))
    if f in NICK:
        same.append(("nickname", f"{NICK[f]} {l}"))
    other_first = rng.choice([x for x in FIRST if x != f and x != NICK.get(f)])
    other = rng.choice([q for q in people if q["last"] != l and q["first"] != f])
    diff = [("same_surname", f"{other_first} {l}"), ("same_first", f"{f} {rng.choice([x for x in LAST if x != l])}"),
            ("unrelated", full(other)), ("business_entered", f"{l} {rng.choice(BIZ_TRADE)} {rng.choice(BIZ_SUFFIX)}")]
    return [(c, 1, e, acc, "individual") for c, e in same] + [(c, 0, e, acc, "individual") for c, e in diff]


def business_pairs(b, biz, people, rng):
    acc = biz_name(b)
    same = [("exact", acc), ("suffix_dropped", f"{b['word']} {b['trade']}"), ("typo", se.typo(acc, rng)),
            ("noise", acc.upper())]
    other = rng.choice([x for x in biz if x["word"] != b["word"] and x["trade"] != b["trade"]])
    word = rng.choice([w for w in BIZ_WORD if w != b["word"]])
    p = rng.choice(people)
    out = [(c, 1, e, acc, "business") for c, e in same]
    out += [("same_trade", 0, f"{word} {b['trade']} {b['suffix']}", acc, "business"), ("unrelated", 0, biz_name(other), acc, "business"),
            ("person_entered", 0, f"{p['first']} {p['last']}", acc, "business"),
            ("owner_name", 0, f"{p['first']} {p['last']}", f"{p['first']} {p['last']} Trading Ltd", "business")]
    return out


def build():
    rng = random.Random(SEED)
    people, biz = identities(rng)
    sp, sb = se.split3(range(len(people)), rng), se.split3(range(len(biz)), rng)
    pairs = {}
    for part in ("train", "validation", "test"):
        rows = []
        for i in sp[part]:
            rows += person_pairs(people[i], people, rng)
        for i in sb[part]:
            rows += business_pairs(biz[i], biz, people, rng)
        pairs[part] = rows
    return pairs


def dump(rows, t_hi, t_lo):
    lines = "".join(f"{e}\t{a}\t{t}\n" for _, _, e, a, t in rows)
    r = subprocess.run([BIN, str(t_hi), str(t_lo)], input=lines, capture_output=True, text=True, check=True)
    out = [line.split("\t") for line in r.stdout.splitlines()]
    assert len(out) == len(rows)
    return [(float(s), v) for s, v in out]


def fit(rows):
    """T_HI, T_LO on validation, as pre-registered. A type mismatch shows as close_match at thresholds (0, 0)."""
    sc = [(s, v != "match", y) for (s, v), (_, y, *_) in zip(dump(rows, 0, 0), rows)]
    cands = sorted({s for s, _, _ in sc})

    def prec(t):
        m = [y for s, mis, y in sc if s >= t and not mis]
        return sum(m) / len(m) if m else 1.0
    ok, t_hi = True, cands[-1]
    for t in reversed(cands):  # lowest t with precision >= 0.995 at t and every higher candidate
        ok = ok and prec(t) >= 0.995
        if ok:
            t_hi = t
    same = [s for s, _, y in sc if y]
    t_lo = max(t for t in cands if t <= t_hi and sum(s >= t for s in same) / len(same) >= 0.97)
    return t_hi, t_lo


def report(rows, verdicts):
    labelled = [(y, v) for (_, y, *_), (_, v) in zip(rows, verdicts)]
    m = [y for y, v in labelled if v == "match"]
    same = [v for y, v in labelled if y]
    diff = [v for y, v in labelled if not y]
    cats = {}
    for (c, y, *_), (_, v) in zip(rows, verdicts):
        d = cats.setdefault(("same:" if y else "different:") + c, {"match": 0, "close_match": 0, "no_match": 0})
        d[v] += 1
    return {"pairs": len(rows), "same_pairs": len(same), "different_pairs": len(diff),
            "match_precision": round(sum(m) / len(m), 4) if m else None,
            "different_labelled_match": sum(v == "match" for v in diff),
            "same_match_or_close": round(sum(v != "no_match" for v in same) / len(same), 4),
            "same_match": round(sum(v == "match" for v in same) / len(same), 4),
            "different_close_match_share": round(sum(v == "close_match" for v in diff) / len(diff), 4),
            "verdicts_by_category": dict(sorted(cats.items()))}


def main():
    pairs = build()
    t_hi, t_lo = fit(pairs["validation"])
    res = {part: report(rows, dump(rows, t_hi, t_lo)) for part, rows in pairs.items()}
    test = res["test"]
    checks = {"a_test_match_precision_ge_0.99": (test["match_precision"] or 0) >= 0.99,
              "b_test_same_match_or_close_ge_0.95": test["same_match_or_close"] >= 0.95}
    out = {"rule": "ADOPTION RULE in research/payee_check_eval.py (pre-registered)", "date": date.today().isoformat(),
           "scorer": "Rust sanctions score_v2 (crates/rusetect/src/sanctions.rs) via crates/rusetect/src/payee.rs and the payee_dump example; no Python mirror",
           "seed": SEED, "people": N_PEOPLE, "businesses": N_BUSINESSES,
           "threshold_rule": "fitted on validation only: T_HI lowest with match precision >= 0.995 (and above); T_LO highest <= T_HI with same-pair coverage >= 0.97",
           "t_hi": t_hi, "t_lo": t_lo, **res, "checks": checks, "adopted": all(checks.values())}
    with open(OUT, "w") as f:
        json.dump(out, f, indent=2)
        f.write("\n")
    json.dump(out, sys.stdout, indent=2)


if __name__ == "__main__":
    main()
