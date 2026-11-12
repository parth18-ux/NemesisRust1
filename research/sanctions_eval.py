"""Sanctions name screening: fit the hit threshold on validation, report test once.

    cargo build --release -p rusetect --example sanctions_dump
    python3 -I research/sanctions_eval.py

Pre-registered rule (committed before the first run): every list entry yields true variants (exact, reordered tokens,
honorific and punctuation noise, one typo, two typos, a transliteration rewrite, a dropped middle token). A positive pair
is (variant, its own entry); a negative pair is (variant, the highest-scoring other entry) and (common name not on the
list, the highest-scoring entry). List entries are split 60/20/20 into train/validation/test (variants stay with their
entry), common names likewise; seed 0. The threshold is the score on validation with recall >= 0.95 and the highest
precision (ties: the higher threshold). Test precision, recall and F1 are reported once at that threshold; train is not
used for fitting (the scorer has no learned parameters) and is reported for completeness.
Scores come from the served Rust scorer (crates/rusetect/src/sanctions.rs) via the sanctions_dump example.
Results: research/out/sanctions_eval.json (also read by the API for the threshold).
"""
import csv, json, os, random, subprocess, sys
from datetime import date

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
LIST = os.path.join(ROOT, "service", "models", "sanctions_sdn.csv")
BIN = os.path.join(ROOT, "target", "release", "examples", "sanctions_dump")
OUT = os.path.join(ROOT, "research", "out", "sanctions_eval.json")
SEED = 0
FIRST = ["John", "Maria", "Wei", "Priya", "Ahmed", "Olga", "James", "Fatima", "Carlos", "Anna", "Ivan", "Mei", "David",
         "Aisha", "Pedro", "Sara", "Ali", "Elena", "Rahul", "Grace", "Peter", "Yuki", "Hassan", "Laura", "Tomas"]
LAST = ["Smith", "Garcia", "Zhang", "Sharma", "Khan", "Ivanova", "Brown", "Ali", "Silva", "Muller", "Petrov", "Wang",
        "Johnson", "Hassan", "Santos", "Novak", "Kim", "Lopez", "Patel", "Nguyen", "Cohen", "Okafor", "Rossi", "Tanaka"]
TRANSLIT = [("ph", "f"), ("ou", "u"), ("ee", "i"), ("kh", "h"), ("y", "i"), ("mm", "m"), ("v", "w"), ("c", "k"),
            ("z", "s"), ("sh", "ch"), ("oo", "u"), ("ei", "ai"), ("o", "u"), ("a", "e")]


def typo(name, rng):
    toks = name.split()
    i = rng.choice([k for k, t in enumerate(toks) if len(t) >= 4] or [0])
    t = list(toks[i])
    j = rng.randrange(1, len(t) - 1) if len(t) > 2 else 0
    op = rng.choice(["delete", "swap", "substitute", "insert"])
    if op == "delete":
        del t[j]
    elif op == "swap" and j + 1 < len(t):
        t[j], t[j + 1] = t[j + 1], t[j]
    elif op == "insert":
        t.insert(j, rng.choice("aeiou"))
    else:
        t[j] = rng.choice([c for c in "abcdefghijklmnopqrstuvwxyz" if c != t[j].lower()])
    toks[i] = "".join(t)
    return " ".join(toks)


def translit(name, rng):
    low = name.lower()
    rules = [r for r in TRANSLIT if r[0] in low]
    for a, b in rng.sample(rules, min(2, len(rules))):
        low = low.replace(a, b, 1)
    return low.title()


def variants(name, rng):
    toks = name.split()
    out = {"exact": name, "reordered": " ".join(toks[1:] + toks[:1]) if len(toks) > 1 else name,
           "noise": "Mr. " + name.upper().replace(" ", ", ", 1), "typo1": typo(name, rng),
           "typo2": typo(typo(name, rng), rng), "translit": translit(name, rng)}
    if len(toks) >= 3:
        out["dropped"] = " ".join(toks[:1] + toks[2:])
    return out


def scores(names):
    r = subprocess.run([BIN, LIST], input="\n".join(names) + "\n", capture_output=True, text=True, check=True)
    return [[float(x) for x in line.split("\t")] for line in r.stdout.splitlines()]


def split3(items, rng):
    items = list(items)
    rng.shuffle(items)
    a, b = int(0.6 * len(items)), int(0.8 * len(items))
    return {"train": items[:a], "validation": items[a:b], "test": items[b:]}


def metrics(rows, t):
    tp = sum(1 for s, y in rows if y and s >= t)
    fp = sum(1 for s, y in rows if not y and s >= t)
    fn = sum(1 for s, y in rows if y and s < t)
    p = tp / (tp + fp) if tp + fp else 0.0
    r = tp / (tp + fn) if tp + fn else 0.0
    return {"precision": round(p, 4), "recall": round(r, 4), "f1": round(2 * p * r / (p + r), 4) if p + r else 0.0,
            "tp": tp, "fp": fp, "fn": fn, "positives": tp + fn, "negatives": sum(1 for _, y in rows if not y)}


def main():
    rng = random.Random(SEED)
    with open(LIST, newline="") as f:
        entries = list(csv.DictReader(f))
    common = sorted({f"{a} {b}" for a in FIRST for b in LAST})
    common = rng.sample(common, 300)
    ent_split, com_split = split3(range(len(entries)), rng), split3(common, rng)

    queries = []  # (split, kind, name, entry index or None)
    for part, idx in ent_split.items():
        for i in idx:
            for kind, v in variants(entries[i]["name"], rng).items():
                queries.append((part, kind, v, i))
    for part, names in com_split.items():
        queries += [(part, "common", n, None) for n in names]
    sc = scores([q[2] for q in queries])

    rows = {p: [] for p in ent_split}
    for (part, kind, _, i), s in zip(queries, sc):
        if i is None:
            rows[part].append((max(s), 0))
        else:
            rows[part].append((s[i], 1))
            rows[part].append((max(x for k, x in enumerate(s) if k != i), 0))

    val = rows["validation"]
    ok = [t for t in sorted({s for s, _ in val}) if metrics(val, t)["recall"] >= 0.95]
    t = max(ok, key=lambda t: (metrics(val, t)["precision"], t))
    by_kind = {}
    for (part, kind, _, i), s in zip(queries, sc):
        if part == "test" and i is not None:
            by_kind.setdefault(kind, []).append(s[i] >= t)
    res = {"threshold": round(t, 6), "rule": "highest validation precision with validation recall >= 0.95",
           "list": os.path.relpath(LIST, ROOT), "list_entries": len(entries),
           "list_source": "sample of fictitious test entities (program FICTITIOUS-TEST); the OFAC SDN download was "
                          "blocked by the network here, so the real list must be loaded in deployment",
           "date": date.today().isoformat(), "seed": SEED,
           "validation": metrics(val, t), "test": metrics(rows["test"], t), "train": metrics(rows["train"], t),
           "test_recall_by_variant": {k: round(sum(v) / len(v), 4) for k, v in sorted(by_kind.items())}}
    os.makedirs(os.path.dirname(OUT), exist_ok=True)
    with open(OUT, "w") as f:
        json.dump(res, f, indent=2)
        f.write("\n")
    json.dump(res, sys.stdout, indent=2)


if __name__ == "__main__":
    main()
