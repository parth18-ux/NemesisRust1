"""Compare a pilot sender replay log and incumbent scores under the same daily review budget."""

import argparse
import csv
import heapq
import json
import math
from collections import defaultdict
from datetime import date, datetime, timezone


ACTION_PRIORITY = {"allow": 0, "verify": 1, "hold": 2, "block": 3}


def timestamp(value):
    # The webhook accepts timezone-naive timestamps. Pilot exports use UTC.
    if not isinstance(value, str) or len(value) <= 10:
        raise ValueError("expected an ISO timestamp with time")
    parsed = datetime.fromisoformat(value)
    return parsed.replace(tzinfo=timezone.utc) if parsed.tzinfo is None else parsed.astimezone(timezone.utc)


def finite_score(value):
    if value is None or isinstance(value, bool):
        raise ValueError("expected a numeric score")
    number = float(value)
    if not math.isfinite(number):
        raise ValueError("expected a finite score")
    return number


def read_replay(path):
    events = {}
    with open(path, encoding="utf-8") as stream:
        for line_number, line in enumerate(stream, 1):
            if not line.strip():
                continue
            try:
                row = json.loads(line)
                if not isinstance(row, dict) or row.get("status") != 200 or "replayed" not in row:
                    raise ValueError("replay contains an unsuccessful send")
                event_id = row["event_id"]
                if not isinstance(event_id, str) or not event_id.strip():
                    raise ValueError("missing event_id")
                if "risk" not in row:
                    raise ValueError("replay log is missing risk; regenerate it with the updated sender")
                risk = row["risk"]
                paid_at = row["timestamp"]
                if isinstance(paid_at, str):
                    paid_at = paid_at.replace("/", "-")  # accepted by the webhook
                record = (timestamp(paid_at), ACTION_PRIORITY[row["action"]],
                          -math.inf if risk is None else finite_score(risk))
                if event_id in events and events[event_id] != record:
                    raise ValueError("duplicate event_id has conflicting replay results")
                events[event_id] = record
            except (KeyError, TypeError, ValueError) as exc:
                raise ValueError(f"replay line {line_number}: {exc}") from exc
    return events


def read_csv(path, columns, parse):
    values = {}
    with open(path, newline="", encoding="utf-8-sig") as stream:
        reader = csv.DictReader(stream)
        if not reader.fieldnames or not set(columns).issubset(reader.fieldnames):
            raise ValueError(f"expected columns {', '.join(columns)} in {path}")
        for row in reader:
            try:
                event_id = row["event_id"]
                if not event_id or not event_id.strip() or event_id in values:
                    raise ValueError("empty or duplicate event_id")
                values[event_id] = parse(row)
            except (TypeError, ValueError) as exc:
                raise ValueError(f"{path}, line {reader.line_num}: {exc}") from exc
    return values


def parse_outcome(row):
    case_id = row["case_id"]
    if not case_id or not case_id.strip():
        raise ValueError("empty case_id")
    return case_id, timestamp(row["confirmed_at"])


def summarize(selected, labels, total_cases):
    hits = [labels[event_id] for event_id in selected if event_id in labels]
    found = set(hits)
    return {
        "reviews": len(selected),
        "reviews_linked_to_confirmed_cases": len(hits),
        "confirmed_cases_found": len(found),
        "confirmed_event_yield": len(hits) / len(selected) if selected else None,
        "confirmed_case_recall": len(found) / total_cases if total_cases else None,
    }, found


def evaluate(replay, incumbent, outcomes, start, end, as_of, budget):
    days = defaultdict(list)
    for event_id, (paid_at, _, _) in replay.items():
        if start <= paid_at.date() < end:
            days[paid_at.date()].append(event_id)
    if not days:
        raise ValueError("no replay events in the holdout")
    holdout = {event_id for events in days.values() for event_id in events}
    missing = holdout.difference(incumbent)
    if missing:
        raise ValueError(f"incumbent scores missing for {len(missing)} holdout events")

    labels = {}
    pending = 0
    for event_id in holdout.intersection(outcomes):
        case_id, confirmed_at = outcomes[event_id]
        if confirmed_at < replay[event_id][0]:
            raise ValueError("a holdout event has a confirmation before its payment timestamp")
        if confirmed_at <= as_of:
            labels[event_id] = case_id
        else:
            pending += 1

    # Both rankings break ties by earliest payment, then event ID.
    current, comparison = [], []
    for day in sorted(days):
        events = days[day]
        current.extend(heapq.nsmallest(budget, events, key=lambda eid: (
            -replay[eid][1], -replay[eid][2], replay[eid][0], eid)))
        comparison.extend(heapq.nsmallest(budget, events, key=lambda eid: (
            -incumbent[eid], replay[eid][0], eid)))

    case_count = len(set(labels.values()))
    rusetect, current_cases = summarize(current, labels, case_count)
    baseline, incumbent_cases = summarize(comparison, labels, case_count)
    return {
        "holdout_start_utc": start.isoformat(),
        "holdout_end_utc_exclusive": end.isoformat(),
        "labels_as_of_utc": as_of.isoformat(),
        "reviews_per_utc_day": budget,
        "days_with_payments": len(days),
        "payments": len(holdout),
        "confirmed_cases": case_count,
        "events_confirmed_after_snapshot": pending,
        "rusetect": rusetect,
        "incumbent": baseline,
        "cases_found_by_both": len(current_cases & incumbent_cases),
        "cases_found_only_by_rusetect": len(current_cases - incumbent_cases),
        "cases_found_only_by_incumbent": len(incumbent_cases - current_cases),
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("replay", help="JSONL from pilot_sender.py --log")
    parser.add_argument("incumbent", help="CSV with event_id,score")
    parser.add_argument("outcomes", help="CSV with event_id,case_id,confirmed_at")
    parser.add_argument("--start", required=True, type=date.fromisoformat, help="first holdout day in UTC")
    parser.add_argument("--end", required=True, type=date.fromisoformat, help="first day after the holdout in UTC")
    parser.add_argument("--as-of", required=True, type=timestamp, help="label snapshot timestamp")
    parser.add_argument("--reviews-per-day", required=True, type=int)
    args = parser.parse_args()
    if args.start >= args.end or args.as_of.date() < args.end or args.reviews_per_day < 1:
        parser.error("require start < end, a label snapshot at or after end, and a positive review budget")
    try:
        replay = read_replay(args.replay)
        incumbent = read_csv(args.incumbent, ("event_id", "score"), lambda row: finite_score(row["score"]))
        outcomes = read_csv(args.outcomes, ("event_id", "case_id", "confirmed_at"), parse_outcome)
        report = evaluate(replay, incumbent, outcomes, args.start, args.end, args.as_of, args.reviews_per_day)
    except (OSError, ValueError) as exc:
        parser.exit(2, f"pilot evaluation: {exc}\n")
    print(json.dumps(report, indent=2))


if __name__ == "__main__":
    main()
