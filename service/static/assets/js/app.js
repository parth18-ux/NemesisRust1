// App shell: the section navigation (A to I), hash routing, the instance status chips, the analyst key and the theme.
import {api, getKey, setKey, demoKeys} from "./api.js";
import {$, $$, html, toast} from "./util.js";

const SECTIONS = [
  {k: "A", id: "dashboard", name: "Executive dashboard", load: () => import("./dashboard.js")},
  {k: "B", id: "transactions", name: "Transaction intelligence", load: () => import("./transactions.js")},
  {k: "C", id: "scams", name: "Scam intelligence", stage: "W7", what: "message checks, scam workflows and tactics, and the lure model card",
   links: [["/investigate", "Investigator"]]},
  {k: "D", id: "behaviour", name: "Behavioural", stage: "W7", what: "typing, session and identity signals, PRAMAAN and duress outcomes, each marked consent-based",
   links: [["/investigate", "Investigator"]]},
  {k: "E", id: "network", name: "Network intelligence", load: () => import("./network.js")},
  {k: "F", id: "alerts", name: "Alert management", load: () => import("./alerts.js")},
  {k: "G", id: "investigations", name: "Investigations", load: () => import("./investigations.js")},
  {k: "H", id: "assistant", name: "AI-assisted investigation", stage: "W6", what: "a narrative draft in which every statement links to its source and is marked for analyst review",
   links: []},
  {k: "I", id: "reports", name: "Reports and evidence", stage: "W6", what: "the printable case report, STR draft, evidence packs and signature checks",
   links: [["/report", "Benchmark report"], ["/docs", "API docs"]]},
];
const OTHER = [["/console", "Live console"], ["/investigate", "Investigator"], ["/report", "Benchmark report"], ["/docs", "API docs"]];

const main = $("#main");
const side = $("#side");
const menu = $("#menu");
const scrim = $("#scrim");

function drawNav(current) {
  $("#nav").innerHTML = html`<div class="nav-h">Workspace</div>
    ${SECTIONS.map((s) => html`<a href="#/${s.id}" data-id="${s.id}" ${s.id === current ? html`aria-current="page"` : ""}>
      <span class="k" aria-hidden="true">${s.k}</span><span>${s.name}</span>${s.load ? "" : html`<span class="soon">${s.stage}</span>`}</a>`)}
    <div class="nav-h">Other pages</div>
    ${OTHER.map(([href, name]) => html`<a href="${href}"><span class="k" aria-hidden="true">&#8599;</span><span>${name}</span></a>`)}`;
}

function soonView(s) {
  main.innerHTML = html`<section class="soon-view card" aria-labelledby="soon-h">
    <span class="pill plain">Section ${s.k} · stage ${s.stage}</span>
    <h2 id="soon-h">${s.name}: coming in a later stage</h2>
    <p>This section is not built yet. It will cover ${s.what}. Nothing on this page is a placeholder number.</p>
    ${s.links.length ? html`<p>Until then, the existing pages cover part of it:</p>
      <p>${s.links.map(([href, name]) => html`<a class="btn" href="${href}">${name}</a> `)}</p>` : ""}
  </section>`;
}

let renderSeq = 0;
async function route() {
  const [path, query = ""] = location.hash.replace(/^#\/?/, "").split("?");
  let [id = "dashboard", ...rest] = path.split("/").map(decodeURIComponent);
  if (id === "cases") id = "investigations";
  const s = SECTIONS.find((x) => x.id === id) || SECTIONS[0];
  drawNav(s.id);
  $("#title").textContent = s.name;
  document.title = `${s.name} · RuseTect`;
  closeMenu();
  $$(".drawer, dialog").forEach((d) => d.remove());
  const seq = ++renderSeq;
  if (!s.load) { soonView(s); return; }
  main.innerHTML = html`<p class="muted" role="status">Loading ${s.name.toLowerCase()}…</p>`;
  try {
    const mod = await s.load();
    if (seq === renderSeq) await mod.render(main, {params: rest, query: new URLSearchParams(query), isCurrent: () => seq === renderSeq});
  } catch (e) {
    if (seq === renderSeq) main.innerHTML = html`<div class="banner err" role="alert">This view failed to load: ${e.explain || e.message}</div>`;
  }
}

function openMenu() { side.classList.add("open"); scrim.hidden = false; menu.setAttribute("aria-expanded", "true"); $("#nav a")?.focus(); }
function closeMenu() { side.classList.remove("open"); scrim.hidden = true; menu.setAttribute("aria-expanded", "false"); }
menu.addEventListener("click", () => (side.classList.contains("open") ? closeMenu() : openMenu()));
scrim.addEventListener("click", closeMenu);
document.addEventListener("keydown", (e) => { if (e.key === "Escape" && side.classList.contains("open")) { closeMenu(); menu.focus(); } });

$("#theme").addEventListener("click", () => {
  const t = document.documentElement.getAttribute("data-theme") === "dark" ? "light" : "dark";
  document.documentElement.setAttribute("data-theme", t);
  try { localStorage.setItem("rt-theme", t); } catch { /* storage blocked */ }
});

async function drawChips() {
  const box = $("#chips");
  try {
    const h = await api("/health");
    const auth = String(h.auth || "");
    box.innerHTML = html`<span class="chip"><span class="dot ${h.ready ? "ok" : "bad"}"></span>${h.ready ? "ready" : "not ready"}</span>
      <span class="chip opt" title="Where decisions are stored">store: ${h.store?.kind ?? "n/a"}</span>
      <span class="chip opt" title="Evidence signing key${h.signing?.ephemeral ? " (ephemeral: changes on restart)" : ""}">key ${h.signing?.key_id ?? "n/a"}</span>
      <span class="chip opt" title="${auth}">${auth.startsWith("demo") ? "demo auth" : "analyst keys"}</span>
      <span class="chip opt">v${h.version ?? "?"}</span>`;
  } catch (e) {
    box.innerHTML = html`<span class="chip"><span class="dot bad"></span>health unavailable</span>`;
  }
}

async function drawWho() {
  const box = $("#who");
  const demo = await demoKeys();
  const key = getKey();
  if (demo.length) {
    box.innerHTML = html`<label for="who-key">Acting as</label>
      <select id="who-key"><option value="">no key (read only)</option>
        ${demo.map((a) => html`<option value="${a.key}" ${a.key === key ? "selected" : ""}>${a.analyst} (${a.role})</option>`)}</select>`;
    $("#who-key").addEventListener("change", (e) => { setKey(e.target.value); toast(e.target.value ? "Demo analyst key selected" : "Read only"); route(); });
  } else {
    box.innerHTML = html`<label for="who-key">Analyst key</label>
      <input id="who-key" type="password" autocomplete="off" placeholder="${key ? "key set for this tab" : "paste key"}" size="14">
      <button class="btn" id="who-set" type="button">${key ? "Change" : "Use"}</button>
      ${key ? html`<button class="btn" id="who-clear" type="button">Clear</button>` : ""}`;
    $("#who-set").addEventListener("click", () => { setKey($("#who-key").value.trim()); drawWho(); route(); });
    $("#who-clear")?.addEventListener("click", () => { setKey(""); drawWho(); route(); });
  }
}

window.addEventListener("hashchange", route);
drawChips();
drawWho().then(route);
