"""Replays through the served engine and the drift monitor (crates/rusetect/examples/drift_replay.rs).

    python3 -I research/drift_replay.py validation <hi_small_sorted.csv.gz>
    python3 -I research/drift_replay.py amlsim <amlsim_aml.csv>   (the converted stream of research/holdout_amlsim.py)
    (after cargo build --release -p rusetect --example drift_replay)

validation  in-distribution check: HI-Small validation (rows VAL0 to TEST0; the test part is never parsed) scored in
            order and observed by the monitor, (a) with all earlier rows as account history, as the reference was built,
            and (b) with only the 100,000 rows before validation as history, as in the console demo (about five hours).
            Window sizes SIZES are tried at once. RULE (docs/decision.md 8ag, written before this was run): the served
            default window is the smallest size with no alert check in (a); if every size has one, 10,000 stays and
            the alert rate is documented. Output: results/drift_validation.json
amlsim      PRE-REGISTERED in docs/decision.md 8ag before it was run: the AMLSim holdout stream scored in order from its
            first step, the monitor observing the test steps (576 to 719) as in research/holdout_amlsim.py. Expected
            final status at the chosen window size (results/drift_validation.json): alert. Output: results/drift_amlsim.json
Statuses are taken by the monitor itself every 10,000 observed payments once its window is full, and at the end.
"""
import hashlib, json, os, subprocess, sys
import pandas as pd

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BIN = os.path.join(ROOT, "target", "release", "examples", "drift_replay")
VAL0, TEST0, DEMO_WARM = 3_047_007, 4_062_676, 100_000
SIZES = [10_000, 50_000, 250_000]  # 250,000 is about one day of HI-Small validation traffic
TEST_STEP, BASE = 576, pd.Timestamp("2017-01-01")  # research/holdout_amlsim.py


def replay(src, out, observe, state=0, stop=None):
    subprocess.run([BIN, src, out, str(observe), str(state), str(stop or 2**63), ",".join(map(str, SIZES))], check=True)
    r = json.load(open(out))
    os.remove(out)
    return r


def main():
    mode, src = sys.argv[1:3]
    tmp = os.path.join(ROOT, "target", f"drift_{mode}.json")
    if mode == "validation":
        R = {"script": "research/drift_replay.py", "split": "HI-Small validation (rows 3,047,007 to 4,062,675); test not parsed",
             "rule": "served default window = the smallest size with no alert check with full history; else 10,000",
             "full_history": replay(src, tmp, VAL0, 0, TEST0), "demo_history": replay(src, tmp, VAL0, VAL0 - DEMO_WARM, TEST0)}
        clean = [n for n in SIZES if R["full_history"]["windows"][str(n)]["check_status"]["alert"] == 0]
        R["chosen_window"] = clean[0] if clean else 10_000
    elif mode == "amlsim":
        ts = pd.read_csv(src, usecols=["ts"]).ts
        steps = ((pd.to_datetime(ts, format="%Y/%m/%d %H:%M") - BASE).dt.days).values
        assert (steps[1:] >= steps[:-1]).all()
        first = int((steps < TEST_STEP).sum())
        r = replay(src, tmp, first)
        n = str(json.load(open(os.path.join(ROOT, "results", "drift_validation.json")))["chosen_window"])
        R = {"script": "research/drift_replay.py", "prereg": "docs/decision.md 8ag", "stream": os.path.basename(src),
             "stream_sha256": hashlib.sha256(open(src, "rb").read()).hexdigest(), "rows": int(len(ts)), "observed_from_row": first,
             "window": int(n), "expected": "alert", "result": r["windows"][n]["final"]["status"],
             "as_expected": r["windows"][n]["final"]["status"] == "alert", "replay": r}
    else:
        sys.exit(__doc__)
    json.dump(R, open(os.path.join(ROOT, "results", f"drift_{mode}.json"), "w"), indent=1)
    print(json.dumps({k: v for k, v in R.items() if k not in ("replay", "full_history", "demo_history")}, indent=1))


if __name__ == "__main__":
    main()
