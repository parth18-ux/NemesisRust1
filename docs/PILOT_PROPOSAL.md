# Shadow-mode pilot proposal

**For:** the fraud-risk team of [BANK]. **From:** Parth Sharma, NEMESIS / RuseTect. **Status:** offer, for discussion.

## The offer

Run RuseTect beside your current fraud and mule controls for about eight weeks, in **shadow mode**: it scores a copy
of your payment stream and writes its decisions to a log your team reads. It never touches a payment, a customer or a
case. At the end you get a measured answer, on your own data, to one question: does it find coordinated payment fraud
and mule accounts that your current system misses, at an alert volume your team can absorb?

## Scope

- **Use case:** coordinated payment fraud and mule-account detection (fan-in to collector accounts, rapid
  pass-through, split payments, payment-format switching).
- **In scope:** one payment rail or channel chosen with you; payment scoring, the account graph checks and the
  analyst evidence pack. Optional: message context (with customer consent) and the PRAMAAN account-takeover check.
- **Out of scope:** any live action on payments or customers; the consortium registry (it needs more than one bank and
  a governance agreement); sending any data outside your network.

## What the bank provides

As in `docs/PILOT.md`: a payment stream (timestamp, payer and payee as account or salted hash, amount, currency, rail),
confirmed fraud, mule or STR outcomes for the same period with the date each was known, at least three months of
history before the pilot window, the number of reviews per day your team can absorb, and a rough cost of a missed
fraud against a needless review. One technical contact and one fraud-risk owner (the pilot champion).

## Integration

- RuseTect runs as one binary (`rusetect-server`, or the Redis workers in `deploy/`) **inside your network**. It
  refuses to start without your analyst keys and signing secrets.
- Your system pushes each payment to the signed webhook `POST /api/v2/events/payment`: HMAC-SHA256 over the timestamp
  and raw body, a 5-minute replay window and idempotent `event_id` retries (`docs/live.md`, with curl, Python and Node
  examples). Historical months are replayed through the same route in time order.
- Security posture and open gaps are listed in `docs/THREAT_MODEL.md`; please have your security team review it before
  the pilot starts.

## Success metrics, agreed before any result is read

Written into the pilot script and signed off by both sides in week 2, before labels are joined (`docs/PILOT.md`, step 2):

1. Precision of the hold and block bands at your agreed alert budget, with bootstrap intervals.
2. Recall against your confirmed fraud and mule outcomes, compared with your current system on the same period.
3. Fraud or mule cases RuseTect flagged that your current system did not, and how much earlier.
4. Alerts per day, and analyst minutes per alert with the evidence pack.
5. Latency and throughput on your hardware, measured the way `results/e2e_bench.json` was.

Thresholds are fitted only on the earlier labelled months; the later months are measured once.

## Timeline

| Weeks | Step |
|---|---|
| 0 | NDA and data terms signed; security review of `docs/THREAT_MODEL.md` |
| 1 to 2 | Install inside your network; replay history; freeze the success rule |
| 3 to 8 | Live shadow scoring; weekly comparison as labels arrive |
| 9 | Joint read-out: adopt, retrain on your data under the same rule, or stop |

## What NEMESIS does not claim

- **Validation is synthetic only.** Every payment model was trained and tested on synthetic data (IBM AMLworld,
  SAML-D, AMLgentex and others; `research/out/hardened_transfer.json`, `results/holdout_eval.json`,
  `docs/MODEL_CARD.md`). Nothing has been measured on a bank's real payments; this pilot is how to do that.
- **No certification.** No external penetration test, audit, or regulatory certification has been done.
- **No customers yet.** You would be the first organisation to evaluate it.
- Performance figures in `results/e2e_bench.json` are from one container where load generator, server and Redis share the same CPUs, not a production deployment.
- It does not decide on its own: a block needs two different analysts.

## Commercial terms

**Price:** [PRICE PLACEHOLDER: to be set by Parth Sharma]. Data stays with the bank and is deleted or returned at the
end of the pilot as the data terms specify.
