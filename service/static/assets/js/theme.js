// Applies the saved or system colour theme before the first paint (a classic script, so it runs in <head>).
(function () {
  var t = null;
  try { t = localStorage.getItem("rt-theme"); } catch (e) { /* storage blocked: follow the system */ }
  if (!t) t = window.matchMedia && matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light";
  document.documentElement.setAttribute("data-theme", t);
})();
