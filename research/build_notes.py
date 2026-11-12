"""Study notes and quiz: what RuseTect is, how it works, what was measured. Numbers are read from research/out.

    python3 -I research/build_notes.py && node research/make_notes_pdf.js
Writes docs/notes/notes.html; the PDF goes to docs/notes/RuseTect study notes.pdf.
"""
import html, json, os

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
OUT = os.path.join(ROOT, "research", "out")


def J(name):
    p = os.path.join(OUT, name)
    return json.load(open(p)) if os.path.exists(p) else None


def f(x, d=3):
    return f"{x:.{d}f}"


def pct(x, d=0):
    return f"{x * 100:.{d}f}%"


def main():
    HD, FULL, SD, EV, U, TS = J("hardened_eval.json"), J("full_hi_eval.json"), J("structuring_defence.json"), J("evasion_suite.json"), J("unified_train.json"), J("transfer_samld.json")
    PH, AA, HT = J("pooled_hardening.json"), J("account_aggregation.json"), J("hardened_transfer.json")
    T = HD["test"]
    E = html.escape
    sections = []

    def sec(title, body):
        sections.append(f"<section><h2>{E(title)}</h2>{body}</section>")

    sec("1. What RuseTect is", """
<p>RuseTect scores bank payments for money laundering and mule activity, and screens scam messages, before money moves.
It is one Rust program (<code>rusetect-server</code>) that serves an HTTP API and web pages, plus a research pipeline in
<code>research/</code> that measures every claim. The models are gradient-boosted trees trained on synthetic bank data
(IBM AMLworld), because real labelled bank transfers are not public.</p>""")

    sec("2. How one payment is scored", """
<ol>
<li><b>Features.</b> 31 causal account-history features from earlier payments only: amounts, timing, fan-in and fan-out,
new payee, pass-through ratio, 24 hour velocity. The state is then updated.</li>
<li><b>L1 gate.</b> A small XGBoost (30 trees). Low scores exit as safe.</li>
<li><b>L2.</b> The mean of an XGBoost (400 trees) and a LightGBM (600 trees).</li>
<li><b>Pair or payer window.</b> A 24 hour noisy-OR of recent risks: 1 &minus; &prod;(1 &minus; r<sub>i</sub>). It catches one sum split into parts.</li>
<li><b>Hardening.</b> risk = max(calibrated cascade risk, calibrated format-free model). A launderer cannot lower it by changing the payment format.</li>
<li><b>Bands.</b> Verify, hold, block, with thresholds set to an alert budget on validation data.</li>
<li><b>Reasons.</b> TreeSHAP contributions of the model that set the risk, only for flagged payments.</li>
</ol>""")

    sec("3. Metrics you must know", f"""
<table>
<tr><th>Metric</th><th>Meaning</th><th>Used here?</th></tr>
<tr><td>PR-AUC</td><td>Area under precision against recall. With rare positives it shows how many alerts are real.</td><td>Main metric</td></tr>
<tr><td>ROC-AUC</td><td>Chance a random fraud scores above a random genuine payment. 0.5 is random.</td><td>Across datasets</td></tr>
<tr><td>Precision / recall</td><td>Share of alerts that are fraud / share of fraud that is alerted.</td><td>Per band</td></tr>
<tr><td>ECE</td><td>Expected calibration error: gap between predicted risk and observed rate.</td><td>Calibration</td></tr>
<tr><td>MAE</td><td>Mean absolute error of a numeric prediction: average of |y &minus; &ycirc;|.</td><td>No: fraud is yes/no</td></tr>
<tr><td>RMSE</td><td>Square root of the mean squared error; punishes large errors more than MAE.</td><td>No</td></tr>
<tr><td>R&sup2;</td><td>Share of the variance of a numeric target the model explains (1 = perfect, 0 = predicting the mean).</td><td>No</td></tr>
<tr><td>Brier score</td><td>MSE of predicted probabilities against 0/1 outcomes; the classification cousin of MSE.</td><td>Could be, for calibration</td></tr>
</table>""")

    rows = [("Cascade alone", T["served"], min(T["attack"]["served"].values()), T["bands"]["served"]["block"]),
            ("Served, hardened", T["hardened"], min(T["attack"]["hardened"].values()), T["bands"]["hardened"]["block"])]
    perf = "".join(f"<tr><td>{n}</td><td>{f(v['pr_auc'])}</td><td>{f(a)}</td><td>{pct(b['recall'], 1)} / {pct(b['precision'], 1)}</td></tr>" for n, v, a, b in rows)
    sec("4. Key results (HI-Small test)", f"""
<table><tr><th>Score</th><th>PR-AUC</th><th>PR-AUC, format attack</th><th>Block recall / precision</th></tr>{perf}</table>
<p>The full test window has {T['hardened']['rows']:,} payments and {T['hardened']['positives']:,} laundering. Without the last eight
days (a simulation tail with {pct(FULL['test']['tail']['share_of_test_laundering'])} of test laundering) the cascade scores
{f(T['without_sep11_18_tail']['served']['pr_auc'])}: the honest headline.</p>""")

    rob = ""
    if EV:
        for k, v in EV["attacks"].items():
            rob += f"<tr><td>{E(k)}</td>" + "".join(f"<td>{pct(v[m]['block'])}</td>" for m in ("cascade", "hardened", "hardened_with_pair")) + "</tr>"
    sec("5. Robustness: what an attacker can do", f"""
<p>Block recall on the HI-Small demo window when the launderer changes their own payments (research/out/evasion_suite.json):</p>
<table><tr><th>Attack</th><th>Cascade</th><th>Hardened</th><th>Hardened + pair window</th></tr>{rob}</table>
<p>{'Payer window adopted under its rule.' if AA and AA['decision']['adopt'] else 'The payer window was tested under its own rule' + (' and not adopted.' if AA else '; result pending.')}</p>""")

    gen = ""
    if HT:
        gen = "".join(f"<tr><td>{E(v['label'])}</td><td>{f(v['cascade']['roc_auc'])}</td><td>{f(v['hardened']['roc_auc'])}</td></tr>" for v in HT.values())
    sec("6. Generalisation", f"""
<p>ROC-AUC on each dataset's last 20% in time, served models trained on HI-Small (research/out/hardened_transfer.json):</p>
<table><tr><th>Dataset</th><th>Cascade</th><th>Hardened</th></tr>{gen}</table>
<p>{'Pooled second opinion adopted.' if PH and PH['decision']['adopt'] else 'Pooled second opinion: ' + ('not adopted by its rule.' if PH else 'pending.')}
Lesson: synthetic generators differ; a new bank must label some of its own data (docs/PILOT.md).</p>""")

    sec("7. Why these choices", """
<ul>
<li><b>Rust serves, Python trains.</b> Per-payment code needs speed and one binary; training needs mature libraries.</li>
<li><b>Trees, not neural networks.</b> Tabular data, rare positives, exact explanations; no evidence a GNN wins here.</li>
<li><b>Chronological splits.</b> Fraud drifts; a random split leaks the future.</li>
<li><b>Pre-registered rules.</b> Each change is adopted only if a rule written before reading test data passes.</li>
<li><b>Isotonic calibration.</b> Puts two models on one probability scale without assuming a shape.</li>
<li><b>Fail closed.</b> No keys and no demo flag: the server does not start.</li>
</ul>""")

    quiz = [
        ("Which file is the entry point of the API server?", "crates/rusetect-server/src/main.rs (api/rusetect.rs on Vercel)."),
        ("Name the three stages a payment passes before banding.", "Features, the L1 XGBoost gate, the L2 XGBoost + LightGBM mean; then the window and hardening."),
        ("Why is PR-AUC preferred over accuracy here?", "Fraud is about 0.1% of payments, so 99.9% accuracy is possible by approving everything."),
        ("What does the hardened risk protect against?", "A launderer switching payment format, which the cascade over-relies on."),
        ("What is a noisy-OR?", "1 minus the product of (1 minus each risk): the chance at least one part is bad if parts were independent."),
        ("Why are thresholds fitted on validation only?", "Fitting on test would report an optimistic number that a bank would never see."),
        ("Is MAE a sensible metric for RuseTect? Why?", "No: MAE measures numeric prediction error; fraud is a yes/no label, so ranking and calibration metrics are used."),
        ("What does R² = 0 mean?", "The model is no better than always predicting the average of the target."),
        ("Why are explanations computed only for flagged payments?", "TreeSHAP was most of the scoring time and approved payments show no reasons."),
        ("What stops a misconfigured deployment from using demo keys?", "check_config: without RUSETECT_ANALYST_KEYS the server needs RUSETECT_DEMO=1 explicitly."),
        ("What is the biggest remaining weakness?", "All payment data is synthetic; real performance is unknown until a shadow pilot on a bank's data."),
        ("Which attack is still hardest?", "Changes that keep each payment small and new, such as splitting over new mule accounts and delaying."),
    ]
    q = "".join(f"<li><p><b>{E(a)}</b></p><p class='ans'>{E(b)}</p></li>" for a, b in quiz)
    sec("8. Quiz (answers in grey)", f"<ol class='quiz'>{q}</ol>")

    page = f"""<!doctype html><html lang="en"><head><meta charset="utf-8"><title>RuseTect Study Notes</title>
<style>
body{{font:13px/1.55 Georgia,serif;color:#111;max-width:820px;margin:0 auto;padding:24px}}
h1{{font-size:30px;margin:0 0 4px}} h2{{font-size:19px;border-bottom:1px solid #ccc;padding-bottom:4px;margin-top:26px}}
table{{border-collapse:collapse;width:100%;font:12px/1.4 system-ui,sans-serif}} th,td{{border:1px solid #ddd;padding:5px 7px;text-align:left}}
th{{background:#f3f3ef}} code{{font:12px monospace;background:#f3f3ef;padding:0 3px}} .ans{{color:#666}} section{{break-inside:avoid-page}}
.quiz li{{margin-bottom:6px}} .quiz p{{margin:2px 0}}
</style></head><body>
<h1>RuseTect study notes</h1><p>Generated by <code>research/build_notes.py</code> from <code>research/out/*.json</code>.</p>
{''.join(sections)}
</body></html>"""
    os.makedirs(os.path.join(ROOT, "docs", "notes"), exist_ok=True)
    open(os.path.join(ROOT, "docs", "notes", "notes.html"), "w").write(page)
    print("wrote docs/notes/notes.html")


if __name__ == "__main__":
    main()
