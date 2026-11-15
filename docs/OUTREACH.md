# Outreach kit

A template email and a checklist for the user to adapt. **This is not legal advice.** Have a lawyer qualified in
Indian law review any NDA, data terms or DPDP Act position before signing or receiving data.

## Email template

> **Subject:** Eight-week shadow test of mule-account detection on your own data
>
> Dear [NAME],
>
> I am building RuseTect, an open-source engine that scores payments for coordinated fraud and mule accounts
> (collector accounts with many payers, rapid pass-through, split payments) and gives analysts a short, explained
> evidence pack for each alert.
>
> I would like to propose a **shadow-mode** evaluation: it runs inside your network on a copy of one payment stream,
> never acts on a payment or customer, and at the end we measure, against your confirmed outcomes, whether it finds
> cases your current controls miss at an alert volume your team can absorb. The success measures are agreed with you
> in writing before any result is read.
>
> To be clear about where it stands: it has been validated on synthetic data only, it has no certification or external
> security test yet, and you would be the first organisation to evaluate it. The one-page proposal, threat model and
> integration guide are attached.
>
> Could we have a 30-minute call with someone on your fraud-risk team in the next two weeks?
>
> Regards,
> Parth Sharma
> [PHONE] · [EMAIL] · github.com/parth18-ux/NemesisRust1

Attach: `docs/PILOT_PROPOSAL.md`, `docs/THREAT_MODEL.md`, `docs/live.md`. Before sending, check every claim against
those files; do not add accuracy figures that are not in `results/` or `research/out/` with their file named.

## Before any data is shared: checklist

### NDA

- [ ] Mutual NDA, covering the bank's data, its fraud controls and RuseTect's code and models.
- [ ] Term, permitted purpose (the pilot only), named people with access, return or destruction at the end.
- [ ] Who owns improvements and models retrained on the bank's data (default: retrained models stay with the bank).
- [ ] Publication: nothing about the bank or its results is published without written consent.

### Data terms

- [ ] Exact fields shared (`docs/PILOT.md`, "What the bank provides"); account identifiers as salted hashes where
      possible, the salt held only by the bank.
- [ ] Where processing happens: inside the bank's network; no copy to public AI services, the Vercel demo, or any
      third-party store (`docs/THREAT_MODEL.md`).
- [ ] Retention period and deletion method, with a written deletion confirmation at the end.
- [ ] Access control: named people, analyst keys, audit log kept.
- [ ] Breach notification: who tells whom, how fast.
- [ ] Labels: which outcomes (fraud, mule, STR), and the date each was known.
- [ ] STR material: confirm what may be shared at all; sharing suspicion reports can amount to tipping off.

### India: Digital Personal Data Protection Act, 2023 (and its Rules)

- [ ] Role: the bank is the data fiduciary; RuseTect (or Parth Sharma) would be a data processor acting only on the
      bank's written contract.
- [ ] Lawful basis the bank relies on for using payment data for fraud prevention, and whether any customer notice or
      consent is needed for the pilot (message context needs explicit customer consent in RuseTect regardless).
- [ ] Purpose limitation and data minimisation: only the fields the pilot needs.
- [ ] Security safeguards the processor must keep, and breach reporting duties.
- [ ] Erasure when the purpose is served; processor's obligations on deletion.
- [ ] Children's data and any significant data fiduciary obligations, if they apply to the bank.
- [ ] Cross-border transfer: data stays in India (and inside the bank).

### Banking regulation (check with the bank's compliance team)

- [ ] RBI rules on outsourcing, IT and cyber security, and payment data storage in India.
- [ ] The bank's own vendor risk and information security assessment.
- [ ] Whether the bank's regulator must be told of the pilot.
