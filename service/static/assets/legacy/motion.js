(() => {
  const bar = document.createElement("div"); bar.className = "scroll-progress"; document.body.appendChild(bar);
  const h = document.documentElement;
  const tick = () => { const m = h.scrollHeight - h.clientHeight; bar.style.setProperty("--p", m > 0 ? h.scrollTop / m : 0); };
  addEventListener("scroll", tick, {passive: true}); addEventListener("resize", tick); tick();
  if (matchMedia("(prefers-reduced-motion: reduce)").matches || !("IntersectionObserver" in window)) return;
  // reveal: only elements that start below the fold, so nothing visible on load flickers
  const sel = "main > section, section > .wrap > *, section > h2, section > p, .card, .kpi, .spec, .find, .stat, table, .tbl";
  const seen = new Set();
  const items = [...document.querySelectorAll(sel)].filter(e => {
    if (e.closest("header, nav, .hero") || [...seen].some(s => s.contains(e))) return false;
    const r = e.getBoundingClientRect();
    if (!r.height || !r.width || r.top < innerHeight * 0.92) return false;  // hidden or already in view: leave as is
    seen.add(e); return true;
  });
  const sib = new Map();
  items.forEach(e => { const i = sib.get(e.parentNode) || 0; sib.set(e.parentNode, i + 1); e.style.setProperty("--m-d", Math.min(i, 5) * 0.07 + "s"); e.classList.add("m-rv"); });
  const io = new IntersectionObserver(es => es.forEach(x => { if (x.isIntersecting) { x.target.classList.add("m-in"); io.unobserve(x.target); } }), {rootMargin: "0px 0px -8% 0px"});
  items.forEach(e => io.observe(e));
  // anything scrolled past too fast to intersect (jump links, restored scroll) is shown, never left invisible
  addEventListener("scroll", () => items.forEach(e => { if (!e.classList.contains("m-in") && e.getBoundingClientRect().top < innerHeight) { e.classList.add("m-in"); io.unobserve(e); } }), {passive: true});
  // draw-in for icons, the ring diagram and the benchmark bars
  const NS = "http://www.w3.org/2000/svg";
  const draw = (svg, step) => svg.querySelectorAll("path, line, polyline, rect, circle").forEach((el, i) => {
    const filled = el.getAttribute("fill") && el.getAttribute("fill") !== "none" || el.closest("g[fill]:not([fill=none])");
    el.style.setProperty("--m-dd", (i * step).toFixed(2) + "s");
    if (filled && el.tagName === "circle") el.classList.add("m-pop");
    else { el.setAttribute("pathLength", "1"); el.classList.add("m-draw"); }
  });
  document.querySelectorAll(".feat svg").forEach(svg => draw(svg, 0.12));
  const ring = document.querySelector(".card svg");
  if (ring) {
    const edges = [...ring.querySelectorAll("g[stroke] path")];
    edges.forEach((p, i) => { p.setAttribute("pathLength", "1"); p.classList.add("m-draw"); p.style.setProperty("--m-dd", (i < 4 ? i * 0.12 : 0.6 + (i - 4) * 0.12) + "s"); });
    ring.querySelectorAll("circle").forEach((c, i) => { c.classList.add("m-pop"); c.style.setProperty("--m-dd", (i === 0 ? 0 : i < 5 ? 0.35 + i * 0.1 : 1.1) + "s"); });
    // a bright dash per edge: victim to mules first, then mules to the cash-out account
    const g = edges[0] && edges[0].parentNode;
    edges.forEach((p, i) => {
      const f = p.cloneNode(); f.removeAttribute("class"); f.setAttribute("pathLength", "1"); f.setAttribute("stroke", i < 4 ? "#ffffff" : "#ffd3cf");
      f.classList.add("m-flow"); f.style.setProperty("--m-fd", (1.6 + (i < 4 ? 0 : 1.15) + (i % 4) * 0.05) + "s"); g.appendChild(f);
    });
    const out = [...ring.querySelectorAll("circle")].pop();
    if (out) { const pl = out.cloneNode(); pl.removeAttribute("class"); pl.setAttribute("fill", "none"); pl.setAttribute("stroke", "#ff6b5f"); pl.setAttribute("stroke-width", "2"); pl.classList.add("m-pulse"); pl.style.animationDelay = "2.4s"; out.parentNode.insertBefore(pl, out); }
  }
  const bars = document.getElementById("bars");
  if (bars) bars.querySelectorAll(".t div").forEach((d, i) => d.style.setProperty("--m-dd", (i * 0.05).toFixed(2) + "s"));
  const dio = new IntersectionObserver(es => es.forEach(x => { if (x.isIntersecting) { x.target.classList.add("m-drawn"); dio.unobserve(x.target); } }), {threshold: 0.35});
  [...document.querySelectorAll(".feat svg"), ring, bars].filter(Boolean).forEach(e => dio.observe(e));

  // count-up: the leading number of a stat (keeps decimals, thousands separators and any suffix)
  const nums = document.querySelectorAll(".stat.num, .kpi .v");
  const run = el => {
    const node = [...el.childNodes].find(n => n.nodeType === 3 && /\d/.test(n.textContent));
    if (!node) return;
    const m = node.textContent.match(/^(\D*)([\d,]*\.?\d+)(.*)$/s);
    if (!m) return;
    const [, pre, raw, post] = m, target = parseFloat(raw.replace(/,/g, "")), dec = (raw.split(".")[1] || "").length, comma = raw.includes(",");
    if (!isFinite(target)) return;
    const fmt = v => { let s = v.toFixed(dec); if (comma) { const [a, b] = s.split("."); s = a.replace(/\B(?=(\d{3})+(?!\d))/g, ",") + (b ? "." + b : ""); } return pre + s + post; };
    const t0 = performance.now(), dur = 1100;
    const step = t => { const k = Math.min(1, (t - t0) / dur), e = 1 - Math.pow(1 - k, 3); node.textContent = fmt(target * e); if (k < 1) requestAnimationFrame(step); else node.textContent = pre + raw + post; };
    requestAnimationFrame(step);
  };
  const nio = new IntersectionObserver(es => es.forEach(x => { if (x.isIntersecting) { run(x.target); nio.unobserve(x.target); } }), {threshold: 0.6});
  nums.forEach(e => nio.observe(e));
})();
