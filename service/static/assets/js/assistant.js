// H. AI-assisted investigation: the narrative draft of an investigation (GET /api/v2/investigations/{id}/narrative), a
// deterministic template draft, not a language model. Each statement shows its type and links to its sources (alert,
// transaction, ledger entry, evidence pack, comment). The analyst can edit a summary and accept it into a case comment
// through the existing actions endpoint (POST .../actions, action "comment", a fresh X-Nonce), so the acceptance is in
// the ledger. The draft decides nothing. Used in the case workspace (narrativePanel) and on #/assistant (render).
import {api} from "./api.js";
import {$, html, words, toast} from "./util.js";

const enc = encodeURIComponent;
export const TYPE_LABEL = {observed_fact: "Fact", model_output: "Model output", rule_finding: "Rule finding",
  interpretation: "Interpretation", analyst_conclusion: "Analyst conclusion"};
export const NOTICE = "Draft — requires analyst review. AI-assisted interpretations are not conclusions.";
const COMMENT_MAX = 1000;

export const typeBadge = (t) => html`<span class="nb nb-${t}" title="${t}">${TYPE_LABEL[t] ?? words(t)}</span>`;

/** Where a source lives in the workspace: [href, label, focus key]. */
export function sourceLink(caseId, s) {
  const ws = (f) => `#/investigations/${enc(caseId)}?focus=${enc(f)}`;
  switch (s.kind) {
    case "alert": return [`#/alerts/${enc(s.id)}`, `alert ${s.id}`, ""];
    case "transaction": return [`#/transactions/${enc(s.id)}`, `txn ${s.id}`, ""];
    case "ledger": return [ws(`ledger-${s.seq}`), `ledger #${s.seq}`, `ledger-${s.seq}`];
    case "evidence_pack": return [ws(`pack-${s.alert_id}`), `pack ${String(s.pack_hash).slice(0, 10)}… (${s.alert_id})`, `pack-${s.alert_id}`];
    case "comment": return [ws(`comment-${s.index}`), `comment ${s.index + 1}`, `comment-${s.index}`];
    default: return ["", `${s.kind}`, ""];
  }
}

export const sourceLinks = (caseId, sources) => html`<span class="nr-src">${(sources || []).map((s) => {
  const [href, label, focus] = sourceLink(caseId, s);
  return href ? html`<a class="src-link" href="${href}" ${focus ? html`data-focus-to="${focus}"` : ""}>${label}</a>` : html`<span class="src-link">${label}</span>`;
})}</span>`;

/** The statements grouped by section, with type badges and source links (also used by the report). */
export function narrativeBody(n) {
  const by = (sec) => (n.statements || []).filter((s) => s.section === sec);
  return html`${(n.sections || []).map((sec) => { const st = by(sec.id); return html`<div class="nr-sec">
      <h3>${sec.title}</h3>${st.length ? html`<ul class="nr-list">${st.map((s) => html`<li class="nr-st nr-${s.type}" data-st="${s.id}">
        ${typeBadge(s.type)} <span class="nr-text">${s.text}</span> ${sourceLinks(n.case_id, s.sources)}</li>`)}</ul>`
        : html`<p class="small muted">${sec.id === "analyst_conclusions" ? "No analyst conclusion recorded yet." : "Nothing in the stored data for this section."}</p>`}</div>`; })}
    <div class="nr-sec"><h3>Suggested additional checks</h3><p class="small muted">Chosen by which signals fired.</p>
      <ul class="nr-list">${(n.suggested_checks || []).map((c) => html`<li class="nr-st"><span class="nb nb-check">Check</span> <span class="nr-text">${c.text}</span> ${sourceLinks(n.case_id, c.sources)}</li>`)}</ul></div>
    <div class="nr-sec"><h3>Open questions</h3>
      ${(n.open_questions || []).length ? html`<ul class="nr-list">${n.open_questions.map((q) => html`<li class="nr-st"><span class="nb nb-question">Question</span> <span class="nr-text">${q.text}</span> ${sourceLinks(n.case_id, q.sources)}</li>`)}</ul>`
        : html`<p class="small muted">None.</p>`}</div>`;
}

export const legend = () => html`<p class="nr-legend small">${Object.keys(TYPE_LABEL).map((t) => html`${typeBadge(t)} `)}</p>`;

/** A first text for the analyst to edit: the summary and interpretations, cut to the comment limit. */
function acceptText(n) {
  const pick = (n.statements || []).filter((s) => s.section === "summary" || s.type === "interpretation");
  const head = `Narrative draft accepted after review (${n.generated_by}, digest ${String(n.digest).slice(0, 12)}): `;
  let t = head + pick.map((s) => `[${TYPE_LABEL[s.type]}] ${s.text}`).join(" ");
  if (t.length > COMMENT_MAX) t = t.slice(0, COMMENT_MAX - 1) + "…";
  return t;
}

/** The panel's markup: the banner and the Generate button; wireNarrative fills it. */
export const narrativePanel = (caseId) => html`<div class="nr" data-nr="${caseId}">
  <div class="banner nr-banner" role="note"><b>${NOTICE}</b> <span class="small">Built from fixed templates over the stored case data (no language model). It never blocks or changes a decision.</span></div>
  <div class="row-btns"><button class="btn primary" type="button" data-nr-gen>Generate draft</button></div>
  <div class="nr-out" aria-live="polite"></div></div>`;

/** Wires every narrative panel under root. `onAccepted(investigation)` runs after the comment is saved. */
export function wireNarrative(root, onAccepted = () => {}) {
  root.addEventListener("click", async (e) => {
    const gen = e.target.closest("[data-nr-gen]");
    const acc = e.target.closest("[data-nr-accept]");
    const link = e.target.closest("a[data-focus-to]");
    if (link) {
      // in the workspace the source is on this page: scroll to it instead of re-rendering
      const t = document.querySelector(`[data-focus="${CSS.escape(link.dataset.focusTo)}"]`);
      if (t) { e.preventDefault(); focusEl(t); }
      return;
    }
    if (!gen && !acc) return;
    const box = e.target.closest("[data-nr]"), id = box.dataset.nr, out = $(".nr-out", box);
    if (gen) {
      gen.disabled = true;
      out.innerHTML = html`<p class="muted small" role="status">Generating the draft from stored data…</p>`;
      try {
        const n = await api(`/v2/investigations/${enc(id)}/narrative`);
        box._n = n;
        out.innerHTML = html`${legend()}${narrativeBody(n)}
          <p class="src">GET /api/v2/investigations/${id}/narrative · ${n.generated_by} · requires_human_review: ${String(n.requires_human_review)} ·
            digest <span class="mono">${String(n.digest).slice(0, 16)}…</span> · optional LLM rephrasing: ${n.llm_rephrase?.enabled ? "enabled" : "not enabled"}.</p>
          <form class="nr-accept" data-nr-form><label class="field">Edit before accepting (saved as a case comment, at most ${COMMENT_MAX} characters)
            <textarea name="text" rows="5" maxlength="${COMMENT_MAX}" required>${acceptText(n)}</textarea></label>
            <div class="row-btns"><button class="btn primary" type="button" data-nr-accept>Accept into case comment</button>
              <span class="small muted">Recorded as an analyst action in the hash-chained ledger.</span></div>
            <div class="nr-msg" role="status" aria-live="polite"></div></form>`;
        gen.textContent = "Regenerate draft";
      } catch (err) {
        out.innerHTML = html`<p class="msg err" role="alert">${err.explain || err.message}</p>`;
      } finally {
        gen.disabled = false;
      }
    } else {
      const text = $("textarea", box).value.trim(), msg = $(".nr-msg", box);
      if (!text) { msg.innerHTML = html`<p class="msg err" role="alert">Write the conclusion to accept first.</p>`; return; }
      acc.disabled = true;
      try {
        const inv = await api(`/v2/investigations/${enc(id)}/actions`, {method: "POST", body: {action: "comment", text}});
        const h = (inv.history || []).at(-1) || {};
        toast(`${id}: draft accepted into a comment (ledger #${h.ledger_seq})`);
        msg.innerHTML = html`<p class="msg ok">Accepted by ${h.analyst} as a case comment: ledger entry <span class="mono">#${h.ledger_seq}</span>
          (hash <span class="mono">${String(h.ledger_hash).slice(0, 12)}…</span>). <a href="#/investigations/${enc(id)}?focus=ledger-${h.ledger_seq}">See it in the audit history</a>.</p>`;
        await onAccepted(inv);
      } catch (err) {
        msg.innerHTML = html`<p class="msg err" role="alert">Not saved: ${err.explain || err.message}</p>`;
        acc.disabled = false;
      }
    }
  });
}

/** Scrolls to an element and marks it for a moment. */
export function focusEl(el) {
  el.scrollIntoView({block: "center"});
  el.classList.add("hl");
  setTimeout(() => el.classList.remove("hl"), 2400);
}

// ---- #/assistant: the same panel with a case picker

export async function render(main, ctx) {
  let r;
  try { r = await api("/v2/investigations?limit=500"); } catch (e) { main.innerHTML = html`<div class="banner err" role="alert">${e.explain || e.message}</div>`; return; }
  if (!ctx.isCurrent()) return;
  const all = r.investigations || [];
  const id = ctx.params[0] || ctx.query.get("case") || all[0]?.case_id || "";
  main.innerHTML = html`<section class="card" aria-labelledby="as-h">
    <div class="card-h"><h2 id="as-h">AI-assisted investigation</h2>${id ? html`<a class="btn" href="#/investigations/${enc(id)}">Open the case workspace</a>
      <a class="btn" href="#/reports/${enc(id)}">Report</a>` : ""}</div>
    <p class="small">A draft narrative of one investigation. Every statement is typed (fact, model output, rule finding, interpretation, analyst conclusion) and links to the stored record it was read from.</p>
    ${all.length ? html`<label class="field">Investigation <select id="as-pick">${all.map((c) => html`<option value="${c.case_id}" ${c.case_id === id ? "selected" : ""}>${c.case_id} · ${c.title} (${words(c.status)})</option>`)}</select></label>`
      : html`<p class="empty">No investigation yet: open one from Alert management first.</p>`}
    ${id ? narrativePanel(id) : ""}</section>`;
  $("#as-pick", main)?.addEventListener("change", (e) => { location.hash = "#/assistant/" + enc(e.target.value); });
  wireNarrative(main.firstElementChild);
}
