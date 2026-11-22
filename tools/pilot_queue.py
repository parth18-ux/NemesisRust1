"""Compare chronological review queues at a frozen daily capacity."""

import argparse
import hashlib
import json
import os
import sys
from datetime import date
from statistics import median

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from pilot_evaluate import finite_score, parse_outcome, read_csv, timestamp


SCORE_COLUMNS = ("served", "cascade", "amount_usd", "velocity")
METHODS = (*SCORE_COLUMNS, "random")
PAYMENT_COLUMNS = ("event_id", "timestamp", "review_group", *SCORE_COLUMNS)


def payment(row):
    group = row["review_group"]
    if not group or not group.strip():
        raise ValueError("empty review_group")
    return timestamp(row["timestamp"]), group, tuple(finite_score(row[name]) for name in SCORE_COLUMNS)


def random_score(seed, event_id):
    key = f"{seed}:{event_id}".encode()
    return (int.from_bytes(hashlib.sha256(key).digest()[:8], "big") >> 11) / (1 << 53)


def load_events(path, start, end):
    payments = read_csv(path, PAYMENT_COLUMNS, payment)
    return sorted(
        ((event_id, record) for event_id, record in payments.items()
         if start <= record[0].date() < end),
        key=lambda item: (item[1][0], item[0]),
    )


def scores_for(event_id, scores, seed):
    return zip(METHODS, (*scores, random_score(seed, event_id)))


def select(events, thresholds, budget, seed):
    queues = {name: {} for name in METHODS}
    groups = {name: set() for name in METHODS}
    usage = {name: {} for name in METHODS}
    for event_id, (paid_at, group, scores) in events:
        day = paid_at.date().isoformat()
        for name, score in scores_for(event_id, scores, seed):
            used = usage[name].setdefault(day, 0)
            if used >= budget or group in groups[name] or score < thresholds[name]:
                continue
            queues[name][event_id] = paid_at
            groups[name].add(group)
            usage[name][day] += 1
    return queues, usage


def summarize(queues, usage, events, outcomes, as_of):
    first_payment = {}
    labels = {}
    pending = 0
    for event_id, (paid_at, _, _) in events:
        if event_id not in outcomes:
            continue
        case, confirmed_at = outcomes[event_id]
        if confirmed_at < paid_at:
            raise ValueError("a holdout event has a confirmation before its payment timestamp")
        if confirmed_at > as_of:
            pending += 1
            continue
        labels[event_id] = case
        first_payment.setdefault(case, paid_at)

    methods = {}
    cases_by_method = {}
    for name, queue in queues.items():
        first_review = {}
        linked_reviews = 0
        for event_id, paid_at in queue.items():
            if event_id in labels:
                linked_reviews += 1
                first_review.setdefault(labels[event_id], paid_at)
        cases_by_method[name] = set(first_review)
        delays = [(paid_at - first_payment[case]).total_seconds() / 60
                  for case, paid_at in first_review.items()]
        methods[name] = {
            "reviews": len(queue),
            "reviews_by_day": usage[name],
            "confirmed_cases_found": len(first_review),
            "confirmed_event_yield": linked_reviews / len(queue) if queue else None,
            "confirmed_case_recall": len(first_review) / len(first_payment) if first_payment else None,
            "median_minutes_to_first_review": median(delays) if delays else None,
        }

    served = cases_by_method["served"]
    for name, found in cases_by_method.items():
        methods[name]["confirmed_cases_also_found_by_served"] = len(found & served)
    return {
        "payments": len(events),
        "confirmed_cases": len(first_payment),
        "events_confirmed_after_snapshot": pending,
        "methods": methods,
    }


def read_policy(path):
    with open(path, encoding="utf-8") as stream:
        config = json.load(stream)
    if not isinstance(config, dict):
        raise ValueError("expected a JSON policy object")
    validation_end = date.fromisoformat(config["validation_end_utc_exclusive"])
    thresholds = config["thresholds"]
    if not isinstance(thresholds, dict) or set(thresholds) != set(METHODS):
        raise ValueError(f"require a frozen threshold for each method: {', '.join(METHODS)}")
    thresholds = {name: finite_score(value) for name, value in thresholds.items()}
    seed = config["random_seed"]
    if type(seed) is not int or not 0 <= thresholds["random"] <= 1:
        raise ValueError("require an integer random_seed and a random threshold between 0 and 1")
    budget = config.get("reviews_per_utc_day")
    if type(budget) is not int or budget < 1:
        raise ValueError("require a positive integer reviews_per_utc_day")
    return validation_end, thresholds, seed, budget


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("payments", help="CSV with event_id,timestamp,review_group,served,cascade,amount_usd,velocity")
    parser.add_argument("outcomes", help="CSV with event_id,case_id,confirmed_at")
    parser.add_argument("policy", help="JSON with validation_end_utc_exclusive, reviews_per_utc_day, thresholds, and random_seed")
    parser.add_argument("--start", required=True, type=date.fromisoformat)
    parser.add_argument("--end", required=True, type=date.fromisoformat)
    parser.add_argument("--as-of", required=True, type=timestamp)
    parser.add_argument("--reviews-per-day", required=True, type=int)
    args = parser.parse_args()
    if args.start >= args.end or args.as_of.date() < args.end or args.reviews_per_day < 1:
        parser.error("require start < end, a label snapshot at or after end, and a positive review budget")

    try:
        validation_end, thresholds, seed, frozen_budget = read_policy(args.policy)
        if validation_end > args.start:
            raise ValueError("validation overlaps holdout")
        if frozen_budget != args.reviews_per_day:
            raise ValueError("review budget differs from the frozen policy")
        events = load_events(args.payments, args.start, args.end)
        if not events:
            raise ValueError("no payments in the holdout")
        queues, usage = select(events, thresholds, args.reviews_per_day, seed)
        outcomes = read_csv(args.outcomes, ("event_id", "case_id", "confirmed_at"), parse_outcome)
        report = summarize(queues, usage, events, outcomes, args.as_of)
    except (OSError, KeyError, TypeError, ValueError) as exc:
        parser.exit(2, f"pilot queue: {exc}\n")
    print(json.dumps({
        "holdout_start_utc": args.start.isoformat(),
        "holdout_end_utc_exclusive": args.end.isoformat(),
        "labels_as_of_utc": args.as_of.isoformat(),
        "validation_end_utc_exclusive": validation_end.isoformat(),
        "reviews_per_utc_day": args.reviews_per_day,
        "thresholds": thresholds,
        "random_seed": seed,
        **report,
    }, indent=2))


if __name__ == "__main__":
    main()
