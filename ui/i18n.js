/* Langue de l'interface (français / anglais).
   L'interface est écrite en français. En anglais, ce script traduit la page à la volée :
   - les paragraphes d'aide statiques (texte + <b>, <code>…) d'un bloc, via I18N_EN_HTML ;
   - tout le reste par nœud de texte et attribut (title, placeholder…), via I18N_EN :
     correspondance exacte, puis modèles « {0} » (parties variables), puis segments séparés par « · ».
   Les dictionnaires sont dans i18n-en.js (clé = texte français). */
(function () {
  const KEY = "island-lang";
  let saved = null;
  try { saved = localStorage.getItem(KEY); } catch {}
  const sys = (navigator.language || "fr").toLowerCase().startsWith("fr") ? "fr" : "en";
  const lang = saved === "fr" || saved === "en" ? saved : sys;
  const I18N = (window.I18N = {
    lang,
    t: (s) => s,
    set(l) {
      try { localStorage.setItem(KEY, l); } catch {}
      location.reload();
    },
  });
  document.documentElement.lang = lang;

  // La langue enregistrée côté appli fait foi (et suit les changements faits dans les réglages).
  const tauri = window.__TAURI__;
  function sync() {
    tauri.core.invoke("get_ui_config").then((c) => {
      if (c && (c.lang === "fr" || c.lang === "en") && c.lang !== lang) I18N.set(c.lang);
    }).catch(() => {});
  }
  if (tauri && tauri.core) {
    sync();
    try { tauri.event.listen("settings-changed", sync); } catch {}
  }
  if (lang !== "en") return;

  const D = window.I18N_EN || {};
  const H = window.I18N_EN_HTML || {};
  const has = (o, k) => Object.prototype.hasOwnProperty.call(o, k);
  const escRe = (s) => s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");

  // modèles : « Envoi sur {0} · {1} % » → /^Envoi sur (.+?) · (.+?) %$/
  const pats = [];
  for (const k of Object.keys(D)) {
    if (!/\{\d\}/.test(k)) continue;
    const parts = k.split(/\{(\d)\}/);
    const lit = parts.filter((_, i) => i % 2 === 0).join("");
    if (lit.replace(/[^A-Za-zÀ-ÿ]/g, "").length < 2) continue; // trop vague
    const idx = parts.filter((_, i) => i % 2 === 1).map(Number);
    const re = new RegExp("^" + parts.map((p, i) => (i % 2 ? "(.+?)" : escRe(p))).join("") + "$");
    pats.push([re, idx, D[k], lit.length]);
  }
  pats.sort((a, b) => b[3] - a[3]);

  function look(n, depth) {
    if (has(D, n)) return D[n];
    if (depth > 2) return null;
    for (const [re, idx, out] of pats) {
      const m = re.exec(n);
      if (!m) continue;
      return out.replace(/\{(\d)\}/g, (_, d) => {
        const v = m[idx.indexOf(+d) + 1];
        if (v == null) return "";
        const t = look(v, depth + 1);
        return t == null ? v : t;
      });
    }
    if (n.includes(" · ")) {
      let any = false;
      const o = n.split(" · ").map((p) => {
        const t = look(p, depth + 1);
        if (t != null) any = true;
        return t == null ? p : t;
      });
      return any ? o.join(" · ") : null;
    }
    const pm = /^(.*?)(\s*[…:.!?]+)$/.exec(n);
    if (pm && pm[1] && has(D, pm[1])) return D[pm[1]] + pm[2];
    const em = /^([^\p{L}\p{N}]+\s)(.+)$/u.exec(n); // « ✓ Envoyé », « 📱 … »
    if (em) {
      const t = look(em[2], depth + 1);
      if (t != null) return em[1] + t;
    }
    return null;
  }

  const cache = new Map();
  function tr(s) {
    if (!s) return s;
    const m = /^(\s*)([\s\S]*?)(\s*)$/.exec(s);
    const core = m[2].replace(/\s+/g, " ");
    if (!core || !/[A-Za-zÀ-ÿ]/.test(core)) return s;
    let r = cache.get(core);
    if (r === undefined) {
      r = look(core, 0);
      if (cache.size > 5000) cache.clear();
      cache.set(core, r);
    }
    return r == null ? s : m[1] + r + m[3];
  }
  I18N.t = tr;

  // ---- blocs d'aide : <li>Ouvre <b>Réglages</b> puis…</li> traduits d'un seul tenant ----
  const INLINE = new Set(["B", "I", "EM", "STRONG", "CODE", "A", "KBD", "SPAN", "BR", "SMALL", "U"]);
  function blockKey(e) {
    let i = 0, text = false;
    const tags = [];
    function rec(node) {
      let s = "";
      for (const c of node.childNodes) {
        if (c.nodeType === 3) {
          s += c.nodeValue;
          if (node === e && c.nodeValue.trim()) text = true;
        } else if (c.nodeType === 1) {
          if (!INLINE.has(c.tagName) || (c.id && c.tagName !== "CODE")) throw 0;
          const k = i++;
          const o = c.outerHTML;
          tags[k] = [o.slice(0, o.indexOf(">") + 1), `</${c.tagName.toLowerCase()}>`];
          s += c.tagName === "BR" ? `<${k}/>` : `<${k}>${rec(c)}</${k}>`;
        }
      }
      return s;
    }
    try {
      const s = rec(e).replace(/\s+/g, " ").trim();
      return i && text ? [s, tags] : null;
    } catch {
      return null;
    }
  }
  const escH = (s) => s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
  function blocks(root) {
    for (const e of root.querySelectorAll("*")) {
      if (INLINE.has(e.tagName) || e.closest("[data-noi18n]")) continue;
      const b = blockKey(e);
      if (!b || !has(H, b[0])) continue;
      const tags = b[1];
      e.innerHTML = H[b[0]]
        .split(/(<\/?\d+\/?>)/)
        .map((p, i) => {
          if (i % 2 === 0) return escH(p);
          const m = /^<(\/?)(\d+)(\/?)>$/.exec(p);
          const t = tags[+m[2]];
          if (!t) return "";
          return m[3] ? t[0] : m[1] ? t[1] : t[0];
        })
        .join("");
    }
  }

  // ---- nœuds de texte et attributs ----
  const SKIP = new Set(["SCRIPT", "STYLE", "TEXTAREA", "CODE", "KBD"]);
  const ATTR = ["title", "placeholder", "aria-label", "data-tip"];
  const seen = new WeakMap();
  function text(n) {
    const v = n.nodeValue;
    if (seen.get(n) === v) return;
    const p = n.parentElement;
    if (!p || SKIP.has(p.tagName) || p.closest("[data-noi18n]")) return seen.set(n, v);
    const t = tr(v);
    seen.set(n, t);
    if (t !== v) n.nodeValue = t;
  }
  function attrs(e) {
    for (const a of ATTR) {
      const v = e.getAttribute(a);
      if (!v) continue;
      const t = tr(v);
      if (t !== v && !e.closest("[data-noi18n]")) e.setAttribute(a, t);
    }
  }
  function walk(root) {
    if (root.nodeType === 3) return text(root);
    if (root.nodeType !== 1) return;
    attrs(root);
    const w = document.createTreeWalker(root, NodeFilter.SHOW_ELEMENT | NodeFilter.SHOW_TEXT);
    for (let n = w.nextNode(); n; n = w.nextNode()) {
      if (n.nodeType === 3) text(n);
      else attrs(n);
    }
  }

  function start() {
    blocks(document.body);
    walk(document.body);
    document.title = tr(document.title);
    new MutationObserver((list) => {
      for (const m of list) {
        if (m.type === "characterData") text(m.target);
        else if (m.type === "attributes") attrs(m.target);
        else for (const n of m.addedNodes) walk(n);
      }
    }).observe(document.body, { subtree: true, childList: true, characterData: true, attributes: true, attributeFilter: ATTR });
  }
  if (document.body) start();
  else document.addEventListener("DOMContentLoaded", start);
})();
