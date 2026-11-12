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
