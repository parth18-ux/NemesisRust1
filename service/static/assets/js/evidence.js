// Evidence packs, shared by the transaction, alert and investigation views. Every request goes through api(), so the
// analyst key is sent (a raw /api link opened in a new tab carries no key and gets 401 when keys are set).
//   evidenceRow(alertId)  a row with View, Verify and Download buttons; wireEvidence(root) makes them work.
// Verify fetches the signed pack, then asks POST /api/v2/verify to check its Ed25519 signature. "Verified" is shown only
// when the server answers {valid: true}.
import {api} from "./api.js";
import {$, html} from "./util.js";

const enc = encodeURIComponent;

export const evidenceRow = (alertId) => html`<li class="ev-row" data-ev="${alertId}" data-focus="pack-${alertId}">
  <span class="mono">${alertId}</span>
  <span class="ev-btns"><button class="btn" type="button" data-ev-view>View pack</button>
    <button class="btn" type="button" data-ev-verify>Verify signature</button>
    <button class="btn" type="button" data-ev-dl="evidence">Download</button>
    <button class="btn" type="button" data-ev-dl="str">STR draft</button></span>
  <div class="ev-out" role="status" aria-live="polite"></div></li>`;

export function save(name, data) {
  const url = URL.createObjectURL(new Blob([JSON.stringify(data, null, 2)], {type: "application/json"}));
  const a = document.createElement("a");
  a.href = url;
  a.download = name;
  document.body.append(a);
  a.click();
  a.remove();
  setTimeout(() => URL.revokeObjectURL(url), 1000);
}

/** Checks one alert's evidence pack; returns {pack, valid} as the server answered. */
export async function verifyPack(alertId) {
  const pack = await api(`/v2/alerts/${enc(alertId)}/evidence`);
  const sig = pack.signature || {};
  const r = await api("/v2/verify", {method: "POST", body: {signed_fields: {pack_hash: pack.pack_hash, alert_id: alertId}, signature: sig.signature || ""}});
  return {pack, valid: r?.valid === true};
}

export function wireEvidence(root) {
  root.addEventListener("click", async (e) => {
    const b = e.target.closest("[data-ev-view],[data-ev-verify],[data-ev-dl]");
    if (!b) return;
    const row = b.closest("[data-ev]"), id = row.dataset.ev, out = $(".ev-out", row);
    b.disabled = true;
    try {
      if (b.hasAttribute("data-ev-verify")) {
        out.innerHTML = html`<span class="muted">Checking…</span>`;
        const {pack, valid} = await verifyPack(id);
        const s = pack.signature || {};
        out.innerHTML = valid
          ? html`<p class="msg ok"><b>Verified</b>: POST /api/v2/verify accepted the ${s.alg} signature (key <span class="mono">${s.key_id}</span>)
              over pack hash <span class="mono">${String(pack.pack_hash).slice(0, 16)}…</span>.</p>`
          : html`<p class="msg err"><b>Not verified</b>: POST /api/v2/verify answered valid: false for this pack.</p>`;
      } else if (b.hasAttribute("data-ev-view")) {
        const pack = await api(`/v2/alerts/${enc(id)}/evidence`);
        out.innerHTML = html`<details open><summary class="small">Evidence pack ${id} (pack hash <span class="mono">${String(pack.pack_hash).slice(0, 16)}…</span>)</summary>
          <pre class="json">${JSON.stringify(pack, null, 2)}</pre></details>`;
      } else {
        const kind = b.dataset.evDl;
        save(`${kind === "str" ? "str-draft" : "evidence"}-${id}.json`, await api(`/v2/alerts/${enc(id)}/${kind}`));
        out.innerHTML = html`<span class="small muted">Downloaded ${kind === "str" ? "the STR draft" : "the evidence pack"} of ${id}.</span>`;
      }
    } catch (err) {
      out.innerHTML = html`<p class="msg err" role="alert">${b.hasAttribute("data-ev-verify") ? "Not verified: " : ""}${err.explain || err.message}</p>`;
    } finally {
      b.disabled = false;
    }
  });
}
