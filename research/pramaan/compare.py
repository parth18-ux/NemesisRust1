"""Is PRAMAAN's account-takeover engine an improvement over RuseTect's identity rule? Same traffic, same assumptions.

    python3 -I research/pramaan/compare.py <reference.jsonl from research/pramaan/reference.py>

Traffic: PRAMAAN's simulator (simulated customers and attacks). Both designs are scored on the same requests, and a
challenge succeeds or fails with PRAMAAN's assumed efficacies (ml/auth/policy.py EFFICACY, GENUINE_PASS), drawn from
one seeded stream so both designs face the same luck.
  pramaan    the ported engine (crates/rusetect/src/pramaan.rs; its decisions equal PRAMAAN's Python engine, which
             research/pramaan/reference.py recorded)
  rusetect   RuseTect's identity rule (crates/rusetect/src/session_signals.rs identity_check): a device never seen on
             the account within SIM_SWAP_RECENT_H of a SIM change -> no OTP, re-verify (scored as a bank call-back);
             either one alone -> step-up (scored as an SMS OTP); otherwise allow
Outcome per request: stopped (blocked, or its challenge failed) or not. Reported: attack requests stopped, genuine
requests challenged and genuine requests finally refused. Results: research/out/pramaan_eval.json
Decision (the rule recorded in docs/decision.md 8f, written down after the run, not pre-registered): adopt PRAMAAN next to
the identity rule if it stops a larger share of attack requests in both arms, without_behaviour (the conservative case)
and with_behaviour; the extra genuine challenges are the accepted cost and are reported, not gated.
    python3 -I research/pramaan/compare.py --decide   (re-derives "decision" from the saved arms, no inputs needed)
Caveat: simulated traffic and assumed efficacies, from PRAMAAN's own simulator; the comparison ranks designs, it is not
a real-world fraud rate.
"""
import json, os, sys
import numpy as np

EFFICACY = {"sms_otp": {"ato_new_device": 0.50, "sim_swap": 0.00, "bot": 0.90, "remote_access": 0.20, "coerced": 0.05},
            "passkey": {"ato_new_device": 0.95, "sim_swap": 0.95, "bot": 0.99, "remote_access": 0.40, "coerced": 0.10},
            "bank_callback": {"ato_new_device": 0.90, "sim_swap": 0.30, "bot": 0.99, "remote_access": 0.85, "coerced": 0.60},
            "cool_off": {"ato_new_device": 0.60, "sim_swap": 0.50, "bot": 0.90, "remote_access": 0.60, "coerced": 0.50}}
GENUINE_PASS = {"sms_otp": 0.97, "passkey": 0.98, "bank_callback": 0.90, "cool_off": 0.85}
SIM_SWAP_RECENT_H = float(sys.argv[2]) if len(sys.argv) > 2 else 72.0
ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))


def outcome(decision, ctype, lab, u):
    if decision == "allow":
        return False, False
    if decision == "block":
        return True, False
    if lab["is_attack"]:
        return u < EFFICACY[ctype][lab["scenario"]], True
    return u >= GENUINE_PASS[ctype], True


RULE = ("adopt PRAMAAN next to the identity rule if it stops a larger share of attack requests than the identity rule in both "
        "arms (without_behaviour, the conservative case, and with_behaviour); extra genuine challenges are reported, not gated")


def decide(allr):
    checks = {}
    for arm in ("without_behaviour", "with_behaviour"):
        a, r = allr[arm]["pramaan"], allr[arm]["rusetect"]
        checks[arm] = {"attack_requests_stopped": [a["attack_requests_stopped"], r["attack_requests_stopped"]],
                       "genuine_challenged": [a["genuine_challenged"], r["genuine_challenged"]],
                       "pass": a["attack_requests_stopped"] > r["attack_requests_stopped"]}
    return {"adopted": all(c["pass"] for c in checks.values()), "rule": RULE,
            "rule_source": "docs/decision.md 8f (recorded after the run, not pre-registered)",
            "checks_pramaan_vs_rusetect": checks}


def main():
    out = os.path.join(ROOT, "research", "out", "pramaan_eval.json")
    if sys.argv[1] == "--decide":
        allr = json.load(open(out))
        allr["decision"] = decide(allr)
        json.dump(allr, open(out, "w"), indent=1)
        print(json.dumps(allr["decision"], indent=1))
        return
    rng = np.random.default_rng(11)
    rows = [json.loads(l) for l in open(sys.argv[1])]
    S = {a: {"attack": [], "genuine_challenged": [], "genuine_refused": [], "by_scenario": {}} for a in ("pramaan", "rusetect")}
    for r in rows:
        lab, e, req = r["label"], r["expected"], r["request"]
        u = float(rng.random())
        dev = req.get("device") or {}
        new_dev = e["features"]["device_new"] == 1.0
        sim_h = dev.get("sim_change_hours")
        swap = sim_h is not None and sim_h <= SIM_SWAP_RECENT_H
        rd = ("challenge", "bank_callback") if (new_dev and swap) else ("challenge", "sms_otp") if (new_dev or swap) else ("allow", None)
        for arm, (dec, ct) in (("pramaan", (e["decision"], e["challenge_type"])), ("rusetect", rd)):
            stopped, challenged = outcome(dec, ct, lab, u)
            s = S[arm]
            if lab["is_attack"]:
                s["attack"].append(stopped)
                s["by_scenario"].setdefault(lab["scenario"], []).append(stopped)
            else:
                s["genuine_challenged"].append(challenged or dec == "block")
                s["genuine_refused"].append(stopped)
    R = {"traffic": {"requests": len(rows), "attack_requests": sum(r["label"]["is_attack"] for r in rows)},
         "sim_swap_recent_h": SIM_SWAP_RECENT_H}
    for arm, s in S.items():
        R[arm] = {"attack_requests_stopped": round(float(np.mean(s["attack"])), 4), "genuine_challenged": round(float(np.mean(s["genuine_challenged"])), 4),
                  "genuine_refused": round(float(np.mean(s["genuine_refused"])), 5),
                  "stopped_by_scenario": {k: round(float(np.mean(v)), 3) for k, v in sorted(s["by_scenario"].items())}}
    key = os.environ.get("ARM_NAME", "with_behaviour")
    allr = json.load(open(out)) if os.path.exists(out) else {}
    allr[key] = R
    allr["note"] = ("PRAMAAN's simulator and assumed challenge efficacies; with_behaviour draws touch and typing deviations "
                    "(attackers far from the owner), without_behaviour has none, the conservative case")
    if "with_behaviour" in allr and "without_behaviour" in allr:
        allr["decision"] = decide(allr)
    json.dump(allr, open(out, "w"), indent=1)
    print(json.dumps(R, indent=1))


if __name__ == "__main__":
    main()
