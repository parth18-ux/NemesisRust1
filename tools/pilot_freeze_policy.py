"""Freeze chronological review thresholds on a preceding, unlabeled validation window."""

import argparse
import json
import os
import sys
from datetime import date

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from pilot_queue import METHODS, load_events, scores_for, select


def freeze_thresholds(events, target, seed):
    peaks = {name: {} for name in METHODS}
    for event_id, (_, group, scores) in events:
        for name, score in scores_for(event_id, scores, seed):
            if group not in peaks[name] or score > peaks[name][group]:
                peaks[name][group] = score

    thresholds = {}
    for name, groups in peaks.items():
        rank = min(target, len(groups))
        thresholds[name] = sorted(groups.values(), reverse=True)[rank - 1]
    return thresholds


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("payments", help="decision-time payments CSV containing the validation interval")
    parser.add_argument("--start", required=True, type=date.fromisoformat)
    parser.add_argument("--end", required=True, type=date.fromisoformat)
    parser.add_argument("--reviews-per-day", required=True, type=int)
    parser.add_argument("--random-seed", required=True, type=int)
    args = parser.parse_args()
    if args.start >= args.end or args.reviews_per_day < 1:
        parser.error("require start < end and a positive review budget")

    try:
        events = load_events(args.payments, args.start, args.end)
        if not events:
            raise ValueError("no payments in the validation window")
        days = (args.end - args.start).days
        thresholds = freeze_thresholds(events, days * args.reviews_per_day, args.random_seed)
        queues, usage = select(events, thresholds, args.reviews_per_day, args.random_seed)
    except (OSError, KeyError, TypeError, ValueError) as exc:
        parser.exit(2, f"pilot policy: {exc}\n")

    print(json.dumps({
        "validation_start_utc": args.start.isoformat(),
        "validation_end_utc_exclusive": args.end.isoformat(),
        "reviews_per_utc_day": args.reviews_per_day,
        "random_seed": args.random_seed,
        "thresholds": thresholds,
        "validation": {
            "payments": len(events),
            "calendar_days": days,
            "methods": {
                name: {"reviews": len(queues[name]), "reviews_by_payment_day": usage[name]}
                for name in METHODS
            },
        },
    }, indent=2))


if __name__ == "__main__":
    main()
