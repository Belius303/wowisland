"use strict";

const T = window.__TAURI__;
const invoke = T ? T.core.invoke : async (...a) => console.log("invoke", ...a);
const listen = T ? T.event.listen : async () => {};
const $ = (id) => document.getElementById(id);
/* langue des dates et heures (voir i18n.js) */
const LOC = window.I18N && I18N.lang === "en" ? "en-GB" : "fr-FR";
const island = $("island");
const bubble = $("bubble");
const arrL = $("arr-l"), arrR = $("arr-r");
const ARW = 26, ARGAP = 7; // flèches du mode compact : diamètre, écart avec l'île
const pdot = $("pdot"), pager = $("pager");
const TOP = 7;
const MAX_XH = 450; // fenêtre de 470 px de haut

/* ============================================================
   Ressorts physiques (interruptibles, conservent la vitesse)
   ============================================================ */
class Spring {
  constructor(v) { this.v = v; this.t = v; this.vel = 0; }
  step(dt, k, c) {
    const f = -k * (this.v - this.t) - c * this.vel;
    this.vel += f * dt;
    this.v += this.vel * dt;
  }
  get done() { return Math.abs(this.v - this.t) < 0.08 && Math.abs(this.vel) < 0.08; }
}
const W = new Spring(124), H = new Spring(30), R = new Spring(15);
let springCfg = { k: 380, c: 28 };
let animating = false;
let lastFrame = 0;

function animate(now) {
  const dt = Math.min(0.034, (now - lastFrame) / 1000 || 0.016);
  lastFrame = now;
  const sub = 4;
  for (let i = 0; i < sub; i++) {
    W.step(dt / sub, springCfg.k, springCfg.c);
    H.step(dt / sub, springCfg.k, springCfg.c);
    R.step(dt / sub, springCfg.k * 1.1, springCfg.c * 1.05);
  }
  applySize();
  if (W.done && H.done && R.done) {
    W.v = W.t; H.v = H.t; R.v = R.t; W.vel = H.vel = R.vel = 0;
    applySize();
    animating = false;
    return;
  }
  requestAnimationFrame(animate);
}
function applySize() {
  island.style.width = Math.max(10, W.v) + "px";
  island.style.height = Math.max(10, H.v) + "px";
  island.style.borderRadius = Math.max(4, Math.min(R.v, H.v / 2)) + "px";
  const half = Math.max(10, W.v) / 2;
  const arr = arrowsShown();
  bubble.style.left = `calc(50% + ${half + 7 + (arr ? ARW + ARGAP : 0)}px)`;
  arrL.style.left = `calc(50% - ${half + ARGAP + ARW}px)`;
  arrR.style.left = `calc(50% + ${half + ARGAP}px)`;
}
function springTo(w, h, r) {
  const grow = w * h > W.t * H.t + 1;
  // Expansion : un peu de rebond. Repli : plus amorti.
  springCfg = grow ? { k: 340, c: 25 } : { k: 420, c: 36 };
  W.t = w; H.t = h; R.t = r;
  if (!animating) {
    animating = true;
    lastFrame = performance.now();
    requestAnimationFrame(animate);
  }
}

/* ============================================================
   État
   ============================================================ */
const S = {
  hover: false,
  fullscreen: false,
  hideFs: true,
  media: null, // source principale
  media2: null, // 2e source qui joue en même temps (ex. un live + Spotify)
  medias: [],
  mediaSel: null,
  mediaAt: 0,
  mixerOpen: false,
  mixer: null,
  mixDrag: null,
  notifs: [], // centre de notifications
  unread: 0,
  notifOpen: false,
  notifExpanded: new Set(),
  spOn: false, // Spotify connecté (réglages)
  sp: null, // { connected, volume, device, supports_volume, error }
  lastPlayingAt: 0,
  accent: "#ff375f",
  phone: { connected: false },
  game: { running: null, last: null, color: null }, // jeu en cours / dernier jeu joué
  lol: null, // coach League of Legends (partie en cours)
  lolRot: 0,
  timer: null, // { end, total }
  showTimer: false, // bouton minuteur (option des réglages)
  hiddenTools: new Set(), // boutons de l'île masqués (réglages)
  mu: { open: false, tracks: [], now: null, q: "", loaded: false }, // « Ma musique »
  muVol: 100, // volume du lecteur de l'île (0-100), indépendant du volume Windows
  modeHold: 0, // juste après un clic aléatoire / répéter : on garde l'état affiché le temps que l'appli suive
  pin: null, // activité choisie avec les flèches en mode compact
  privacy: { mic: [], cam: [] }, // applis qui utilisent le micro / la caméra
  clips: [], // historique du presse-papiers
  clipOpen: false,
  gr: { open: false, url: "", info: null, probing: false, err: null, kind: "video", fmt: 0, cut: false, a: "", b: "", cutErr: "", jobs: [], status: null, lastClip: null, used: new Set(), liking: new Set(), adding: new Set(), unfold: new Set() },
  bdls: [], // téléchargements du navigateur (S.dls = ceux-ci + ceux de l'île)
  cl: { open: false, msgs: [], running: false, model: null, usage: null, rate: null, status: null, stick: false },
  dls: [], // téléchargements en cours
  lyrics: { key: null, lines: null }, // paroles synchronisées du morceau affiché
  lyricsCompact: false,
  clipQ: "", // recherche dans le presse-papiers
  voice: null, // salon vocal Discord { linked, channel, members, mute, deaf, me }
  picking: false,
  queue: [],
  current: null,
  view: "idle",
  target: [124, 30, 15],
  warned: new Set(),
};
let endTimer = null;
let hoverTimer = null;

const hasMedia = () => !!(S.media && S.media.title);

/* Ordre naturel des activités ; celle choisie avec les flèches passe devant */
function activities() {
  const list = baseActivities();
  const i = S.pin ? list.indexOf(S.pin) : -1;
  if (i > 0) list.unshift(...list.splice(i, 1));
  return list;
}
function baseActivities() {
  const list = [];
  if (S.dls.length) list.push("download");
  if (S.game.running) list.push("game");
  if (voiceOn()) list.push("voice");
  const mediaActive = hasMedia() && (S.media.playing || Date.now() - S.lastPlayingAt < 45000);
  if (mediaActive && S.media.playing) list.push("media");
  if (S.media2) list.push("media2");
  if (S.timer) list.push("timer");
  if (mediaActive && !S.media.playing) list.push("media");
  if (S.phone.connected && S.phone.battery != null) list.push("phone");
  return list;
}

function currentView() {
  if (S.current) return S.current.kind;
  if (S.fullscreen && S.hideFs) return "hidden";
  if (S.hover) return "expanded";
  return activities().length ? "compact" : "idle";
}

function pvKind() {
  return S.privacy.cam.length ? "cam" : S.privacy.mic.length ? "mic" : "";
}
function sizeFor(view) {
  const t = baseSize(view);
  // le point micro / caméra a sa propre place à droite : il ne recouvre jamais l'icône ou le texte
  if (pvKind() && (view === "compact" || view === "idle")) t[0] += 12;
  return t;
}
function baseSize(view) {
  switch (view) {
    case "compact": {
      const p = activities()[0];
      if (p === "media" || p === "media2") {
        // pochette + nom (titre, ou streamer pour un live) + égaliseur
        // avec les paroles : largeur de la plus longue ligne, pour que l'île ne bouge pas à chaque ligne
        const lw = p === "media" && lyricsCompactOn() ? Math.max(textWidth(S.media.title), lyricsMaxW()) : textWidth(compactLabel());
        const w = 6 + 23 + 12 + lw + 12 + 22 + 10;
        return [Math.round(Math.min(380, Math.max(268, w))), 34, 17];
      }
      if (p === "download") {
        const w = 6 + 23 + 12 + textWidth(compactLabel(), 200) + 12 + textWidth(dlLabel(S.dls[0])) + 14;
        return [Math.round(Math.min(380, Math.max(240, w))), 34, 17];
      }
      if (p === "voice") {
        const w = 6 + 23 + 12 + textWidth(compactLabel(), 200) + 12 + voiceTrailWidth() + 10;
        return [Math.round(Math.min(380, Math.max(240, w))), 34, 17];
      }
      if (p === "game") {
        // logo du jeu + nom + temps de jeu (en LoL : champion + conseil + prochain item)
        const w = 6 + 23 + 12 + textWidth(compactLabel(), S.lol ? 310 : 250) + 12 + (S.lol ? 36 : 46) + 10;
        return [Math.round(Math.min(S.lol ? 400 : 380, Math.max(240, w))), 34, 17];
      }
      return [214, 34, 17];
    }
    case "expanded": {
      // Même largeur pour tous les onglets, et la hauteur ne rétrécit jamais tant que la souris est dessus :
      // sinon le bord de l'île passe sous le curseur, le survol se perd et l'île se referme.
      let h = S.cl.open ? 440 : S.gr.open ? grPanelHeight() : S.mu.open ? muPanelHeight() : S.clipOpen ? clipPanelHeight() : S.notifOpen ? notifPanelHeight() : S.mixerOpen ? mixerHeight() : S.picking ? 190
        : (hasMedia() ? 236 + (volSrc() ? 26 : 0) + (lyricsOn() ? 48 : 0) + otherMedias().length * 38 : 140 + (S.lol ? 10 + lolCardHeight() : S.game.running || S.game.last ? 66 : 0))
          + (S.dls.length ? 6 + Math.min(2, S.dls.length) * 40 : 0)
          + (voiceOn() ? 8 + voiceH() : 0);
      // jamais plus haut que la fenêtre transparente (sinon le bas est coupé)
      h = Math.min(MAX_XH, Math.max(h, S.xMinH || 0));
      S.xMinH = h;
      return [440, h, 38];
    }
    case "alert": return [408, S.current && (S.current.data.body || "").length > 46 ? 90 : 76, 30];
    case "secret": return [S.current && S.current.data.link ? 404 : 376, 92, 32];
    case "charge": return [340, 64, 32];
    case "osd": return [250, 34, 17];
    case "pill": {
      const d = S.current && S.current.data;
      const w = 6 + 24 + 10 + textWidth(d ? d.name + (d.join ? " a rejoint" : " est parti") : "", 220) + 10 + 8 + 14;
      return [Math.round(Math.max(190, Math.min(360, w))), 34, 17];
    }
    case "shot": {
      const d = S.current && S.current.data;
      return [d && (d.text || d.qr) ? 412 : 362, 84, 30];
    }
    default: return idleSize();
  }
}

/* ============================================================
   Rendu principal
   ============================================================ */
function render() {
  const view = currentView();
  const target = sizeFor(view);
  const prev = S.view;
  S.view = view;

  document.querySelectorAll(".view").forEach((el) => {
    el.classList.toggle("active", el.dataset.view === (view === "hidden" ? "idle" : view));
  });
  island.classList.toggle("hidden", view === "hidden");
  // teinte aux couleurs de la pochette quand la musique joue
  const m = S.media;
  // une notif prend la couleur de son appli (Instagram, Snapchat…), sinon la musique celle de la pochette
  const nc = ((view === "alert" || view === "pill") && S.current && S.current.data.color) || (view === "compact" && activities()[0] === "game" && S.game.color);
  if (nc) {
    island.style.setProperty("--tint1", `hsl(${nc[0]} ${nc[1]}% 42% / 0.55)`);
    island.style.setProperty("--tint2", `hsl(${nc[0]} ${Math.round(nc[1] * 0.85)}% 15%)`);
  } else {
    island.style.removeProperty("--tint1");
    island.style.removeProperty("--tint2");
  }
  island.classList.toggle("tinted", !!nc || (!!(m && m.art && m.playing) && (view === "expanded" || (view === "compact" && activities()[0] === "media"))));
  island.classList.toggle("big", view !== "idle" && view !== "compact" && view !== "hidden");

  // Chaque vue a la taille de sa cible (pas de reflow pendant l'animation)
  const active = document.querySelector(`.view[data-view="${view}"]`);
  if (active) {
    active.style.width = target[0] + "px";
    active.style.height = target[1] + "px";
  }

  if (view === "compact" || view === "idle") renderCompact();
  if (view === "idle" || view === "hidden") renderIdle();
  if (view === "expanded") renderExpanded();
  const arr = view === "compact" && baseActivities().length > 1;
  arrL.classList.toggle("show", arr);
  arrR.classList.toggle("show", arr);
  if (S.arrows !== arr) { S.arrows = arr; applySize(); }
  renderPager(arr);
  const pv = pvKind();
  const pvShow = !!pv && (view === "compact" || view === "idle");
  pdot.className = pv ? (pvShow ? "show " : "") + pv : "";
  island.classList.toggle("pv", pvShow);
  renderBubble(view);

  if (prev !== view || target.join() !== S.target.join()) {
    S.target = target;
    springTo(...target);
  }
  reportRects();
}

function reportRects() {
  const vw = window.innerWidth;
  if (S.view === "hidden") return invoke("set_island_rects", { rects: [] });
  const [w, h] = S.target;
  const rects = [{ x: (vw - w) / 2, y: TOP, w, h }];
  const arr = arrowsShown();
  if (arr) {
    // zone 1 : cliquable sans ouvrir l'île
    rects.push({ x: vw / 2 - w / 2 - ARW - ARGAP, y: TOP + 2, w: ARW, h: 30, zone: 1 });
    rects.push({ x: vw / 2 + w / 2 + ARGAP, y: TOP + 2, w: ARW, h: 30, zone: 1 });
  }
  if (bubble.classList.contains("show")) rects.push({ x: vw / 2 + w / 2 + 7 + (arr ? ARW + ARGAP : 0), y: TOP, w: 34, h: 34 });
  invoke("set_island_rects", { rects });
}

/* ---------- compact ---------- */
/* Nom affiché au milieu de l'activité compacte : le streamer pour un live, le titre pour une musique */
function compactMedia() {
  const p = activities()[0];
  return p === "media" ? S.media : p === "media2" ? S.media2 : null;
}
function compactLabel() {
  if (activities()[0] === "download") return S.dls[0].name;
  if (activities()[0] === "voice") return S.voice.channel.name;
  if (activities()[0] === "game") return S.lol ? lolTip().text : S.game.running.name || "";
  const m = compactMedia();
  if (!m) return "";
  if (activities()[0] === "media" && lyricsCompactOn()) {
    const l = lyricLine(0);
    if (l) return l;
  }
  return (m.duration > 0 ? m.title : m.artist || m.title) || "";
}
const measureCtx = document.createElement("canvas").getContext("2d");
function textWidth(t, cap = 250) {
  if (!t) return 0;
  measureCtx.font = '600 12.5px "Segoe UI Variable Text", "Segoe UI", system-ui, sans-serif';
  return Math.min(cap, measureCtx.measureText(t).width);
}
function renderCompact() {
  const p = activities()[0];
  const lead = $("c-lead"), trail = $("c-trail");
  const mid = $("c-mid");
  const label = compactLabel();
  if (p === "media" && lyricsCompactOn() && lyricLine(0)) setKaraoke(mid, label);
  else mid.textContent = label;
  mid.style.color = compactMedia() && !compactMedia().playing ? "var(--fg3)" : "";
  mid.className = "c-mid" + (p === "game" && S.lol ? " lol k-" + lolTip().kind + (S._lolFlash > Date.now() ? " flash" : "") : "");
  const cv = document.querySelector('[data-view="compact"]');
  cv.classList.toggle("dl", p === "download");
  if (p === "voice") {
    const sp = voiceSpeaker();
    setHTML(lead, sp ? `<div class="vc-lead sp"><img src="${esc(sp.avatar)}" alt=""></div>` : `<div class="vc-ic">${I.voice}</div>`, "vlead" + (sp ? sp.id : ""));
    setHTML(trail, voiceTrailHTML(), voiceTrailKey());
  } else if (p === "download") {
    const d = S.dls[0];
    setHTML(lead, `<div class="dl-ic">${I.download}</div>`, "dl");
    setHTML(trail, `<div class="lbl blue" id="c-dl"></div>`, "dllbl");
    $("c-dl").textContent = dlLabel(d);
    const bar = document.querySelector('[data-view="compact"] .dl-bar');
    bar.classList.toggle("det", d.pct != null);
    bar.firstElementChild.style.width = d.pct != null ? (d.pct * 100).toFixed(1) + "%" : "";
  } else if (p === "game" && S.lol) {
    const L = S.lol, tip = lolTip();
    setHTML(lead, `<img src="${L.champ_icon}" alt="">`, "lol:" + L.champ_icon);
    const n = L.next;
    setHTML(trail, n ? `<img class="c-item${L.missing === 0 ? " ready" : ""}" src="${n.icon}" alt="">` : `<div class="glyph green">${I.gamepad}</div>`,
      "lolnext" + (n ? n.id + "|" + (L.missing === 0) : ""));
    if (tip.text !== S._lolShown) {
      S._lolShown = tip.text;
      if (tip.prio >= 70) { S._lolFlash = Date.now() + 1100; mid.classList.remove("flash"); void mid.offsetWidth; mid.classList.add("flash"); }
    }
  } else if (p === "game") {
    const g = S.game.running;
    setHTML(lead, g.icon ? `<img src="${g.icon}" alt="">` : `<div class="glyph green">${I.gamepad}</div>`, "game:" + g.key + (g.icon ? 1 : 0));
    setHTML(trail, `<div class="lbl green" id="c-gtime"></div>`, "gtime");
    $("c-gtime").textContent = playTime(g.since);
  } else if (p === "media" || p === "media2") {
    const m = p === "media" ? S.media : S.media2;
    const art = m.art;
    setHTML(lead, art ? `<img src="${art}" alt="">` : `<div class="glyph" style="color:${S.accent}">${I.note}</div>`, "media:" + (art ? art.length : 0));
    setHTML(trail, `<div class="eq" id="c-eq"><i></i><i></i><i></i><i></i></div>`, "eq");
    $("c-eq").classList.toggle("paused", !m.playing);
  } else if (p === "timer") {
    setHTML(lead, `<div class="glyph orange">${I.timer}</div>`, "timer");
    setHTML(trail, `<div class="lbl orange" id="c-timer"></div>`, "timerlbl");
    $("c-timer").textContent = fmtTimer(timerLeft());
  } else if (p === "phone") {
    setHTML(lead, `<div class="glyph">${I.phone}</div>`, "phone");
    setHTML(trail, `<div class="lbl green" id="c-pct"></div>${batteryHTML("c-batt")}`, "phonelbl");
    $("c-pct").textContent = S.phone.battery + "%";
    updBattery($("c-batt"), S.phone);
  }
}

/* ---------- flèches : choisir ce qu'affiche l'île en mode compact ---------- */
function arrowsShown() { return !!S.arrows; }
function switchActivity(dir) {
  const list = baseActivities();
  if (list.length < 2) return;
  const cur = activities()[0];
  const i = Math.max(0, list.indexOf(cur));
  S.pin = list[(i + dir + list.length) % list.length];
  const v = document.querySelector('[data-view="compact"]');
  v.classList.remove("swap-next", "swap-prev");
  void v.offsetWidth;
  v.classList.add(dir > 0 ? "swap-next" : "swap-prev");
  const a = dir > 0 ? arrR : arrL;
  a.classList.remove("tap"); void a.offsetWidth; a.classList.add("tap");
  render();
}
arrL.addEventListener("click", () => switchActivity(-1));
arrR.addEventListener("click", () => switchActivity(1));
let wheelAt = 0;
for (const a of [arrL, arrR]) a.addEventListener("wheel", (e) => {
  e.preventDefault();
  if (Date.now() - wheelAt < 280) return;
  wheelAt = Date.now();
  switchActivity(e.deltaY > 0 ? 1 : -1);
}, { passive: false });

/* Points sous l'île : combien d'activités, laquelle est affichée */
function renderPager(show) {
  pager.classList.toggle("show", show);
  if (!show) return;
  const list = baseActivities(), cur = activities()[0];
  if (pager.children.length !== list.length) pager.innerHTML = list.map(() => "<i></i>").join("");
  [...pager.children].forEach((d, i) => d.classList.toggle("on", list[i] === cur));
}

/* ---------- téléchargements ---------- */
function fmtBytes(b) {
  if (b < 1024) return b + " o";
  const u = ["Ko", "Mo", "Go", "To"];
  let i = -1;
  do { b /= 1024; i++; } while (b >= 1024 && i < u.length - 1);
  return (b >= 100 ? Math.round(b) : b.toFixed(1)).toString().replace(".", ",") + " " + u[i];
}
function dlLabel(d) {
  if (d.live) return "REC " + fmtBytes(d.size);
  if (d.queued) return "En attente";
  if (d.post) return "Finalisation";
  if (d.pct != null) return Math.round(d.pct * 100) + " %";
  return d.speed > 0 ? fmtBytes(d.speed) + "/s" : fmtBytes(d.size);
}
function renderDls() {
  const box = $("x-dls");
  const list = S.dls.slice(0, 2);
  box.style.display = list.length && !S.clipOpen && !S.notifOpen && !S.mixerOpen && !S.picking && !S.gr.open ? "" : "none";
  setHTML(box, list.map((d) => `<div class="dl-row"><div class="dl-ic">${I.download}</div>
      <div class="dl-t"><b>${esc(d.name)}</b><span>${d.pct != null ? Math.round(d.pct * 100) + " % · " : d.post ? "Finalisation · " : d.queued ? "En attente · " : ""}${fmtBytes(d.size)}${d.speed > 0 ? " · " + fmtBytes(d.speed) + "/s" : d.pct != null || d.post || d.queued ? "" : " · en attente"}</span></div>
      <div class="dl-track${d.pct != null ? " det" : ""}"><i${d.pct != null ? ` style="width:${(d.pct * 100).toFixed(1)}%"` : ""}></i></div></div>`).join(""), JSON.stringify(list));
}
listen("downloads", (e) => {
  // nouveau téléchargement : l'île se met dessus avec un petit rebond (dlsChanged)
  S.bdls = e.payload || [];
  dlsChanged();
});
listen("download-done", (e) => {
  const d = e.payload;
  pushTransient("alert", {
    app: "Téléchargements", title: "Téléchargement terminé", body: `${d.name} · ${fmtBytes(d.size)}`,
    icon: d.thumb || null, svg: d.thumb ? null : `<div class="dl-done">${I.check}</div>`, color: [212, 90],
    onClick: () => invoke("reveal_download", { path: d.path }).catch(() => {}),
  }, 7000, true);
});

/* ---------- salon vocal Discord ---------- */
/* Carte complète (avatars + noms) quand il y a de la place ; une seule ligne sous une musique */
function voiceSlim() { return hasMedia(); }
function voiceH() { return voiceSlim() ? 54 : 116; }
function voiceOn() { return !!(S.voice && S.voice.channel); }
/* Qui parle (les autres d'abord), sinon personne */
function voiceSpeaker() {
  const v = S.voice;
  if (!v) return null;
  return v.members.find((m) => m.speaking && m.id !== v.me) || v.members.find((m) => m.speaking) || null;
}
function voiceOthers() {
  const v = S.voice;
  return v.members.filter((m) => m.id !== v.me);
}
function voiceTrailWidth() {
  const v = S.voice, n = Math.min(3, voiceOthers().length);
  let w = (v.deaf ? 22 : v.mute ? 22 : 0);
  if (n) w += 18 + (n - 1) * 11;
  if (voiceOthers().length > 3) w += 22;
  return Math.max(w, 18);
}
function voiceTrailHTML() {
  const v = S.voice, others = voiceOthers();
  const flag = v.deaf ? `<div class="vc-flag">${I.headOff}</div>` : v.mute ? `<div class="vc-flag">${I.micOff}</div>` : "";
  const stack = others.slice(0, 3).map((m) => `<img class="${m.speaking ? "sp" : ""}" src="${esc(m.avatar)}" alt="">`).join("");
  const more = others.length > 3 ? `<span class="vc-more">+${others.length - 3}</span>` : "";
  return `<div class="vc-trail">${flag}${stack ? `<div class="vc-stack">${stack}</div>` : ""}${more}</div>`;
}
function voiceTrailKey() {
  const v = S.voice;
  return "vt" + v.mute + v.deaf + voiceOthers().slice(0, 4).map((m) => m.id + m.speaking + m.avatar.length).join();
}
function renderVoice() {
  const box = $("x-voice");
  const on = voiceOn() && !S.clipOpen && !S.notifOpen && !S.mixerOpen && !S.picking;
  box.style.display = on ? "" : "none";
  if (!on) return;
  const v = S.voice, c = v.channel;
  const n = v.members.length;
  const sub = [c.guild, n ? `${n} personne${n > 1 ? "s" : ""}` : null].filter(Boolean).join(" · ");
  const shown = n > 8 ? v.members.slice(0, 7) : v.members;
  const slim = voiceSlim();
  if (slim) {
    // une ligne : salon, avatars empilés (anneau vert = parle), boutons
    const st = v.members.slice(0, 4).map((m) => `<img class="${m.speaking ? "sp" : ""}" src="${esc(m.avatar)}" title="${esc(m.id === v.me ? "Toi" : m.name)}" alt="">`).join("");
    const sp = voiceSpeaker();
    const sub2 = sp ? `${sp.id === v.me ? "Tu parles" : sp.name + " parle"}` : sub;
    setHTML(box, `<div class="vc slim">
        <div class="vc-head">
          <div class="vc-ic">${I.voice}</div>
          <div class="vc-t"><b>${esc(c.name)}</b><span class="${sp ? "talk" : ""}">${esc(sub2)}</span></div>
          <div class="vc-stack big">${st}${n > 4 ? `<span class="vc-more">+${n - 4}</span>` : ""}</div>
          <button class="vc-btn${v.mute ? " off" : ""}" data-vc="mute" title="${v.mute ? "Réactiver le micro" : "Couper le micro"}">${v.mute ? I.micOff : I.mic}</button>
          <button class="vc-btn${v.deaf ? " off" : ""}" data-vc="deaf" title="${v.deaf ? "Réactiver le son" : "Couper le son (casque)"}">${v.deaf ? I.headOff : I.headphones}</button>
          <button class="vc-btn leave" data-vc="leave" title="Quitter le salon">${I.hangup}</button>
        </div>
      </div>`, "slim" + JSON.stringify([c, v.mute, v.deaf, v.members]));
    return;
  }
  const people = shown.map((m) => `<div class="vc-p${m.speaking ? " sp" : ""}" title="${esc(m.name)}${m.deaf ? " · son coupé" : m.mute ? " · micro coupé" : ""}">
      <div class="vc-av"><img src="${esc(m.avatar)}" alt=""></div>
      ${m.deaf ? `<div class="vc-badge">${I.headOff}</div>` : m.mute ? `<div class="vc-badge">${I.micOff}</div>` : ""}
      <span>${esc(m.id === v.me ? "Toi" : m.name)}</span></div>`).join("")
    + (n > 8 ? `<div class="vc-p"><div class="vc-plus">+${n - 7}</div></div>` : "");
  setHTML(box, `<div class="vc">
      <div class="vc-head">
        <div class="vc-ic">${I.voice}</div>
        <div class="vc-t"><b>${esc(c.name)}</b><span>${esc(sub)}</span></div>
        <button class="vc-btn${v.mute ? " off" : ""}" data-vc="mute" title="${v.mute ? "Réactiver le micro" : "Couper le micro"}">${v.mute ? I.micOff : I.mic}</button>
        <button class="vc-btn${v.deaf ? " off" : ""}" data-vc="deaf" title="${v.deaf ? "Réactiver le son" : "Couper le son (casque)"}">${v.deaf ? I.headOff : I.headphones}</button>
        <button class="vc-btn leave" data-vc="leave" title="Quitter le salon">${I.hangup}</button>
      </div>
      <div class="vc-people">${people}</div>
    </div>`, JSON.stringify([c, v.mute, v.deaf, v.members]));
}
$("x-voice").addEventListener("click", (e) => {
  const b = e.target.closest("[data-vc]");
  if (!b || !S.voice) return;
  e.stopPropagation();
  const a = b.dataset.vc;
  // retour immédiat (Discord confirme juste après)
  if (a === "mute") S.voice.mute = !S.voice.mute;
  if (a === "deaf") S.voice.deaf = !S.voice.deaf;
  if (a === "leave") $("x-voice").classList.add("left");
  render();
  invoke("discord_action", { action: a }).catch((err) => {
    $("x-voice").classList.remove("left");
    pushTransient("alert", { app: "Discord", title: "Salon vocal", body: String(err), svg: `<div class="vc-ic" style="width:30px;height:30px">${I.voice}</div>`, color: [235, 86] }, 5000);
  });
});
function applyVoice(v) {
  const prev = S.voice && S.voice.channel ? S.voice.channel.id : null;
  // même salon : qui arrive, qui part (pas au moment où on rejoint soi-même)
  if (prev && v && v.channel && v.channel.id === prev) {
    const before = new Map(S.voice.members.map((m) => [m.id, m]));
    const after = new Map(v.members.map((m) => [m.id, m]));
    const me = v.me;
    const color = [235, 86];
    let n = 0;
    for (const [id, m] of after) if (id !== me && !before.has(id) && n++ < 3) pushTransient("pill", { name: m.name, avatar: m.avatar, join: true, color }, 2600);
    for (const [id, m] of before) if (id !== me && !after.has(id) && n++ < 3) pushTransient("pill", { name: m.name, avatar: m.avatar, join: false, color }, 2600);
  }
  S.voice = v || null;
  const now = S.voice && S.voice.channel ? S.voice.channel.id : null;
  if (now !== prev) $("x-voice").classList.remove("left");
  if (now && now !== prev) {
    // on rejoint un salon : l'île se met dessus
    S.pin = "voice";
    island.classList.remove("bump"); void island.offsetWidth; island.classList.add("bump");
  }
  if (!now && S.pin === "voice") S.pin = null;
  render();
}
listen("discord", (e) => applyVoice(e.payload));
invoke("discord_state").then((v) => { if (v) applyVoice(v); }).catch(() => {});

/* ---------- volume / luminosité (touches du clavier) ---------- */
function showOsd(data) {
  if (S.hover) return; // l'île est ouverte : pas d'interruption
  if (S.current && S.current.kind === "osd") {
    S.current.data = data;
    fillTransient(S.current);
    scheduleEnd(1500);
    return;
  }
  const item = { kind: "osd", data, ms: 1500 };
  if (S.current && S.current.kind === "secret") { S.queue.unshift(item); return; }
  // la notif en cours reprendra juste après
  if (S.current) S.queue.unshift({ ...S.current, ms: Math.max(2500, S.current.ms) });
  S.queue.unshift(item);
  showNext();
}
listen("osd", (e) => showOsd(e.payload));

/* ---------- paroles synchronisées ---------- */
function isMusic(m) {
  return m && m.duration > 30 && m.artist && !/zen|firefox|chrome|edge|opera|brave|twitch|youtube|discord/i.test((m.app || "") + " " + (m.id || ""));
}
function maybeFetchLyrics() {
  const m = S.media;
  if (!isMusic(m) || !m.title) { if (S.lyrics.key) S.lyrics = { key: null, lines: null }; return; }
  const key = m.artist + "|" + m.title;
  if (key === S.lyrics.key) return;
  S.lyrics = { key, lines: null, maxW: 0 };
  S._lyIdx = null;
  invoke("lyrics", { artist: m.artist || "", title: m.title, album: m.album || "", duration: m.duration || 0 })
    .then((lines) => {
      if (S.lyrics.key !== key) return;
      S.lyrics.lines = lines && lines.length ? lines : null;
      render();
    })
    .catch(() => {});
}
function lyricsOn() {
  return hasMedia() && !!S.lyrics.lines && S.lyrics.key === S.media.artist + "|" + S.media.title;
}
function lyricsCompactOn() {
  return S.lyricsCompact && lyricsOn() && S.media.playing;
}
function lyricsMaxW() {
  if (!S.lyrics.maxW) S.lyrics.maxW = Math.max(0, ...S.lyrics.lines.map((l) => textWidth(l.text, 250)));
  return S.lyrics.maxW;
}
function lyricIndex() {
  const lines = S.lyrics.lines, p = mediaPos(S.media) + 0.25;
  let i = -1;
  for (let k = 0; k < lines.length && lines[k].t <= p; k++) i = k;
  return i;
}
/* Ligne actuelle (off = 0) ou suivante non vide (off = 1) */
function lyricLine(off) {
  if (!lyricsOn()) return "";
  const lines = S.lyrics.lines, i = lyricIndex();
  if (off === 0) return i >= 0 ? lines[i].text || "♪" : "";
  for (let k = i + 1; k < lines.length; k++) if (lines[k].text) return lines[k].text;
  return "";
}
/* Avancement du chant dans la ligne en cours (0 → 1). Les paroles LRCLIB ne sont datées qu'à la ligne :
   on estime la durée chantée d'après la longueur de la ligne (≈ 75 ms par lettre), sans dépasser la
   ligne suivante, pour que la surbrillance suive la voix plutôt que le silence qui suit. */
function lyricProgress() {
  const lines = S.lyrics.lines, i = lyricIndex();
  if (i < 0 || !lines[i].text) return 0;
  const t0 = lines[i].t, pos = mediaPos(S.media) + 0.25;
  let t1 = Infinity;
  for (let k = i + 1; k < lines.length; k++) if (lines[k].t > t0) { t1 = lines[k].t; break; }
  const sung = Math.min((t1 - t0) * 0.92, Math.max(1.2, lines[i].text.length * 0.075 + 0.4));
  return Math.max(0, Math.min(1, (pos - t0) / sung));
}
/* Ligne de paroles « karaoké » : la partie déjà chantée s'allume progressivement (dégradé animé). */
function setKaraoke(el, text) {
  const k = el.firstElementChild;
  if (k && k.classList.contains("kar") && k.textContent === text) return;
  if (text === "♪") { el.textContent = text; return; }
  el.innerHTML = `<span class="kar">${esc(text)}</span>`;
  el.firstElementChild.style.setProperty("--kp", "0%");
}
function karaokeTick(el) {
  const k = el && el.firstElementChild;
  if (!k || !k.classList.contains("kar")) return;
  // la bordure douce du dégradé (14 %) doit sortir entièrement à la fin
  k.style.setProperty("--kp", (lyricProgress() * 114).toFixed(1) + "%");
}
function updateLyric(force) {
  if (!lyricsOn()) return;
  const i = lyricIndex();
  karaokeTick($("ly-cur"));
  if (i === S._lyIdx && !force) return;
  const changed = i !== S._lyIdx;
  S._lyIdx = i;
  const cur = i >= 0 ? S.lyrics.lines[i].text || "♪" : "♪";
  if (cur === "♪") $("ly-cur").textContent = cur;
  else { setKaraoke($("ly-cur"), cur); karaokeTick($("ly-cur")); }
  $("ly-nxt").textContent = lyricLine(1);
  if (changed && !force) { const b = $("x-lyric"); b.classList.remove("swap"); void b.offsetWidth; b.classList.add("swap"); }
}

/* ---------- gestes sur la musique (vue étendue) ---------- */
const xTop = document.querySelector(".x-top");
let volHintT = null, volBusy = false, volPending = 0, swipeAcc = 0, swipeAt = 0;
function volHint(v, own) {
  const h = $("x-volhint");
  h.innerHTML = `${v === 0 ? I.mute : I.volume}<span>${Math.round(v)} %</span>`;
  h.title = own ? "Volume de l'appli" : "Volume Windows";
  h.classList.add("show");
  clearTimeout(volHintT);
  volHintT = setTimeout(() => h.classList.remove("show"), 1000);
}
function sendAppVolume() {
  if (volBusy || !volPending || !S.media) return;
  const delta = volPending;
  volPending = 0;
  volBusy = true;
  invoke("media_volume", { id: S.media.id, app: S.media.app || "", delta })
    .then((r) => volHint(r.volume * 100, r.app))
    .catch(() => {})
    .finally(() => { volBusy = false; sendAppVolume(); });
}
function swipeTrack(dir) {
  if (!S.media) return;
  invoke("media_control", { id: S.media.id, action: dir > 0 ? "next" : "prev" }).catch(() => {});
  xTop.classList.remove("sw-next", "sw-prev"); void xTop.offsetWidth;
  xTop.classList.add(dir > 0 ? "sw-next" : "sw-prev");
}
xTop.addEventListener("wheel", (e) => {
  if (!hasMedia()) return;
  e.preventDefault();
  // glissé horizontal (pavé tactile / molette inclinable) : morceau suivant / précédent
  if (Math.abs(e.deltaX) > Math.abs(e.deltaY)) {
    swipeAcc += e.deltaX;
    if (Math.abs(swipeAcc) > 90 && Date.now() - swipeAt > 700) { swipeAt = Date.now(); swipeTrack(swipeAcc > 0 ? 1 : -1); swipeAcc = 0; }
    return;
  }
  const up = e.deltaY < 0;
  if (volSrc()) { setSpVolume(curVol() + (up ? 4 : -4)); volHint(curVol(), true); return; }
  volPending += up ? 0.04 : -0.04;
  sendAppVolume();
}, { passive: false });
// glisser la pochette / le titre à gauche ou à droite = morceau suivant / précédent ; clic sur la pochette = ouvrir l'appli
let drag = null;
xTop.addEventListener("pointerdown", (e) => {
  if (!hasMedia() || e.button !== 0 || e.target.closest("button")) return;
  drag = { x: e.clientX, dx: 0, art: !!e.target.closest(".art-wrap") };
  xTop.setPointerCapture(e.pointerId);
});
xTop.addEventListener("pointermove", (e) => {
  if (!drag) return;
  drag.dx = e.clientX - drag.x;
  if (Math.abs(drag.dx) > 4) {
    xTop.classList.add("drag");
    const t = `translateX(${drag.dx * 0.45}px)`, o = String(Math.max(0.35, 1 - Math.abs(drag.dx) / 260));
    for (const el of xTop.querySelectorAll(".art-wrap, .meta")) { el.style.transform = t; el.style.opacity = o; }
  }
});
function endDrag(e) {
  if (!drag) return;
  const d = drag;
  drag = null;
  xTop.classList.remove("drag");
  for (const el of xTop.querySelectorAll(".art-wrap, .meta")) { el.style.transform = ""; el.style.opacity = ""; }
  if (Math.abs(d.dx) > 55) swipeTrack(d.dx < 0 ? 1 : -1);
  else if (Math.abs(d.dx) < 5 && d.art && e.type === "pointerup" && S.media) {
    invoke("media_open", { id: S.media.id, app: S.media.app || "" }).catch((err) =>
      pushTransient("alert", { app: S.media.app || "Musique", title: "Ouvrir l'appli", body: String(err), svg: `<div style="width:26px">${I.note}</div>` }, 4000));
  }
}
xTop.addEventListener("pointerup", endDrag);
xTop.addEventListener("pointercancel", endDrag);

/* ---------- presse-papiers ---------- */
function clipsShown() {
  const q = S.clipQ.trim().toLowerCase();
  if (!q) return S.clips;
  return S.clips.filter((c) => (c.kind === "image" ? "image " + c.w + "x" + c.h : c.text).toLowerCase().includes(q));
}
function clipPanelHeight() {
  const list = clipsShown();
  let h = 0;
  if (!list.length) h += 76;
  for (const c of list) h += c.kind === "image" ? 112 : 52;
  if (list.some((c) => c.pinned) && list.some((c) => !c.pinned)) h += 20;
  return Math.min(370, 18 + 30 + 38 + Math.min(h, 236) + 14 + 52);
}
function clipRow(c) {
  return `<div class="cb-row ${c.kind}${c.pinned ? " pinned" : ""}" data-cb="${c.id}" title="Cliquer pour recopier">
      ${c.kind === "image" && c.thumb ? `<img class="cb-img" src="${c.thumb}" alt="">` : `<div class="cb-text">${esc(c.text)}</div>`}
      <div class="cb-meta"><span>${c.kind === "image" ? `${c.w} × ${c.h}` : ""}</span><span>${c.pinned ? "Épinglé" : ago(c.at)}</span></div>
      <button class="cb-pin${c.pinned ? " on" : ""}" data-cbpin="${c.id}" title="${c.pinned ? "Désépingler" : "Épingler (gardé même après redémarrage)"}">${I.pin}</button>
      <button class="cb-x" data-cbrm="${c.id}" title="Retirer">${I.close}</button>
      <div class="cb-ok">${I.check}<span>Copié</span></div>
    </div>`;
}
function renderClips() {
  const list = clipsShown();
  $("cb-clear").style.visibility = S.clips.some((c) => !c.pinned) ? "" : "hidden";
  const key = list.map((c) => c.id + ":" + c.at + (c.pinned ? "p" : "")).join(",") + "|" + S.clipQ + "|" + Math.floor(Date.now() / 30000);
  const pins = list.filter((c) => c.pinned), rest = list.filter((c) => !c.pinned);
  let html;
  if (list.length) {
    html = pins.map(clipRow).join("") + (pins.length && rest.length ? `<div class="cb-sep">Récents</div>` : "") + rest.map(clipRow).join("");
  } else if (S.clipQ.trim()) {
    html = `<div class="cb-empty">Aucun résultat pour « ${esc(S.clipQ.trim())} »</div>`;
  } else {
    html = `<div class="cb-empty">Ce que tu copies apparaîtra ici.<br><span>Gardé en mémoire seulement ; épingle un élément pour le garder.</span></div>`;
  }
  setHTML($("cb-list"), html, key);
}
/* Taper dans la recherche : l'île prend le clavier le temps de la recherche, puis le rend */
const cbQ = $("cb-q");
$("cb-sic").innerHTML = I.search;
function grabKeyboard(el = cbQ) {
  S.kbdEl = el;
  if (S.kbd) { el.focus(); return; }
  S.kbd = true;
  invoke("island_focus", { on: true }).then(() => el.focus()).catch(() => {});
}
function releaseKeyboard() {
  S.clipQ = "";
  cbQ.value = "";
  if (!S.kbd) return;
  S.kbd = false;
  cbQ.blur();
  $("cl-q").blur();
  $("mu-q").blur();
  grQ.blur();
  document.querySelectorAll(".gr-time").forEach((i) => i.blur());
  invoke("island_focus", { on: false }).catch(() => {});
}
cbQ.addEventListener("mousedown", () => grabKeyboard(cbQ));
cbQ.addEventListener("input", () => { S.clipQ = cbQ.value; renderClips(); render(); });
cbQ.addEventListener("keydown", (e) => {
  if (e.key === "Escape") {
    if (cbQ.value) { cbQ.value = ""; S.clipQ = ""; renderClips(); render(); }
    else releaseKeyboard();
  } else if (e.key === "Enter") {
    const first = clipsShown()[0];
    if (first) invoke("clip_copy", { id: first.id }).then(() => { const r = document.querySelector(`[data-cb="${first.id}"]`); if (r) { r.classList.remove("copied"); void r.offsetWidth; r.classList.add("copied"); } }).catch(() => {});
  }
});
$("x-clips").addEventListener("click", (e) => {
  const t = e.target.closest("[data-cbclear], [data-cbrm], [data-cbpin], [data-cb]");
  if (!t) return;
  e.stopPropagation();
  if (t.dataset.cbclear) return invoke("clip_clear").catch(() => {});
  if (t.dataset.cbpin) {
    const c = S.clips.find((x) => x.id === +t.dataset.cbpin);
    return invoke("clip_pin", { id: +t.dataset.cbpin, on: !(c && c.pinned) }).catch((err) =>
      pushTransient("alert", { app: "Presse-papiers", title: "Épingle", body: String(err), svg: `<div style="width:26px;color:var(--orange)">${I.pin}</div>` }, 4000));
  }
  if (t.dataset.cbrm) return invoke("clip_remove", { id: +t.dataset.cbrm }).catch(() => {});
  const row = t;
  invoke("clip_copy", { id: +row.dataset.cb }).then(() => {
    row.classList.remove("copied"); void row.offsetWidth; row.classList.add("copied");
  }).catch(() => {});
});
function grClipSeen(list, offer) {
  const c = (list || []).filter((x) => x.kind === "text").sort((a, b) => b.at - a.at)[0];
  if (!c) {
    // historique vide au démarrage : la toute première copie compte
    if (!offer && S.gr.lastClip == null) S.gr.lastClip = 0;
    return;
  }
  const first = S.gr.lastClip == null;
  if (!first && c.id <= S.gr.lastClip) return;
  S.gr.lastClip = c.id;
  const u = c.text.trim();
  if (!first && offer && Date.now() - c.at < 8000 && GR_SITES.test(u) && !/\s/.test(u)) grOffer(u);
}
listen("clipboard", (e) => {
  S.clips = e.payload || [];
  grClipSeen(S.clips, true);
  if (S.view === "expanded" && S.clipOpen) render();
});
invoke("clip_list").then((l) => { S.clips = l || []; grClipSeen(S.clips, false); }).catch(() => {});

/* ============================================================
   Ma musique : les musiques de l'île, lues par le lecteur natif (Rust + Windows).
   La lecture s'affiche ensuite dans l'île comme Spotify (pochette, paroles, aléatoire, répéter…).
   ============================================================ */
const muQ = $("mu-q"), muList = $("mu-list");
const MU_COV = {}; // id → data URL | "" (pas de pochette) | 1 (en cours)
$("mu-logo").innerHTML = I.music;
$("mu-sic").innerHTML = I.search;
function muShown() {
  const q = S.mu.q.trim().toLowerCase();
  if (!q) return S.mu.tracks;
  return S.mu.tracks.filter((t) => (t.title + " " + t.artist + " " + t.album).toLowerCase().includes(q));
}
function muPanelHeight() {
  const n = muShown().length;
  return Math.min(MAX_XH, 16 + 32 + 38 + Math.min(Math.max(n, 2) * 46, 276) + 10 + 62);
}
async function muLoad() {
  try {
    const r = await invoke("music_library");
    if (r) { S.mu.tracks = r.tracks || []; S.mu.now = r.now || S.mu.now; }
  } catch {}
  S.mu.loaded = true;
  if (S.mu.open) render();
}
function muPlaying() {
  return hasMedia() && S.media.app === "Ma musique" ? S.media : null;
}
function muRow(t, i) {
  const now = t.id === S.mu.now && muPlaying();
  const c = MU_COV[t.id];
  const art = typeof c === "string" && c ? `<img src="${c}" alt="">` : I.note;
  return `<div class="mu-row${now ? " now" : ""}" data-mu-i="${i}" title="${esc(t.title)}${t.artist ? " · " + esc(t.artist) : ""}">
      <div class="mu-art" data-mu-art="${esc(t.id)}">${art}</div>
      <div class="mu-meta"><div class="mu-t">${esc(t.title)}</div><div class="mu-a">${esc(t.artist || t.album || "—")}</div></div>
      ${now ? `<div class="eq${now.playing ? "" : " paused"}"><i></i><i></i><i></i><i></i></div>` : ""}
      <div class="mu-d">${t.duration > 0 ? fmtTime(t.duration) : ""}</div>
      ${delBtn("mu-del", t.id, "Supprimer ce titre (Corbeille)")}
    </div>`;
}

/* Suppression fichier par fichier : 1er clic = « Supprimer ? », 2e clic = à la Corbeille (récupérable). */
let delArm = null, delArmTimer = null;
function delBtn(scope, id, title) {
  const armed = delArm === scope + ":" + id;
  return `<button class="del-b${armed ? " armed" : ""}" data-del="${scope}" data-id="${esc(String(id))}" title="${armed ? "Cliquer encore pour supprimer" : title}">${I.trash}${armed ? "<span>Supprimer ?</span>" : ""}</button>`;
}
function delClick(btn, run) {
  const k = btn.dataset.del + ":" + btn.dataset.id;
  clearTimeout(delArmTimer);
  if (delArm !== k) {
    delArm = k;
    delArmTimer = setTimeout(() => { delArm = null; render(); }, 4000);
    return render();
  }
  delArm = null;
  run(btn.dataset.id);
}
function muDelete(id) {
  const t = S.mu.tracks.find((x) => x.id === id);
  S.mu.tracks = S.mu.tracks.filter((x) => x.id !== id); // tout de suite retiré de la liste
  delete MU_COV[id];
  render();
  invoke("music_delete", { id })
    .then(() => muLoad())
    .catch((err) => {
      muLoad();
      pushTransient("alert", { app: "Ma musique", title: t ? t.title : "Suppression", body: String(err), svg: `<div class="dl-done" style="background:#ff453a">${I.close}</div>`, color: [4, 90] }, 5000);
    });
}
function muRender() {
  const list = muShown();
  const playing = muPlaying();
  $("mu-count").textContent = S.mu.tracks.length ? S.mu.tracks.length + (S.mu.tracks.length > 1 ? " titres" : " titre") : "";
  const key = list.map((t) => t.id).join("|") + "#" + S.mu.now + (playing ? playing.playing : "") + "#" + S.mu.q + "#" + Object.keys(MU_COV).length + "#" + delArm;
  let html;
  if (list.length) html = list.map(muRow).join("");
  else if (!S.mu.loaded) html = `<div class="mu-empty">Chargement…</div>`;
  else if (S.mu.q.trim()) html = `<div class="mu-empty">Aucun résultat pour « ${esc(S.mu.q.trim())} »</div>`;
  else html = `<div class="mu-empty">Aucune musique pour l'instant.<br><span>Télécharge un son en MP3, puis clique sur ♪ à côté de lui : il apparaîtra ici.</span></div>`;
  setHTML(muList, html, key);
  // pochettes : seulement celles qui manquent, une à la fois
  muCovers(list);
}
let muCovBusy = false;
async function muCovers(list) {
  if (muCovBusy) return;
  muCovBusy = true;
  try {
    for (const t of list.slice(0, 80)) {
      if (!S.mu.open) break;
      if (MU_COV[t.id] !== undefined) continue;
      MU_COV[t.id] = 1;
      const c = await invoke("music_cover", { id: t.id }).catch(() => null);
      MU_COV[t.id] = c || "";
      if (c) {
        const box = [...muList.querySelectorAll("[data-mu-art]")].find((b) => b.dataset.muArt === t.id);
        if (box) box.innerHTML = `<img src="${c}" alt="">`;
      }
    }
  } finally {
    muCovBusy = false;
  }
}
function muPlay(i, shuffle = false) {
  const list = muShown();
  if (!list.length) return;
  if (shuffle) i = Math.floor(Math.random() * list.length);
  S.mu.now = list[i].id;
  muRender();
  invoke("music_play", { ids: list.map((t) => t.id), start: i, shuffle }).catch((err) =>
    pushTransient("alert", { app: "Ma musique", title: list[i].title, body: String(err), svg: `<div style="width:26px;color:#ff6b8b">${I.music}</div>` }, 5000));
}
muQ.addEventListener("mousedown", () => grabKeyboard(muQ));
muQ.addEventListener("input", () => { S.mu.q = muQ.value; render(); });
muQ.addEventListener("keydown", (e) => {
  if (e.key === "Escape") {
    if (muQ.value) { muQ.value = ""; S.mu.q = ""; render(); }
    else releaseKeyboard();
  } else if (e.key === "Enter") muPlay(0);
});
$("x-music").addEventListener("click", (e) => {
  const d = e.target.closest("[data-del]");
  if (d) { e.stopPropagation(); return delClick(d, muDelete); }
  const t = e.target.closest("[data-mu], [data-mu-i]");
  if (!t) return;
  e.stopPropagation();
  if (t.dataset.mu === "dir") return invoke("music_folder").catch(() => {});
  if (t.dataset.mu === "shuffle") return muPlay(0, true);
  muPlay(+t.dataset.muI);
});
listen("music-now", (e) => {
  S.mu.now = e.payload;
  if (S.mu.open) muRender();
});

/* Boutons de l'île : chacun peut être masqué dans les réglages (le bouton Réglages reste toujours) */
function renderToolButtons() {
  let n = 0;
  document.querySelectorAll(".tools [data-tool]").forEach((b) => {
    const k = b.dataset.tool;
    if (k === "timer") { if (b.style.display !== "none") n++; return; }
    const hide = k !== "settings" && S.hiddenTools.has(k);
    b.style.display = hide ? "none" : "";
    if (!hide) n++;
  });
  document.querySelector(".tools").classList.toggle("many", n >= 10);
}

/* ============================================================
   Claude Code : mini-session dans l'île (la CLI claude du PC)
   ============================================================ */
const clQ = $("cl-q"), clLog = $("cl-log"), clSendBtn = $("cl-send");
$("cl-logo").innerHTML = I.claude;
let clRaf = 0, clStatusAt = 0;

function clModelName(m) {
  const r = /^claude-([a-z]+)-(\d+)(?:-(\d{1,2}))?(?:-|$)/.exec(m || "");
  if (!r) return m || "";
  return r[1][0].toUpperCase() + r[1].slice(1) + " " + r[2] + (r[3] ? "." + r[3] : "");
}
function clLast() {
  const m = S.cl.msgs[S.cl.msgs.length - 1];
  return m && m.a ? m : null;
}
function clText(m) {
  return m.parts.filter((p) => p.k === "text").map((p) => p.s.trim()).filter(Boolean).join("\n\n");
}
function clWhen(v) {
  if (v == null) return "";
  const d = new Date(typeof v === "number" ? (v < 1e12 ? v * 1000 : v) : v);
  if (isNaN(d)) return "";
  const hm = d.toLocaleTimeString(LOC, { hour: "2-digit", minute: "2-digit" });
  return d.toDateString() === new Date().toDateString() ? (LOC === "fr-FR" ? "à " : "at ") + hm : d.toLocaleDateString(LOC, { weekday: "short" }) + " " + hm;
}

/* quotas restants : fenêtre de 5 h + la plus chargée des limites hebdo */
const CL_QLABEL = { five_hour: "5 h", seven_day: "Semaine", seven_day_opus: "Opus sem.", seven_day_sonnet: "Sonnet sem." };
function clQuotas() {
  let list = (S.cl.usage || []).filter((q) => CL_QLABEL[q.key] && typeof q.pct === "number");
  const r = S.cl.rate;
  if (!list.length && r && typeof r.utilization === "number") {
    list = [{ key: r.rateLimitType in CL_QLABEL ? r.rateLimitType : "five_hour", pct: r.utilization <= 1 ? r.utilization * 100 : r.utilization, resets_at: r.resetsAt }];
  }
  const five = list.find((q) => q.key === "five_hour");
  const week = list.filter((q) => q.key !== "five_hour").sort((a, b) => b.pct - a.pct)[0];
  return [five, week].filter(Boolean);
}
function clQuotaHTML() {
  return clQuotas().map((q) => {
    const left = Math.max(0, Math.min(100, Math.round(100 - q.pct)));
    const cls = left <= 8 ? " full" : left <= 25 ? " warn" : "";
    const when = clWhen(q.resets_at);
    return `<div class="cl-q${cls}" title="${CL_QLABEL[q.key]} : ${Math.round(q.pct)} % utilisé${when ? " · réinitialisé " + when : ""}">
      <span>${CL_QLABEL[q.key]}<b>${left} %</b></span><i style="--p:${left}%"></i></div>`;
  }).join("");
}

function clMd(s) {
  return s.split("```").map((p, i) => {
    if (i % 2) {
      const nl = p.indexOf("\n");
      const code = nl >= 0 && /^[\w+#.-]*$/.test(p.slice(0, nl).trim()) ? p.slice(nl + 1) : p;
      return `<pre>${esc(code.replace(/\n$/, ""))}</pre>`;
    }
    if (i > 0) p = p.replace(/^\n/, "");
    p = p.replace(/\n$/, "");
    return esc(p)
      .replace(/`([^`\n]+)`/g, "<code>$1</code>")
      .replace(/\*\*([^*\n]+)\*\*/g, "<b>$1</b>")
      .replace(/^#{1,4} (.*)$/gm, '<span class="h">$1</span>')
      .replace(/^(\s*)[-*] /gm, "$1• ");
  }).join("");
}

function clMsgHTML(m, i) {
  if (!m.a) return `<div class="cl-u">${esc(m.u)}</div>`;
  let h = "";
  for (const p of m.parts) {
    if (p.k === "text") { if (p.s.trim()) h += `<div class="cl-t">${clMd(p.s.trim())}</div>`; }
    else h += `<div class="cl-tool">${I.wrench}<b>${esc(p.name)}</b><span>${esc(p.detail || "")}</span></div>`;
  }
  const last = m.parts[m.parts.length - 1];
  if (!m.done && (!last || last.k !== "text" || !last.s.trim())) h += `<div class="cl-dots"><i></i><i></i><i></i></div>`;
  if (m.err) {
    h += m.login
      ? `<div class="cl-err">Claude Code n'est pas connecté sur ce PC. <button class="copy-btn" data-cl="login">Se connecter</button></div>`
      : `<div class="cl-err">${esc(m.err)}</div>`;
  }
  if (m.done) {
    const tools = m.parts.filter((p) => p.k === "tool").length;
    const meta = [m.stopped ? "Arrêté" : "", m.ms ? Math.max(1, Math.round(m.ms / 1000)) + " s" : "", tools ? tools + (tools > 1 ? " actions" : " action") : ""].filter(Boolean).join(" · ");
    if (meta) h += `<div class="cl-meta">${meta}</div>`;
    if (clText(m)) h += `<button class="cl-copy" data-cl="copy" data-i="${i}">Copier</button>`;
  }
  return `<div class="cl-a">${h}</div>`;
}

function clEmptyHTML() {
  const st = S.cl.status;
  if (!st) return `<div class="cl-empty"><div class="cl-dots" style="justify-content:center"><i></i><i></i><i></i></div></div>`;
  if (st.installed === false) return `<div class="cl-empty"><b>Claude Code n'est pas installé</b><br>Installe la CLI <code>claude</code>, puis rouvre ce panneau.</div>`;
  if (!st.logged_in) return `<div class="cl-empty"><b>Connecte Claude Code</b><br>Une console va s'ouvrir : connecte-toi dans le navigateur, puis reviens ici.
      <button class="open-btn" data-cl="login">Se connecter</button><button class="copy-btn" data-cl="recheck">C'est fait</button></div>`;
  return `<div class="cl-empty">Pose une question ou demande une tâche.<br>Claude peut <b>modifier des fichiers et lancer des commandes</b> sans demander, dans<br><span class="cl-dir">${esc(st.dir || "")}</span></div>`;
}

function clRenderNow() {
  clRaf = 0;
  document.querySelector('[data-tool="claude"]').classList.toggle("cl-busy", S.cl.running);
  if (!(S.hover && S.cl.open)) return;
  $("cl-model").textContent = clModelName(S.cl.model || (S.cl.status && S.cl.status.model));
  const qh = clQuotaHTML();
  setHTML($("cl-quota"), qh, qh);
  const atBottom = clLog.scrollHeight - clLog.scrollTop - clLog.clientHeight < 48;
  const html = S.cl.msgs.length ? S.cl.msgs.map(clMsgHTML).join("") : clEmptyHTML();
  if (clLog._h !== html) {
    clLog._h = html;
    clLog.innerHTML = html;
    if (atBottom || S.cl.stick) clLog.scrollTop = clLog.scrollHeight;
    S.cl.stick = false;
  }
  clSendBtn.innerHTML = S.cl.running ? I.stop : I.send;
  clSendBtn.classList.toggle("stop", S.cl.running);
  clSendBtn.title = S.cl.running ? "Arrêter" : "Envoyer (Entrée)";
  clSendBtn.disabled = !S.cl.running && !(S.cl.status && S.cl.status.logged_in);
}
function clRender() {
  if (!clRaf) clRaf = setTimeout(clRenderNow, 30);
}

function clRefresh(force) {
  if (!force && Date.now() - clStatusAt < 20000) return;
  clStatusAt = Date.now();
  invoke("claude_status").then((st) => { S.cl.status = st; clRender(); }).catch(() => {});
  invoke("claude_usage").then((u) => { if (u) { S.cl.usage = u; clRender(); } }).catch(() => {});
}

function clAutosize() {
  clQ.style.height = "18px";
  clQ.style.height = Math.min(72, Math.max(18, clQ.scrollHeight - 10)) + "px";
}

function clSend() {
  const text = clQ.value.trim();
  if (!text || S.cl.running) return;
  S.cl.msgs.push({ u: text }, { a: true, parts: [], done: false });
  S.cl.running = true;
  S.cl.stick = true;
  clQ.value = "";
  clAutosize();
  invoke("claude_send", { prompt: text }).catch((e) => {
    const m = clLast();
    if (m) { m.done = true; m.err = String(e); }
    S.cl.running = false;
    clRender();
  });
  clRender();
}

function clNotify(m) {
  if (S.hover && S.cl.open) return;
  const body = m.err ? m.err : clText(m).replace(/\s+/g, " ").slice(0, 110) || "Tâche terminée";
  pushTransient("alert", {
    app: "Claude", title: m.err ? "Claude : problème" : "Claude a répondu", body,
    svg: `<div style="width:26px;color:#e8a184">${I.claude}</div>`,
    onClick: () => { S.cl.open = true; S.clipOpen = S.mixerOpen = S.notifOpen = S.picking = false; render(); },
  }, 7000);
}

listen("claude", (e) => {
  const ev = e.payload || {};
  const m = clLast();
  if (ev.t === "init") { if (ev.model) S.cl.model = ev.model; }
  else if (ev.t === "block") { if (m) m.parts.push({ k: "text", s: "" }); }
  else if (ev.t === "text") {
    if (m) {
      let p = m.parts[m.parts.length - 1];
      if (!p || p.k !== "text") m.parts.push((p = { k: "text", s: "" }));
      p.s += ev.s;
    }
  }
  else if (ev.t === "tool") { if (m) m.parts.push({ k: "tool", name: ev.name, detail: ev.detail }); }
  else if (ev.t === "rate") { S.cl.rate = ev.info; }
  else if (ev.t === "usage") { S.cl.usage = ev.usage; }
  else if (ev.t === "login") { if (m) m.login = true; if (S.cl.status) S.cl.status.logged_in = false; }
  else if (ev.t === "done") {
    S.cl.running = false;
    if (m && !m.done) {
      m.done = true;
      m.ms = ev.ms;
      m.stopped = !!ev.stopped;
      if (ev.login) m.login = true;
      if (ev.error && !ev.stopped) m.err = ev.error;
      clNotify(m);
    }
  }
  clRender();
});


/* ============================================================
   Télécharger : vidéo / MP3 / transcription depuis un lien (yt-dlp)
   ============================================================ */
const grQ = $("gr-q"), grBody = $("gr-body"), grList = $("gr-list"), grPasteBtn = $("gr-paste");
$("gr-logo").innerHTML = I.download;
$("gr-lic").innerHTML = I.link;
const GR_SITES = /^https?:\/\/([a-z0-9-]+\.)*(youtube\.com|youtu\.be|tiktok\.com|instagram\.com|x\.com|twitter\.com|reddit\.com|redd\.it|twitch\.tv|vimeo\.com|dailymotion\.com|dai\.ly|soundcloud\.com|bandcamp\.com|bsky\.app|facebook\.com|fb\.watch|kick\.com|threads\.net|threads\.com|streamable\.com|bilibili\.com|rumble\.com|mixcloud\.com|vk\.com|9gag\.com|imgur\.com|tumblr\.com|ted\.com|arte\.tv|france\.tv|tf1\.fr|6play\.fr|radiofrance\.fr|podcasts\.apple\.com|odysee\.com|nicovideo\.jp|open\.spotify\.com|spotify\.link|deezer\.com|deezer\.page\.link)(\/|$)/i;
const GR_GIF_MAX = 30;
/* lien déjà téléchargé : on ne le repropose plus */
function grUsed(u) {
  u = (u || "").trim();
  return !!u && (S.gr.used.has(u) || S.gr.jobs.some((j) => j.url === u && j.status !== "error" && j.status !== "stopped"));
}

function grActive(j) { return j.status === "queued" || j.status === "running" || j.status === "post"; }
function grQLabel(h) { return !h ? "Meilleure" : h >= 4320 ? "8K" : h >= 2160 ? "4K" : h + "p"; }
function grParseT(s) {
  s = String(s || "").trim().replace(",", ".");
  if (!s) return null;
  if (!/^\d+(?::\d{1,2}){0,2}(?:\.\d+)?$/.test(s)) return NaN;
  return s.split(":").reduce((a, x) => a * 60 + parseFloat(x), 0);
}
/* Mix YouTube : « Ce titre » ou les N premiers titres du mix. grEff() = ce qui sera vraiment téléchargé. */
const GR_MIX_N = [10, 25, 50, 100];
function grEff() {
  const d = S.gr.info;
  if (!d || !d.mix || !S.gr.mixN) return d;
  return { ...d, url: d.mix.url, title: d.mix.title, playlist: S.gr.mixN, limit: S.gr.mixN, videos: [], gif: false, is_gif: false, subs: null, live: false, duration: null };
}
function grCanCut() {
  const d = grEff();
  return d && !d.music && !d.playlist && !d.live && d.duration > 0 && S.gr.kind !== "text";
}
/* durée gardée par l'extrait (pour estimer la taille) */
function grCutRatio() {
  const d = S.gr.info;
  if (!S.gr.cut || !grCanCut()) return 1;
  const a = grParseT(S.gr.a), b = grParseT(S.gr.b);
  const s0 = Number.isFinite(a) ? a : 0, e0 = Number.isFinite(b) ? Math.min(b, d.duration) : d.duration;
  return e0 > s0 ? (e0 - s0) / d.duration : 1;
}
function grKinds(d) {
  if (d.music) return [["audio", "MP3", I.note]];
  // GIF de X : GIF d'abord
  if (d.is_gif) return [["gif", "GIF", I.gif], ["video", "Vidéo", I.film]];
  const k = [];
  if (d.videos.length || d.playlist) k.push(["video", "Vidéo", I.film]);
  if (d.audio) k.push(["audio", "MP3", I.note]);
  if (d.gif && !d.playlist && !d.live) k.push(["gif", "GIF", I.gif]);
  if (d.subs && !d.playlist) k.push(["text", "Texte", I.textdoc]);
  return k;
}
function grVideoOpts(d) {
  if (d.playlist) return [0, 1080, 720, 480].map((h) => ({ h, size: 0 }));
  // la meilleure + 3 paliers (1440p sauté quand il y a de la 4K, pour tenir sur une ligne)
  const best = d.videos[0];
  return best ? [best, ...d.videos.slice(1).filter((v) => !(v.h === 1440 && best.h >= 2160)).slice(0, 3)] : [];
}
function grSize() {
  const d = grEff(), g = S.gr;
  if (!d || d.live || d.playlist) return 0;
  const r = grCutRatio();
  if (g.kind === "audio") return (d.audio_size || 0) * r;
  if (g.kind === "video") { const v = d.videos.find((x) => x.h === g.fmt) || d.videos[0]; return v ? v.size * r : 0; }
  return 0;
}
function grCardH() {
  const d = S.gr.info;
  if (!d) return 0;
  return 63 + (d.mix ? 8 + 28 : 0) + 8 + 30 + 8 + 28 + (S.gr.cut && grCanCut() ? 8 + 28 : 0) + 10 + 34;
}
function grPanelHeight() {
  const g = S.gr, st = g.status;
  let h = 16 + 30 + 8 + 38;
  if (st && !st.ready) h += 10 + 92;
  else if (g.probing || g.err) h += 10 + 50;
  else if (g.spHelp) h += 10 + 210;
  else if (g.ph) h += 10 + grPhH();
  else if (g.info) h += 10 + grCardH();
  else if (!g.jobs.length) h += 10 + 58;
  if (g.jobs.length) {
    // une ligne = 50 px, un mix déplié ajoute ses titres (26 px chacun) ; au-delà, la liste défile
    const list = g.jobs.reduce((s, j) => s + 50 + (g.unfold.has(j.id) && j.status === "done" && (j.items || []).length > 1 ? j.items.length * 26 - 1 : 0), 0);
    h += 12 + Math.min((g.info || g.probing || g.err || g.spHelp || g.ph ? 1 : 5) * 50, list);
  }
  return h + 62;
}

function grCardHTML() {
  const d0 = S.gr.info, d = grEff(), g = S.gr;
  const kinds = grKinds(d);
  if (!kinds.some((k) => k[0] === g.kind)) g.kind = kinds.length ? kinds[0][0] : "video";
  const meta = d.music
    ? [esc(d.author), `${d.music} · ${d.count > 1 ? d.count + " titres" : "titre"}`, d.count === 1 && d.duration ? fmtTime(d.duration) : ""].filter(Boolean).join(" · ")
    : [d.author, d.playlist ? d.playlist + " vidéos" : d.live ? '<b class="gr-live">● En direct</b>' : d.duration ? fmtTime(d.duration) : ""]
      .filter(Boolean).map((x, i) => (i === 0 ? esc(x) : x)).join(" · ");
  const sq = d.music ? " sq" : "";
  const th = d.thumb ? `<img class="gr-thumb${sq}" src="${d.thumb}" alt="">` : `<div class="gr-thumb fb${sq}">${d.music ? I.note : I.film}</div>`;
  const canCut = grCanCut();
  const seg = kinds.map(([k, l, ic]) => `<button class="gr-seg${g.kind === k ? " on" : ""}" data-gr="kind" data-k="${k}">${ic}${l}</button>`).join("");
  let opts = "";
  if (g.kind === "video") {
    const vs = grVideoOpts(d);
    if (!vs.some((v) => v.h === g.fmt)) g.fmt = vs.length ? vs[0].h : 0;
    opts = vs.map((v) => `<button class="gr-chip${v.h === g.fmt ? " on" : ""}" data-gr="fmt" data-h="${v.h}">${grQLabel(v.h)}${v.size ? `<span>${fmtBytes(v.size)}</span>` : ""}</button>`).join("");
  } else if (g.kind === "audio" && d.music) {
    opts = `<span class="gr-note">Chaque titre est retrouvé sur <b>YouTube</b> → MP3 avec la pochette et les infos</span>`;
  } else if (g.kind === "audio") {
    opts = `<span class="gr-note">MP3, meilleure qualité, avec la pochette${d.limit ? ` · les <b>${d.limit} premiers</b> du mix` : d.playlist ? " · toute la playlist" : ""}</span>`;
  } else if (g.kind === "gif") {
    opts = `<span class="gr-note">GIF animé sans le son, 480 px, 15 images/s${d.duration > GR_GIF_MAX ? ` · <b>${GR_GIF_MAX} s max</b> : coupe un extrait` : ""}</span>`;
  } else {
    const l = d.subs.lang.replace(/-orig$/, "");
    opts = `<span class="gr-note">Transcription en <b>${esc(l)}</b> (${d.subs.auto ? "générée par le site" : "sous-titres"}) → fichier texte</span>`;
  }
  const cut = canCut ? `<button class="gr-cut${g.cut ? " on" : ""}" data-gr="cut" title="Ne garder qu'un passage">${I.scissors}Extrait</button>` : "";
  const cutRow = g.cut && canCut ? `<div class="gr-cutrow">De <input class="gr-time" data-t="a" value="${esc(g.a)}" placeholder="0:00" spellcheck="false">
      à <input class="gr-time" data-t="b" value="${esc(g.b)}" placeholder="${fmtTime(d.duration)}" spellcheck="false"><span id="gr-cuterr" class="gr-cuterr">${esc(g.cutErr || "")}</span></div>` : "";
  const size = grSize();
  const go = d.live ? "Enregistrer le live" : d.limit ? `Télécharger ${d.limit} titres du mix` : d.playlist ? `Télécharger les ${d.playlist}` : d.music && d.count > 1 ? `Télécharger les ${d.count} titres` : g.kind === "gif" ? "Créer le GIF" : "Télécharger";
  const mixRow = d0.mix
    ? `<div class="gr-row gr-opts gr-mixrow"><span class="gr-mixl" title="Playlist créée par YouTube à partir de ce titre">${I.shuffle}Mix</span>${(d0.playlist ? [] : [0]).concat(GR_MIX_N)
      .map((n) => `<button class="gr-chip${g.mixN === n ? " on" : ""}" data-gr="mix" data-n="${n}" title="${n ? `Les ${n} premiers titres du mix` : "Seulement ce titre"}">${n ? n + (n === 10 ? " titres" : "") : "Ce titre"}</button>`).join("")}</div>`
    : "";
  return `<div class="gr-card">
    <div class="gr-top">${th}<div class="gr-meta"><b>${esc(d.title || "Sans titre")}</b><span>${meta}</span></div></div>
    ${mixRow}
    <div class="gr-row"><div class="gr-segs">${seg}</div>${cut}</div>
    <div class="gr-row gr-opts">${opts}</div>
    ${cutRow}
    <button class="gr-go" data-gr="start">${I.download}<span id="gr-golbl">${go}${size ? " · " + fmtBytes(size) : ""}</span></button>
  </div>`;
}

function grJobSub(j) {
  if (S.gr.likeMsg && S.gr.likeMsg.id === j.id) return S.gr.likeMsg.text;
  const p = j.phone;
  if (p && j.status === "done") {
    switch (p.state) {
      case "find": return `Recherche de ${p.dev}…`;
      case "wait": return `Accepte l'envoi sur ${p.dev}`;
      case "send": return `Envoi sur ${p.dev} · ${Math.round((p.pct || 0) * 100)} %`;
      case "done": return [`✓ Envoyé sur ${p.dev}`, p.msg].filter(Boolean).join(" · ");
      case "error": return p.msg || "Envoi impossible";
    }
  }
  const pct = j.pct != null ? Math.round(j.pct * 100) + " %" : "";
  const it = j.item && j.item[0] ? `${j.item[0]}/${j.item[1]}` : "";
  switch (j.status) {
    case "queued": return `${j.label} · En attente`;
    case "post": return `${j.label} · Finalisation…`;
    case "running":
      if (j.live) return `● Enregistrement · ${fmtBytes(j.bytes)}`;
      if (j.music) return ["MP3", it, j.cur || "", pct].filter(Boolean).join(" · ");
      return [j.label, it, pct, j.speed > 0 ? fmtBytes(j.speed) + "/s" : "", j.eta > 0 && j.eta < 360000 ? fmtTime(j.eta) + " restant" : ""].filter(Boolean).join(" · ");
    case "done":
      if (j.liked) return `${j.label} · ✓ Dans Spotify (fichiers locaux)`;
      return [j.label, j.files > 1 && !(j.items || []).length ? j.files + " fichiers" : "", j.bytes && j.files <= 1 ? fmtBytes(j.bytes) : "", j.error || "Terminé"].filter(Boolean).join(" · ");
    case "stopped": return `${j.label} · Arrêté`;
    default: return j.error || "Échec";
  }
}
function grJobHTML(j) {
  const ic = j.kind === "audio" ? I.note : j.kind === "text" ? I.textdoc : j.kind === "gif" ? I.gif : I.film;
  const sq = j.music ? " sq" : "";
  const th = j.thumb ? `<img class="gr-jth${sq}" src="${j.thumb}" alt="">` : `<div class="gr-jth fb${sq}">${ic}</div>`;
  let acts = "";
  if (grActive(j)) acts = `<button class="gr-ib" data-gr="stop" data-id="${j.id}" title="${j.live ? "Arrêter l'enregistrement" : "Annuler"}">${j.live ? I.stop : I.close}</button>`;
  else if (j.status === "done") {
    // MP3 : bouton Spotify (met le MP3 dans le dossier lu par Spotify) à la place de « lire »
    if (j.kind === "audio") {
      const busy = S.gr.liking.has(j.id);
      acts = `<button class="gr-ib gr-sp${j.liked ? " on" : ""}${busy ? " busy" : ""}" data-gr="like" data-id="${j.id}" title="${j.liked ? "Déjà dans Spotify (Fichiers locaux) · ouvrir Spotify" : "Mettre dans Spotify (Fichiers locaux)"}">${I.spotify}</button>`;
      // et « Ma musique » (lecteur de l'île) à la place du bouton dossier
      const mb = S.gr.adding.has(j.id);
      acts += `<button class="gr-ib gr-mu${j.in_music ? " on" : ""}${mb ? " busy" : ""}" data-gr="mu" data-id="${j.id}" title="${j.in_music ? "Dans Ma musique · lire" : "Ajouter à Ma musique"}">${j.in_music ? I.play : I.music}</button>`;
    } else {
      if (j.files <= 1 && j.path) acts = `<button class="gr-ib" data-gr="open" data-id="${j.id}" title="Ouvrir">${I.play}</button>`;
      acts += `<button class="gr-ib" data-gr="reveal" data-id="${j.id}" title="Afficher dans le dossier">${I.folder}</button>`;
    }
  }
  else acts = `<button class="gr-ib" data-gr="retry" data-id="${j.id}" title="Réessayer">${I.retry}</button>`;
  if (j.status === "done") {
    // LocalSend : envoyer sur le tel (clic droit : choisir un autre appareil)
    const p = j.phone, busy = grPhBusy(j), dev = (p && p.dev) || S.gr.phDev;
    const tip = busy ? "Annuler l'envoi" : (dev ? `Envoyer sur ${dev} (LocalSend)` : "Envoyer sur le tel (LocalSend)") + " · clic droit : autre appareil";
    acts += `<button class="gr-ib gr-ph${p && p.state === "done" ? " on" : ""}${busy ? " busy" : ""}" data-gr="phone" data-id="${j.id}" title="${esc(tip)}">${busy ? I.close : I.phone}</button>`;
  }
  const many = j.status === "done" && (j.items || []).length > 1;
  if (!grActive(j)) acts += delBtn("gr-del", j.id, many ? `Supprimer les ${j.items.length} fichiers (Corbeille)` : j.path && j.status === "done" ? "Supprimer ce fichier (Corbeille)" : "Retirer de la liste");
  // mix / playlist : « 8 titres ▾ » déplie la liste des musiques téléchargées
  const unf = many ? `<button class="gr-unf${S.gr.unfold.has(j.id) ? " on" : ""}" data-gr="unfold" data-id="${j.id}" title="Voir les titres">${j.items.length} titres${I.chevR}</button>` : "";
  return `${th}<div class="gr-jt"><b>${esc(j.title || j.url)}</b><span class="gr-sub"></span></div>${unf}<div class="gr-acts">${acts}</div>`
    + (grActive(j) || grPhBusy(j) ? `<div class="gr-bar${grPhBusy(j) ? " ph" : ""}"><i></i></div>` : "");
}

/* nom lisible d'un fichier de mix : « 003 - Titre [id].mp3 » → « Titre » */
function grItemName(p) {
  return p.split(/[\\/]/).pop().replace(/\.[^.]+$/, "").replace(/^\d{3} - /, "").replace(/\s*\[[A-Za-z0-9_-]{6,}\]$/, "");
}
function grItemsHTML(j) {
  return (j.items || []).map((p, i) =>
    `<button class="gr-it" data-gr="item" data-id="${j.id}" data-i="${i}" title="Lire dans Ma musique"><span>${i + 1}</span><b>${esc(grItemName(p))}</b>${I.play}</button>`).join("");
}
/* lit un titre du mix (le mix entier passe dans Ma musique, lecture à partir de ce titre) */
function grPlayItem(id, i) {
  const g = S.gr, j = g.jobs.find((x) => x.id === id);
  if (!j || g.adding.has(id)) return;
  const name = j.items[i].split(/[\\/]/).pop();
  g.adding.add(id);
  render();
  invoke("grab_music_add", { id })
    .then((r) => {
      S.mu.loaded = false;
      j.in_music = true;
      const start = Math.max(0, r.ids.findIndex((x) => x.split(/[\\/]/).pop() === name));
      return invoke("music_play", { ids: r.ids, start, shuffle: false });
    })
    .catch((e) => { g.likeMsg = { id, text: "Ma musique : " + e }; })
    .finally(() => { g.adding.delete(id); render(); });
}

function grRenderList() {
  const jobs = S.gr.jobs.slice().reverse();
  const unf = (j) => S.gr.unfold.has(j.id) && j.status === "done" && (j.items || []).length > 1;
  const ids = jobs.map((j) => j.id + (unf(j) ? "+" + j.items.length : "")).join(",");
  if (grList.dataset.ids !== ids) {
    grList.dataset.ids = ids;
    grList.innerHTML = jobs.map((j) => `<div class="gr-job" data-id="${j.id}"></div>` + (unf(j) ? `<div class="gr-items">${grItemsHTML(j)}</div>` : "")).join("");
  }
  for (const j of jobs) {
    const row = grList.querySelector(`.gr-job[data-id="${j.id}"]`);
    if (!row) continue;
    const k = [j.status, j.path, j.error, j.files, (j.thumb || "").length, j.title, j.liked, S.gr.liking.has(j.id), j.in_music, S.gr.adding.has(j.id), delArm === "gr-del:" + j.id, (j.items || []).length, S.gr.unfold.has(j.id), j.phone && j.phone.state, j.phone && j.phone.dev, S.gr.phDev].join("|");
    if (row.dataset.k !== k) {
      row.dataset.k = k;
      row.className = "gr-job " + j.status;
      row.innerHTML = grJobHTML(j);
    }
    row.querySelector(".gr-sub").textContent = grJobSub(j);
    const bar = row.querySelector(".gr-bar");
    if (bar) {
      const ph = j.phone && j.phone.state === "send";
      const det = ph || (j.pct != null && j.status === "running");
      const v = ph ? j.phone.pct || 0 : j.pct;
      bar.classList.toggle("det", det);
      bar.firstElementChild.style.width = det ? (v * 100).toFixed(1) + "%" : "";
    }
  }
}

function grRender() {
  document.querySelector('[data-tool="grab"]').classList.toggle("gr-busy", S.gr.jobs.some(grActive));
  if (!(S.hover && S.gr.open)) return;
  const g = S.gr, st = g.status;
  let body = "", key;
  if (st && !st.ready) {
    const miss = [!st.ytdlp && "yt-dlp (18 Mo)", !st.ffmpeg && "ffmpeg (≈ 90 Mo)", !st.deno && "Deno (≈ 40 Mo)"].filter(Boolean);
    body = `<div class="gr-setup"><b>Installer le téléchargeur</b><span>${miss.join(", ")}, depuis leurs pages officielles (GitHub). Une seule fois, mis à jour tout seul ensuite.</span>
      ${st.error ? `<em>${esc(st.error)}</em>` : ""}
      ${st.installing ? `<div class="gr-inst"><div class="cl-dots"><i></i><i></i><i></i></div>${esc(st.installing)}</div>` : `<button class="gr-go small" data-gr="install">${I.download}<span>Installer</span></button>`}</div>`;
    key = body;
  } else if (g.probing) {
    body = `<div class="gr-msg"><div class="cl-dots"><i></i><i></i><i></i></div>Analyse du lien…</div>`;
    key = "probing";
  } else if (g.err) {
    body = `<div class="gr-msg err">${esc(g.err)}</div>`;
    key = body;
  } else if (g.spHelp) {
    const h = g.spHelp, sf = h.spotifast;
    const btns = `<div class="gr-sprow"><button class="gr-go small" data-gr="spopen">${I.spotify}<span>Ouvrir Spotify</span></button><button class="gr-go small ghost" data-gr="spdir">${I.folder}<span>Dossier</span></button><button class="gr-go small ghost" data-gr="spok"><span>OK</span></button></div>`;
    let steps;
    if (sf) {
      const link = sf.status === "linked" || sf.status === "ok" ? "Dossier relié à Spotifast ✓"
        : sf.status === "running" ? "Ferme Spotifast puis reclique sur le bouton Spotify : l'île reliera le dossier."
        : "Spotifast utilise déjà un autre dossier de fichiers locaux : mets-y tes MP3 ou change-le.";
      steps = `<ol><li>${link}</li>
        <li>${sf.soloist ? "Spotifast ne lit les fichiers locaux qu'avec son <i>lecteur intégré</i> : Réglages → désactive <i>Spotify Soloist</i>." : "Lecteur intégré de Spotifast : ok."}</li>
        <li>Spotifast lit les MP3 <i>qui sont dans une playlist</i> : ajoute-les une fois depuis l'appli Spotify officielle (<i>Fichiers locaux</i> → glisser dans une playlist).</li></ol>`;
    } else {
      steps = `<ol><li>Spotify → <i>Paramètres</i> → <i>Fichiers locaux</i> : active <i>Afficher les fichiers locaux</i>.</li>
        <li><i>Ajouter une source</i> → colle le chemin (déjà copié) :<code>${esc(h.dir)}</code></li></ol>
        <span>Ensuite, chaque MP3 envoyé avec le bouton Spotify apparaît dans <i>Bibliothèque → Fichiers locaux</i>.</span>`;
    }
    body = `<div class="gr-sphelp"><b>${I.spotify}Tes MP3 dans ${sf ? "Spotifast" : "Spotify"} · à faire une fois</b>${steps}${btns}</div>`;
    key = "sphelp|" + JSON.stringify(h);
  } else if (g.ph) {
    body = grPhHTML();
    key = "ph|" + JSON.stringify(g.ph);
  } else if (g.info) {
    body = grCardHTML();
    // les champs de l'extrait ne sont pas redessinés pendant qu'on tape
    key = ["card", g.info.url, g.kind, g.fmt, g.cut].join("|");
  } else if (!g.jobs.length) {
    body = `<div class="gr-help">Vidéo, MP3, GIF ou playlist : YouTube, TikTok, Instagram, X, Reddit, Twitch… et 1 800 autres sites.<br><span>Spotify et Deezer : les titres sont retrouvés sur YouTube. Pas Netflix, Disney+… (DRM).</span></div>`;
    key = "help";
  } else key = "empty";
  setHTML(grBody, body, key);
  grRenderList();
  $("gr-clear").style.display = g.jobs.some((j) => !grActive(j)) ? "" : "none";
  grPasteBtn.textContent = grQ.value.trim() && grQ.value.trim() !== g.url ? "Analyser" : "Coller";
}

function grRefresh() {
  invoke("grab_status").then((st) => { S.gr.status = st; render(); }).catch(() => {});
  invoke("grab_jobs").then((l) => { S.gr.jobs = l || []; dlsChanged(); }).catch(() => {});
}

/* lien vidéo copié il y a peu (pour pré-remplir) */
function grRecentClip(maxAge = 120000) {
  const c = (S.clips || []).filter((x) => x.kind === "text").sort((a, b) => b.at - a.at)[0];
  if (!c) return null;
  const u = c.text.trim();
  return Date.now() - c.at < maxAge && GR_SITES.test(u) && !/\s/.test(u) && !grUsed(u) ? u : null;
}

let grProbeSeq = 0;
function grProbe(url) {
  url = (url || "").trim();
  if (!url) return;
  const g = S.gr;
  g.url = url;
  grQ.value = url;
  g.info = null; g.err = null; g.ph = null; g.cut = false; g.a = g.b = ""; g.cutErr = "";
  if (!/^https?:\/\/\S+$/i.test(url)) { g.err = "Colle un lien complet (qui commence par https://)"; return render(); }
  if (g.status && !g.status.ready) return render(); // l'analyse partira après l'installation
  g.probing = true;
  const seq = ++grProbeSeq;
  render();
  invoke("grab_probe", { url })
    .then((d) => {
      if (seq !== grProbeSeq) return;
      g.info = d; g.fmt = d.videos && d.videos[0] ? d.videos[0].h : 0;
      if (d.is_gif) g.kind = "gif";
      // lien vers un mix seul : 25 titres par défaut ; vidéo dans un mix : ce titre seul par défaut
      g.mixN = d.mix && d.playlist ? 25 : 0;
      if (g.mixN) g.kind = "audio";
    })
    .catch((e) => { if (seq === grProbeSeq) g.err = String(e); })
    .finally(() => { if (seq === grProbeSeq) { g.probing = false; render(); } });
}

function grStart() {
  const d = grEff(), g = S.gr;
  if (!d) return;
  g.used.add(S.gr.info.url);
  let start = null, end = null;
  if (g.cut && grCanCut()) {
    start = grParseT(g.a); end = grParseT(g.b);
    let err = "";
    if (Number.isNaN(start) || Number.isNaN(end)) err = "Format : 1:23 ou 83";
    else if (start != null && start >= d.duration) err = "Début après la fin";
    else if (start != null && end != null && end <= start) err = "La fin est avant le début";
    if (err) { g.cutErr = err; const el = $("gr-cuterr"); if (el) el.textContent = err; return; }
    if (end != null && end >= d.duration) end = null;
    if (start === 0) start = null;
  }
  if (g.kind === "gif" && d.duration > 0) {
    const len = (end != null ? end : d.duration) - (start || 0);
    if (len > GR_GIF_MAX + 0.5) {
      if (!g.cut) { g.cut = true; g.cutErr = `${GR_GIF_MAX} s max pour un GIF`; return render(); }
      g.cutErr = `${GR_GIF_MAX} s max pour un GIF`; const el = $("gr-cuterr"); if (el) el.textContent = g.cutErr; return;
    }
  }
  g.used.add(d.url); if (g.url) g.used.add(g.url);
  invoke("grab_start", { url: d.url, kind: g.kind, format: g.kind === "video" ? g.fmt || 0 : null, start, end, playlist: !!d.playlist, info: d })
    .then(() => { g.info = null; g.url = ""; grQ.value = ""; render(); })
    .catch((e) => { g.err = String(e); g.info = null; render(); });
  releaseKeyboard();
}

function grOpenPanel(u) {
  const g = S.gr;
  g.open = true;
  S.cl.open = S.clipOpen = S.mixerOpen = S.notifOpen = S.picking = false;
  grRefresh();
  const url = u || (!g.url && !g.info ? grRecentClip() : null);
  if (url && url !== g.url) grProbe(url);
  render();
}

/* MP3 → dossier lu par Spotify (fichiers locaux) */
function grLike(id) {
  const g = S.gr, j = g.jobs.find((x) => x.id === id);
  if (!j || g.liking.has(id)) return;
  const inPanel = () => S.hover && S.gr.open;
  const note = (text) => { g.likeMsg = text ? { id, text } : null; render(); };
  note(null);
  const relink = g.spHelp && g.spHelp.spotifast && g.spHelp.spotifast.status === "running";
  if (j.liked && !relink) {
    // déjà dans le dossier : on rouvre Spotify
    invoke("open_spotify").catch(() => {});
    return;
  }
  g.liking.add(id);
  render();
  invoke("grab_spotify_like", { id })
    .then((r) => {
      if (r.first || (r.spotifast && (r.spotifast.status === "running" || g.spHelp))) {
        // la toute première fois : comment faire apparaître le dossier dans Spotify (chemin déjà copié)
        g.spHelp = r;
        if (!r.spotifast) invoke("copy_text", { text: r.dir }).catch(() => {});
      }
      if (inPanel()) return;
      pushTransient("alert", {
        app: "Spotify", title: "Ajouté aux fichiers locaux", body: r.total > 1 ? `${r.total} titres` : j.title,
        icon: j.thumb || null, svg: j.thumb ? null : `<div class="dl-done" style="background:#1db954">${I.spotify}</div>`, color: [141, 70],
      }, 4500, true);
    })
    .catch((e) => {
      e = String(e);
      if (inPanel()) note("Spotify : " + e);
      else pushTransient("alert", { app: "Spotify", title: "Impossible d'ajouter le titre", body: e, svg: `<div class="dl-done" style="background:#ff453a">${I.close}</div>`, color: [4, 90] }, 5000);
    })
    .finally(() => { g.liking.delete(id); render(); });
}

/* ---------- LocalSend : envoyer un téléchargement sur le tel ---------- */
const grPhBusy = (j) => !!(j.phone && ["find", "wait", "send"].includes(j.phone.state));
function grPhone(id) {
  const g = S.gr, j = g.jobs.find((x) => x.id === id);
  if (!j) return;
  if (grPhBusy(j)) return invoke("phone_cancel", { id }).catch(() => {});
  g.likeMsg = null;
  // dernier envoi raté : on montre la liste des appareils (le tel a pu changer de nom ou de réseau)
  if (j.phone && j.phone.state === "error") return grPhPick(id);
  invoke("phone_send", { id, dev: null }).catch((e) => {
    if (String(e) === "choose") grPhPick(id);
    else { g.likeMsg = { id, text: "Tel : " + e }; render(); }
  });
}
let grPhSeq = 0;
function grPhPick(id) {
  const g = S.gr, seq = ++grPhSeq;
  g.spHelp = null;
  g.ph = { id, scanning: true, devs: [] };
  render();
  Promise.all([invoke("phone_scan"), invoke("phone_device").catch(() => null)])
    .then(([devs, cur]) => { if (g.ph && seq === grPhSeq) { g.ph = { id, scanning: false, devs: devs || [], cur: cur && (cur.fingerprint || cur.alias) }; render(); } })
    .catch((e) => { if (g.ph && seq === grPhSeq) { g.ph = { id, scanning: false, devs: [], err: String(e) }; render(); } });
}
function grPhH() {
  const p = S.gr.ph;
  return 20 + 20 + 6 + (p.scanning || !p.devs.length ? 44 : Math.min(4, p.devs.length) * 36) + 32;
}
function grPhHTML() {
  const p = S.gr.ph;
  const list = p.scanning
    ? `<div class="gr-phmsg"><div class="cl-dots"><i></i><i></i><i></i></div>Recherche des appareils LocalSend…</div>`
    : p.devs.length
      ? `<div class="gr-phlist">${p.devs.map((d, i) => `<button class="gr-phd${(d.fingerprint || d.alias) === p.cur ? " on" : ""}" data-gr="phdev" data-i="${i}">${I.phone}<b>${esc(d.alias)}</b><span>${esc(d.model || (d.kind === "mobile" ? "Tel" : d.kind))}</span></button>`).join("")}</div>`
      : `<div class="gr-phmsg"><span>${p.err ? esc(p.err) : "Aucun appareil trouvé. Ouvre <i>LocalSend</i> sur ton tel, connecté au même Wi-Fi que le PC."}</span></div>`;
  return `<div class="gr-phpick"><b>${I.phone}Envoyer sur quel appareil ?</b>${list}
    <div class="gr-sprow"><button class="gr-go small ghost" data-gr="phscan" data-id="${p.id}"${p.scanning ? " disabled" : ""}>${I.retry}<span>Rechercher</span></button><button class="gr-go small ghost" data-gr="phclose"><span>Annuler</span></button></div></div>`;
}

/* Un téléchargement : fichier(s) à la Corbeille + retiré de la liste */
function grRemove(id) {
  id = +id;
  const g = S.gr, j = g.jobs.find((x) => x.id === id);
  g.jobs = g.jobs.filter((x) => x.id !== id); // tout de suite retiré (la liste officielle revient par l'évènement « grab »)
  dlsChanged();
  render();
  invoke("grab_remove", { id }).catch((err) => {
    invoke("grab_jobs").then((l) => { if (l) { g.jobs = l; dlsChanged(); render(); } }).catch(() => {});
    pushTransient("alert", { app: "Télécharger", title: j ? j.title : "Suppression", body: String(err), svg: `<div class="dl-done" style="background:#ff453a">${I.close}</div>`, color: [4, 90] }, 5000);
  });
}

/* MP3 → Ma musique (lecteur de l'île). Déjà ajouté : on le lance. */
function grMusic(id) {
  const g = S.gr, j = g.jobs.find((x) => x.id === id);
  if (!j || g.adding.has(id)) return;
  g.adding.add(id);
  render();
  invoke("grab_music_add", { id })
    .then((r) => {
      S.mu.loaded = false; // la liste sera relue à la prochaine ouverture
      if (j.in_music) return invoke("music_play", { ids: r.ids, start: 0, shuffle: false });
      j.in_music = true;
      const inPanel = S.hover && S.gr.open;
      if (!inPanel)
        pushTransient("alert", {
          app: "Ma musique", title: "Ajouté à Ma musique", body: r.total > 1 ? `${r.total} titres` : j.title,
          icon: j.thumb || null, svg: j.thumb ? null : `<div class="dl-done" style="background:#fa2d6c">${I.music}</div>`, color: [340, 90],
        }, 4000, true);
    })
    .catch((e) => {
      e = String(e);
      if (S.hover && S.gr.open) { g.likeMsg = { id, text: "Ma musique : " + e }; render(); }
      else pushTransient("alert", { app: "Ma musique", title: "Impossible d'ajouter le titre", body: e, svg: `<div class="dl-done" style="background:#ff453a">${I.close}</div>`, color: [4, 90] }, 5000);
    })
    .finally(() => { g.adding.delete(id); render(); });
}

/* lien copié : l'île propose de le télécharger */
function grOffer(u) {
  if (grUsed(u)) return;
  if (S.hover && S.gr.open) return grProbe(u);
  let host = "";
  try { host = new URL(u).hostname.replace(/^(www|m|music|vm)\./, ""); } catch {}
  const music = /spotify|deezer/i.test(host);
  pushTransient("alert", {
    app: "Télécharger", title: "Lien copié" + (host ? " · " + host : ""), body: music ? "Clique pour le télécharger en MP3 (via YouTube)" : "Clique pour télécharger la vidéo, la musique ou un GIF",
    svg: `<div class="dl-done">${I.download}</div>`, color: [212, 90],
    onClick: () => grOpenPanel(u),
  }, 5000);
}

/* téléchargements du navigateur + ceux de l'île, dans la même activité compacte */
function dlsChanged() {
  const prev = new Set(S.dls.map((d) => d.id));
  S.dls = (S.bdls || []).concat(S.gr.jobs.filter(grActive).map((j) => ({
    id: "g" + j.id, name: j.title || "Téléchargement", size: j.bytes, speed: j.speed, pct: j.status === "running" && !j.live ? j.pct : null,
    live: j.live, post: j.status === "post", queued: j.status === "queued",
  })));
  const fresh = S.dls.find((d) => !prev.has(d.id));
  if (fresh) {
    S.pin = "download";
    island.classList.remove("bump"); void island.offsetWidth; island.classList.add("bump");
  }
  render();
}

listen("grab", (e) => {
  const prev = new Map(S.gr.jobs.map((j) => [j.id, j.phone && j.phone.state]));
  S.gr.jobs = e.payload || [];
  // envoi LocalSend fini pendant que le panneau est fermé : petite alerte
  for (const j of S.gr.jobs) {
    const p = j.phone, was = prev.get(j.id);
    if (!p || p.state === was || !(p.state === "done" || p.state === "error") || !was) continue;
    if (S.hover && S.gr.open) continue;
    pushTransient("alert", p.state === "done"
      ? { app: "LocalSend", title: `Envoyé sur ${p.dev}`, body: [j.title, p.msg].filter(Boolean).join(" · "), icon: j.thumb || null, svg: j.thumb ? null : `<div class="dl-done" style="background:#2ec4b6">${I.phone}</div>`, color: [174, 62] }
      : { app: "LocalSend", title: "Envoi sur le tel impossible", body: p.msg || j.title, svg: `<div class="dl-done" style="background:#ff453a">${I.close}</div>`, color: [4, 90] }, 5000, p.state === "done");
  }
  dlsChanged();
});
listen("grab-setup", (e) => {
  const was = S.gr.status && S.gr.status.ready;
  S.gr.status = e.payload;
  if (!was && S.gr.status.ready && S.gr.url && !S.gr.info && !S.gr.probing) grProbe(S.gr.url);
  render();
});
listen("grab-done", (e) => {
  const j = e.payload;
  if (j.status !== "done") S.gr.used.delete(j.url);
  if (S.hover && S.gr.open) return;
  if (j.status === "done") {
    pushTransient("alert", {
      app: "Télécharger", title: j.live ? "Enregistrement terminé" : "Téléchargement terminé",
      body: [j.title, j.label, j.files > 1 ? j.files + " fichiers" : j.bytes ? fmtBytes(j.bytes) : ""].filter(Boolean).join(" · "),
      icon: j.thumb || null, svg: j.thumb ? null : `<div class="dl-done">${I.check}</div>`, color: [212, 90],
      onClick: () => invoke("grab_reveal", { id: j.id }).catch(() => {}),
    }, 7000, true);
  } else if (j.status === "error") {
    pushTransient("alert", {
      app: "Télécharger", title: "Téléchargement impossible", body: j.error || j.title,
      svg: `<div class="dl-done" style="background:#ff453a">${I.close}</div>`, color: [4, 90],
      onClick: () => grOpenPanel(),
    }, 7000);
  }
});

grQ.addEventListener("mousedown", () => grabKeyboard(grQ));
grQ.addEventListener("keydown", (e) => {
  if (e.key === "Enter") { e.preventDefault(); grProbe(grQ.value); }
  else if (e.key === "Escape") releaseKeyboard();
});
grQ.addEventListener("input", () => { grPasteBtn.textContent = grQ.value.trim() && grQ.value.trim() !== S.gr.url ? "Analyser" : "Coller"; });
grQ.addEventListener("paste", () => setTimeout(() => grProbe(grQ.value), 0));
$("x-grab").addEventListener("mousedown", (e) => {
  if (e.target.classList.contains("gr-time")) grabKeyboard(e.target);
});
$("x-grab").addEventListener("input", (e) => {
  const t = e.target;
  if (!t.classList.contains("gr-time")) return;
  S.gr[t.dataset.t] = t.value;
  S.gr.cutErr = "";
  const el = $("gr-cuterr");
  if (el) el.textContent = "";
  const lbl = $("gr-golbl"), size = grSize();
  if (lbl) lbl.textContent = "Télécharger" + (size ? " · " + fmtBytes(size) : "");
});
$("x-grab").addEventListener("keydown", (e) => {
  if (!e.target.classList.contains("gr-time")) return;
  if (e.key === "Enter") { e.preventDefault(); grStart(); }
  else if (e.key === "Escape") releaseKeyboard();
});
$("x-grab").addEventListener("click", (e) => {
  e.stopPropagation();
  const t = e.target.closest("button");
  if (t && t.dataset.del) return delClick(t, grRemove);
  if (!t || !t.dataset.gr) return;
  const g = S.gr, a = t.dataset.gr, id = +t.dataset.id;
  if (a === "install") { invoke("grab_install").catch(() => {}); g.status = { ...g.status, installing: "Préparation…" }; render(); }
  else if (a === "paste") {
    const v = grQ.value.trim();
    if (v && v !== g.url) grProbe(v);
    else {
      const c = (S.clips || []).filter((x) => x.kind === "text").sort((x, y) => y.at - x.at)[0];
      if (c && /^https?:\/\//i.test(c.text.trim())) grProbe(c.text);
      else { g.err = "Aucun lien dans le presse-papiers"; g.info = null; render(); }
    }
  }
  else if (a === "kind") { g.kind = t.dataset.k; render(); }
  else if (a === "mix") {
    const prev = g.mixN || 0;
    g.mixN = +t.dataset.n;
    // un mix, c'est de la musique : MP3 par défaut en passant de « Ce titre » au mix
    if (g.mixN && (!prev || !["video", "audio"].includes(g.kind))) g.kind = "audio";
    g.cut = false;
    render();
  }
  else if (a === "like") grLike(id);
  else if (a === "mu") grMusic(id);
  else if (a === "unfold") { g.unfold.has(id) ? g.unfold.delete(id) : g.unfold.add(id); render(); }
  else if (a === "item") grPlayItem(id, +t.dataset.i);
  else if (a === "phone") grPhone(id);
  else if (a === "phscan") grPhPick(g.ph ? g.ph.id : id);
  else if (a === "phclose") { g.ph = null; grPhSeq++; render(); }
  else if (a === "phdev") {
    const p = g.ph, d = p && p.devs[+t.dataset.i];
    if (!d) return;
    g.ph = null;
    g.phDev = d.alias;
    render();
    invoke("phone_send", { id: p.id, dev: d }).catch((e) => { g.likeMsg = { id: p.id, text: "Tel : " + e }; render(); });
  }
  else if (a === "spopen") invoke("open_spotify").catch(() => {});
  else if (a === "spdir") invoke("grab_spotify_dir").catch(() => {});
  else if (a === "spok") { S.gr.spHelp = null; render(); }
  else if (a === "fmt") { g.fmt = +t.dataset.h; render(); }
  else if (a === "cut") {
    g.cut = !g.cut;
    render();
    if (g.cut) setTimeout(() => { const i = document.querySelector('.gr-time[data-t="a"]'); if (i) grabKeyboard(i); }, 60);
  }
  else if (a === "start") grStart();
  else if (a === "stop") invoke("grab_stop", { id }).catch(() => {});
  else if (a === "retry") invoke("grab_retry", { id }).catch(() => {});
  else if (a === "open") invoke("grab_open", { id }).catch((err) => pushTransient("alert", { app: "Télécharger", title: "Ouvrir", body: String(err) }, 3000));
  else if (a === "reveal") invoke("grab_reveal", { id }).catch(() => {});
  else if (a === "clear") invoke("grab_clear").catch(() => {});
  else if (a === "dir") invoke("grab_open_dir").catch(() => {});
});
$("x-grab").addEventListener("contextmenu", (e) => {
  const t = e.target.closest('button[data-gr="phone"]');
  if (!t) return;
  e.preventDefault();
  e.stopPropagation();
  grPhPick(+t.dataset.id);
});
grRefresh();
invoke("phone_device").then((d) => { if (d) { S.gr.phDev = d.alias; render(); } }).catch(() => {});

clQ.addEventListener("mousedown", () => grabKeyboard(clQ));
$("x-claude").querySelector(".cl-input").addEventListener("mousedown", (e) => {
  if (e.target === clQ || e.target.closest(".cl-send")) return;
  e.preventDefault();
  grabKeyboard(clQ);
});
clQ.addEventListener("input", clAutosize);
clQ.addEventListener("keydown", (e) => {
  if (e.key === "Enter" && !e.shiftKey) { e.preventDefault(); clSend(); }
  else if (e.key === "Escape") releaseKeyboard();
});
$("x-claude").addEventListener("click", (e) => {
  e.stopPropagation();
  const t = e.target.closest("button");
  if (!t) return;
  if (t === clSendBtn) {
    if (S.cl.running) invoke("claude_stop").catch(() => {});
    else clSend();
  } else if (t.id === "cl-new") {
    if (S.cl.running) invoke("claude_stop").catch(() => {});
    invoke("claude_reset").catch(() => {});
    S.cl.msgs = [];
    S.cl.running = false;
    clRender();
    grabKeyboard(clQ);
  } else if (t.dataset.cl === "login") {
    invoke("claude_login").catch((err) => pushTransient("alert", { app: "Claude", title: "Connexion", body: String(err) }, 4000));
  } else if (t.dataset.cl === "recheck") {
    S.cl.status = null;
    clRender();
    clRefresh(true);
  } else if (t.dataset.cl === "copy") {
    const m = S.cl.msgs[+t.dataset.i];
    if (m) invoke("copy_text", { text: clText(m) }).then(() => { t.textContent = "Copié"; t.classList.add("ok"); }).catch(() => {});
  }
});


listen("privacy", (e) => {
  S.privacy = e.payload || { mic: [], cam: [] };
  render();
});
invoke("privacy_state").then((p) => { if (p) { S.privacy = p; render(); } }).catch(() => {});

/* ---------- au repos : l'heure + les notifs en attente ---------- */
function idleInfo() {
  const groups = notifGroups().slice(0, 3);
  return { time: new Date().toLocaleTimeString(LOC, { hour: "2-digit", minute: "2-digit" }), groups, count: S.notifs.length };
}
function idleSize() {
  const { time, groups, count } = idleInfo();
  if (!count) return [124, 30, 15];
  const w = 12 + textWidth(time) + 12 + 16 + (groups.length - 1) * 10 + 5 + textWidth(String(count)) + 10;
  return [Math.round(Math.max(124, w)), 30, 15];
}
function renderIdle() {
  const v = document.querySelector('[data-view="idle"]');
  const { time, groups, count } = idleInfo();
  v.classList.toggle("has-n", count > 0);
  $("i-time").textContent = time;
  setHTML($("i-notifs"), count ? groups.map((g) => notifIcon(g, "i-ic")).join("") + `<span class="i-count">${count > 99 ? "99+" : count}</span>` : "",
    "n" + count + groups.map((g) => g.key).join());
}

/* ---------- jeux ---------- */
function playTime(since) {
  const m = Math.max(0, Math.floor((Date.now() - since) / 60000));
  return m < 60 ? `${m} min` : `${Math.floor(m / 60)} h ${String(m % 60).padStart(2, "0")}`;
}
function playedAgo(t) {
  if (!t) return "Dernière partie";
  const s = (Date.now() - t) / 1000;
  if (s < 90) return "À l'instant";
  if (s < 3600) return `Il y a ${Math.floor(s / 60)} min`;
  if (s < 86400) return `Il y a ${Math.floor(s / 3600)} h`;
  if (s < 2 * 86400) return "Hier";
  return `Il y a ${Math.floor(s / 86400)} jours`;
}
function renderGameCard() {
  const box = $("x-game");
  const run = S.game.running, g = run || S.game.last;
  box.style.display = g ? "" : "none";
  if (!g) return;
  const icon = g.icon ? `<img class="g-icon" src="${g.icon}" alt="">` : `<div class="g-icon fb">${I.gamepad}</div>`;
  const sub = run ? `En jeu · ${playTime(run.since)}` : playedAgo(g.last);
  setHTML(box, `<div class="g-card${g.cover ? " has-cover" : ""}"${g.cover ? ` style="--cover:url('${g.cover}')"` : ""}>
      ${icon}
      <div class="g-text"><div class="g-sub${run ? " live" : ""}">${esc(sub)}</div><div class="g-name">${esc(g.name)}</div></div>
      ${run ? `<div class="g-live">${I.gamepad}</div>` : `<button class="g-play" data-game="${esc(g.key)}">${I.play}<span>Jouer</span></button>`}
    </div>`, g.key + "|" + (run ? "r" : "l") + sub);
}
async function launchGame(btn) {
  const key = btn.dataset.game;
  btn.classList.add("busy");
  btn.querySelector("span").textContent = "Lancement…";
  try {
    await invoke("launch_game", { key });
    setTimeout(() => { S.hover = false; render(); }, 900);
  } catch (err) {
    btn.classList.remove("busy");
    btn.querySelector("span").textContent = "Jouer";
    pushTransient("alert", { app: "Jeux", title: S.game.last ? S.game.last.name : "Jeu", body: String(err), svg: `<div style="width:26px">${I.gamepad}</div>` }, 6000);
  }
}
function applyGame(st) {
  if (!st) return;
  const prevKey = S.game.running && S.game.running.key;
  S.game.running = st.running || null;
  S.game.last = st.last || null;
  const g = S.game.running;
  // fin de partie : on recalcule la couleur de la pochette (le rendu a pu être perturbé pendant le jeu)
  if (prevKey && !g) { S.accentArt = null; S.pin = null; applyMedias(); }
  if (g && g.key !== prevKey) {
    S.game.color = null;
    if (g.icon) appColor("game:" + g.key, g.icon).then((c) => { if (S.game.running && S.game.running.key === g.key) { S.game.color = c; render(); } });
  }
  render();
}

/* ---------- coach League of Legends ---------- */
/* Conseil affiché dans l'île : le plus important s'il est urgent, sinon on alterne entre les 3 premiers */
function lolTip() {
  const L = S.lol;
  if (!L || !L.tips.length) return { text: L && L.loading ? "Analyse du matchup…" : (L && L.opp ? `${L.champ} vs ${L.opp}` : "League of Legends"), kind: "info", prio: 0 };
  if (L.tips[0].prio >= 60) return L.tips[0];
  const top = L.tips.slice(0, 3);
  return top[S.lolRot % top.length];
}
function lolTipsShown() {
  return S.lol ? Math.min(3, S.lol.tips.length) : 0;
}
function lolCardHeight() {
  const n = lolTipsShown();
  return 10 + 36 + 8 + 28 + (n ? 8 + n * 18 : 0) + 10;
}
function renderLolCard() {
  const box = $("x-game"), L = S.lol;
  box.style.display = "";
  const sub = [L.role, L.wr != null ? `${L.wr} % de victoire` : null, L.style].filter(Boolean).join(" · ");
  const head = `<div class="lc-head">
      <img class="lc-ch" src="${L.champ_icon}" alt="">
      ${L.opp_icon ? `<span class="lc-vs">vs</span><img class="lc-ch" src="${L.opp_icon}" alt=""${L.hint ? ` title="${esc(L.hint)}"` : ""}>` : ""}
      <div class="lc-t"><div class="lc-name">${esc(L.champ)}${L.opp ? ` <span>vs</span> ${esc(L.opp)}` : ""}</div><div class="lc-sub">${esc(sub || "League of Legends")}</div></div>
    </div>`;
  const build = L.build.length
    ? `<div class="lc-build">${L.build.map((i) => `<img class="lc-it${i.owned ? " owned" : ""}${L.next && L.next.id === i.id ? " next" : ""}" src="${i.icon}" title="${esc(i.name)}" alt="">`).join("")}
        ${L.next ? `<div class="lc-gold${L.missing === 0 ? " ready" : ""}">${L.missing === 0 ? "Achetable" : `−${L.missing} po`}</div>` : `<div class="lc-gold ready">Build fini</div>`}</div>`
    : `<div class="lc-build"><div class="lc-load">${L.loading ? "Stats OP.GG en chargement…" : "Pas de stats pour ce matchup"}</div></div>`;
  const tips = L.tips.slice(0, 3).map((x) => `<div class="lc-tip k-${x.kind}">${esc(x.text)}</div>`).join("");
  setHTML(box, `<div class="lc">${head}${build}${tips ? `<div class="lc-tips">${tips}</div>` : ""}</div>`, JSON.stringify([L.champ, L.opp, sub, L.build, L.next && L.next.id, L.missing, L.tips.slice(0, 3), L.loading]));
}
function applyLol(c) {
  S.lol = c || null;
  render();
}

/* ---------- bulle secondaire ---------- */
function renderBubble(view) {
  const acts = activities();
  const sec = view === "compact" ? acts[1] : null;
  bubble.classList.toggle("show", !!sec);
  const inner = $("bubble-inner");
  if (!sec) return;
  if (sec === "media" || sec === "media2") {
    const m = sec === "media" ? S.media : S.media2;
    setHTML(inner, m.art ? `<img id="b-art" src="${m.art}" alt="">` : `<div class="glyph" style="width:18px;color:${S.accent}">${I.note}</div>`, "bm" + m.id + (m.art || "").length);
    const a = $("b-art");
    if (a) a.classList.toggle("paused", !m.playing);
  } else if (sec === "download") {
    setHTML(inner, `<div class="dl-ic small">${I.download}</div>`, "bdl");
  } else if (sec === "voice") {
    const sp = voiceSpeaker();
    setHTML(inner, sp ? `<div class="vc-lead sp" style="width:20px;height:20px"><img src="${esc(sp.avatar)}" alt="" style="width:20px!important;height:20px!important"></div>`
      : `<div class="vc-ic" style="width:20px;height:20px">${I.voice}</div>`, "bv" + (sp ? sp.id : ""));
  } else if (sec === "game") {
    const g = S.game.running;
    setHTML(inner, g.icon ? `<img src="${g.icon}" alt="">` : `<div class="glyph green" style="width:18px">${I.gamepad}</div>`, "bg" + g.key);
  } else if (sec === "timer") {
    setHTML(inner, ringHTML("b-ring", "var(--orange)"), "bt");
    setRing($("b-ring"), timerLeft() / (S.timer.total || 1));
  } else if (sec === "phone") {
    setHTML(inner, ringHTML("b-ring", "var(--green)"), "bp");
    setRing($("b-ring"), (S.phone.battery || 0) / 100);
  }
}

/* ---------- étendue ---------- */
function renderExpanded() {
  const x = document.querySelector('[data-view="expanded"]');
  x.classList.toggle("has-media", hasMedia() && !S.picking);
  x.classList.toggle("home", !hasMedia() && !S.picking);
  x.classList.toggle("picking", S.picking && !S.mixerOpen);
  x.classList.toggle("mixing", S.mixerOpen && !S.notifOpen);
  x.classList.toggle("notifying", S.notifOpen && !S.clipOpen);
  x.classList.toggle("clipping", S.clipOpen && !S.cl.open && !S.gr.open);
  x.classList.toggle("clauding", S.cl.open && !S.gr.open);
  x.classList.toggle("grabbing", S.gr.open);
  x.classList.toggle("musicing", S.mu.open && !S.gr.open && !S.cl.open);
  document.querySelector('[data-tool="music"]').classList.toggle("mu-on", S.mu.open);
  if (S.mu.open) muRender();
  renderToolButtons();
  document.querySelector('[data-tool="grab"]').classList.toggle("gr-on", S.gr.open);
  grRender();
  document.querySelector('[data-tool="claude"]').classList.toggle("cl-on", S.cl.open);
  if (S.cl.open) clRender();
  document.querySelector('[data-tool="clip"]').classList.toggle("cb-on", S.clipOpen);
  if (S.clipOpen) renderClips();
  renderDls();
  renderVoice();
  $("x-lyric").classList.toggle("on", lyricsOn());
  updateLyric(true);
  document.querySelector('[data-tool="volume"]').classList.toggle("vol-on", S.mixerOpen);
  if (S.mixerOpen) renderMixer();
  if (S.notifOpen) renderNotifs();
  updNotifBadge();

  if (hasMedia()) {
    const m = S.media;
    const img = $("x-art");
    if (m.art) {
      if (img.getAttribute("src") !== m.art) img.src = m.art;
      img.style.display = "";
      $("x-art-fb").style.display = "none";
    } else {
      img.style.display = "none";
      const fb = $("x-art-fb");
      fb.style.display = "";
      setHTML(fb, I.note, "fb");
    }
    setTitle(m.title);
    $("x-artist").textContent = m.artist || m.album || "";
    $("x-app").textContent = m.duration > 0 ? m.app || "" : [m.app, "En direct"].filter(Boolean).join(" · ");
    $("x-eq").classList.toggle("paused", !m.playing);
    setHTML($("x-play"), m.playing ? I.pause : I.play, m.playing ? "pause" : "play");
    updateProgress();
    renderModes();
  }
  renderSpVolume();
  renderOthers();
  if (!hasMedia()) S.lol ? renderLolCard() : renderGameCard();

  // chips
  const tc = $("x-timer-chip");
  tc.classList.toggle("show", !!S.timer);
  if (S.timer) tc.innerHTML = `${I.timer}<span>${fmtTimer(timerLeft())}</span>`;
  const tb = document.querySelector('[data-tool="timer"]');
  tb.style.display = (S.showTimer && !S.hiddenTools.has("timer")) || S.timer ? "" : "none";
  tb.classList.toggle("on", !!S.timer || S.picking);

  const pvc = $("x-pv-chip");
  const pvApps = [...new Set([...S.privacy.cam, ...S.privacy.mic])];
  pvc.classList.toggle("show", pvApps.length > 0);
  if (pvApps.length) setHTML(pvc, `<span class="pv-ic ${S.privacy.cam.length ? "cam" : "mic"}">${S.privacy.cam.length ? I.cam : I.mic}</span><span class="pv-name">${esc(pvApps.join(", "))}</span>`, "pv" + pvApps.join() + S.privacy.cam.length);
  pvc.title = [S.privacy.cam.length ? "Caméra : " + S.privacy.cam.join(", ") : "", S.privacy.mic.length ? "Micro : " + S.privacy.mic.join(", ") : ""].filter(Boolean).join("\n");

  const pc = $("x-phone-chip");
  const showPhone = S.phone.connected && S.phone.battery != null;
  pc.classList.toggle("show", showPhone);
  if (showPhone) {
    setHTML(pc, `<span>${I.phone}</span><span id="pc-pct"></span>${batteryHTML("pc-batt")}`, "pc");
    $("pc-pct").textContent = S.phone.battery + "%";
    updBattery($("pc-batt"), S.phone);
  }
  updateClock();
}

function setTitle(t) {
  const span = $("x-title");
  if (span.textContent === t) return;
  span.textContent = t;
  const box = span.parentElement;
  box.classList.remove("scroll");
  requestAnimationFrame(() => {
    const over = span.scrollWidth - box.clientWidth;
    if (over > 4) {
      box.style.setProperty("--shift", `-${over + 40}px`);
      box.style.setProperty("--dur", `${Math.max(6, (over + 40) / 28)}s`);
      box.classList.add("scroll");
    }
  });
}

function mediaPos(m = S.media) {
  if (!m) return 0;
  let p = m.position + (m.playing ? (Date.now() - (m._at || S.mediaAt)) / 1000 : 0);
  if (m.duration > 0) p = Math.min(p, m.duration);
  return Math.max(0, p);
}
function updateProgress() {
  if (!hasMedia()) return;
  const m = S.media;
  const p = mediaPos();
  const bar = $("x-bar");
  bar.classList.toggle("nodur", !(m.duration > 0));
  const frac = m.duration > 0 ? p / m.duration : 0;
  $("x-fill").style.width = (frac * 100).toFixed(2) + "%";
  $("x-pos").textContent = fmtTime(p);
  $("x-rem").textContent = m.duration > 0 ? "-" + fmtTime(m.duration - p) : "";
}

function updateClock() {
  const d = new Date();
  const hm = d.toLocaleTimeString(LOC, { hour: "2-digit", minute: "2-digit" });
  $("x-clock").textContent = hm;
  $("x-clock-chip").textContent = hm;
  $("x-date").textContent = d.toLocaleDateString(LOC, { weekday: "long", day: "numeric", month: "long" });
}

/* ============================================================
   File d'alertes temporaires
   ============================================================ */
function pushTransient(kind, data, ms, urgent = false) {
  const item = { kind, data, ms };
  if (urgent) {
    S.queue.unshift(item);
    if (S.current && S.current.kind !== "secret") return showNext();
  } else {
    S.queue.push(item);
  }
  if (!S.current) showNext();
}

function showNext() {
  clearTimeout(endTimer);
  S.current = S.queue.shift() || null;
  if (S.current) {
    fillTransient(S.current);
    scheduleEnd(S.current.ms);
  }
  render();
}

function scheduleEnd(ms) {
  clearTimeout(endTimer);
  if (!S.current || S.hover) return; // en pause tant que la souris est dessus
  endTimer = setTimeout(dismiss, ms);
}

function dismiss() {
  clearTimeout(endTimer);
  S.current = null;
  showNext();
}

function fillTransient({ kind, data }) {
  if (kind === "alert") {
    const icon = $("a-icon");
    icon.className = "a-icon";
    if (data.svg) {
      icon.classList.add("plain");
      icon.innerHTML = data.svg;
    } else if (data.icon) {
      icon.innerHTML = `<img src="${data.icon}" alt="">`;
      icon.classList.add("plain");
    } else {
      icon.textContent = (data.app || "?").trim().charAt(0).toUpperCase();
      icon.style.background = gradientFor(data.app || "");
    }
    if (!data.icon && !data.svg) {} else icon.style.background = "";
    // en-tête : nom de l'appli dans sa couleur, puis la source (iPhone, compte Gmail…)
    const sub = data.phone ? "iPhone" : data.sub || "";
    $("a-app").innerHTML = `<b style="color:${data.color ? appHue(data) : "inherit"}">${esc(data.app || "")}</b>${sub ? ` · ${esc(sub)}` : ""}`;
    $("a-title").textContent = data.title || "";
    $("a-body").textContent = data.body || "";
  } else if (kind === "secret") {
    $("s-icon").innerHTML = data.source === "gmail" ? I.gmail : data.source === "phone" ? `<div style="color:var(--green);width:24px">${I.message}</div>` : I.bell;
    const label = data.code ? "Code de vérification" : "Lien de connexion";
    $("s-head").textContent = `${label} · ${data.from || ""}`;
    const codeEl = $("s-code"), linkEl = $("s-link"), side = $("s-side");
    if (data.code) {
      codeEl.style.display = "";
      linkEl.style.display = "none";
      codeEl.innerHTML = [...data.code].map((c, i) => `<span class="ch" style="animation-delay:${0.12 + i * 0.045}s">${esc(c)}</span>`).join("");
      side.innerHTML = data.copied
        ? `<div class="copied" id="s-copied"><div class="circle">${I.check}</div>Copié</div>`
        : `<button class="copy-btn" id="s-copy">Copier</button>`;
    } else {
      codeEl.style.display = "none";
      linkEl.style.display = "";
      let host = data.link;
      try { host = new URL(data.link).hostname.replace(/^www\./, ""); } catch {}
      linkEl.textContent = host;
      side.innerHTML = `<button class="open-btn" id="s-open">${I.link}Ouvrir</button>`;
    }
  } else if (kind === "osd") {
    const v = document.querySelector('[data-view="osd"]');
    const bright = data.kind === "brightness";
    v.classList.toggle("bright", bright);
    v.classList.toggle("muted", !!data.muted);
    const ic = $("o-ic");
    ic.className = "o-ic" + (bright ? " sun" : data.muted ? " muted" : "");
    setHTML(ic, bright ? I.sun : data.muted || data.value === 0 ? I.mute : I.volume, bright ? "sun" : data.muted || data.value === 0 ? "m" : "v");
    $("o-fill").style.width = (data.muted ? 0 : data.value) + "%";
    $("o-pct").textContent = data.muted ? "Muet" : data.value + "%";
  } else if (kind === "pill") {
    const v = document.querySelector('[data-view="pill"]');
    v.classList.toggle("out", !data.join);
    const av = $("p-av");
    av.innerHTML = `<img src="${esc(data.avatar)}" alt="">`;
    av.style.animation = "none"; void av.offsetWidth; av.style.animation = "";
    $("p-t").innerHTML = `${esc(data.name)} <span>${data.join ? "a rejoint" : "est parti"}</span>`;
    $("p-st").className = "p-st " + (data.join ? "in" : "out");
  } else if (kind === "charge") {
    $("ch-name").textContent = data.name || "iPhone";
    $("ch-state").textContent = data.full ? "Chargé" : data.charging ? "En charge" : "Connecté";
    $("ch-state").style.color = data.charging || data.full ? "var(--green)" : "var(--fg2)";
    $("ch-pct").textContent = data.battery != null ? data.battery + "%" : "--";
    $("ch-pct").style.color = (data.battery ?? 100) <= 20 && !data.charging ? "#ff453a" : "var(--green)";
    const b = $("ch-batt");
    b.style.setProperty("--lvl", "0%");
    requestAnimationFrame(() => setTimeout(() => updBattery(b, data), 120));
  } else if (kind === "shot") {
    $("sh-thumb").src = data.thumb;
    $("sh-thumb").style.animation = "none";
    void $("sh-thumb").offsetWidth;
    $("sh-thumb").style.animation = "";
    fillShotInfo(data);
  }
}

/* ---- capture : texte (OCR) et QR code, analysés en local par Rust ---- */
function qrKind(s) {
  if (/^https?:\/\//i.test(s)) {
    try { return { kind: "url", label: new URL(s).host.replace(/^www\./, "") }; } catch { /* lien mal formé */ }
  }
  if (/^WIFI:/i.test(s)) {
    const f = (k) => { const m = s.match(new RegExp("[:;]" + k + ":((?:\\\\.|[^;\\\\])*)", "i")); return m ? m[1].replace(/\\(.)/g, "$1") : ""; };
    return { kind: "wifi", label: "Wi-Fi « " + (f("S") || "?") + " »", pass: f("P") };
  }
  return { kind: "text", label: s.replace(/\s+/g, " ").slice(0, 60) };
}

function fillShotInfo(d) {
  const acts = [];
  if (d.qr) {
    const q = qrKind(d.qr);
    $("sh-title").textContent = "QR code détecté";
    $("sh-sub").textContent = q.label;
    $("sh-sub").title = d.qr;
    if (q.kind === "url") acts.push(`<button class="sh-btn pri" data-sh="open">${I.link}Ouvrir</button>`);
    else if (q.kind === "wifi" && q.pass) acts.push(`<button class="sh-btn pri" data-sh="wifi">Copier le mot de passe</button>`);
    else acts.push(`<button class="sh-btn pri" data-sh="qr">Copier</button>`);
  } else {
    $("sh-title").textContent = "Capture enregistrée";
    $("sh-sub").textContent = d.path ? "Copiée · cliquer pour ouvrir" : "Copiée dans le presse-papiers";
    $("sh-sub").title = "";
  }
  if (d.text) acts.push(`<button class="sh-btn" data-sh="text">${I.copy || ""}Copier le texte</button>`);
  $("sh-acts").innerHTML = acts.join("");
  $("sh-acts").style.display = acts.length ? "" : "none";
}

function shotAction(b) {
  const d = S.current && S.current.kind === "shot" ? S.current.data : null;
  if (!d) return;
  const a = b.dataset.sh;
  if (a === "open" && d.qr) {
    invoke("open_url", { url: d.qr });
    return dismiss();
  }
  const text = a === "text" ? d.text : a === "wifi" ? qrKind(d.qr).pass : d.qr;
  if (!text) return;
  invoke("copy_text", { text });
  b.classList.add("done");
  b.innerHTML = `${I.check}Copié`;
  scheduleEnd(2600);
}

listen("shot_info", (e) => {
  const info = e.payload || {};
  if (!info.text && !info.qr) return;
  const it = S.current && S.current.kind === "shot" && S.current.data.id === info.id
    ? S.current
    : S.queue.find((q) => q.kind === "shot" && q.data.id === info.id);
  if (!it) return;
  Object.assign(it.data, { text: info.text || null, qr: info.qr || null });
  it.ms = Math.max(it.ms, 9000);
  if (it === S.current) {
    fillShotInfo(it.data);
    scheduleEnd(9000);
    render();
  }
});

/* ============================================================
   Interactions
   ============================================================ */
// survol → géré par Rust (hit-test au curseur, click-through ailleurs)
listen("hover", (e) => {
  clearTimeout(hoverTimer);
  const inside = !!e.payload;
  S.pointerIn = inside;
  hoverTimer = setTimeout(() => setHover(inside), inside ? 70 : 260);
});

function setHover(inside) {
  {
    if (S.hover === inside) return;
    // on garde le panneau ouvert pendant qu'on règle un curseur (replié à la fin du glissé)
    if (!inside && S.mixDrag) return;
    S.hover = inside;
    if (!inside) {
      S.xMinH = 0;
      releaseKeyboard();
      S.clipOpen = false;
      S.cl.open = false;
      S.gr.open = false;
      S.mu.open = false;
      S.picking = false;
      S.mixerOpen = false;
      S.notifOpen = false;
      if (S.current) scheduleEnd(2200);
    } else {
      clearTimeout(endTimer);
      refreshSpotify();
    }
    render();
  }
}

island.addEventListener("contextmenu", (e) => {
  e.preventDefault();
  if (S.current) dismiss();
});
document.addEventListener("contextmenu", (e) => e.preventDefault());

island.addEventListener("click", (e) => {
  const t = e.target.closest("button, [data-pick], [data-view], .bar");
  if (!t) return;

  // boutons
  if (t.dataset.sh) return shotAction(t);
  if (t.dataset.media) {
    const m = t.dataset.id ? S.medias.find((x) => x.id === t.dataset.id) : S.media;
    if (t.dataset.media === "play_pause" && m) {
      m.position = mediaPos(m);
      m._at = Date.now();
      m.playing = !m.playing;
      if (m === S.media && m.playing) S.lastPlayingAt = Date.now();
      render();
    }
    return invoke("media_control", { id: m ? m.id : null, action: t.dataset.media });
  }
  if (t.dataset.mode) return toggleMode(t.dataset.mode);
  if (t.dataset.pick) {
    S.mediaSel = t.dataset.pick;
    S.selPlayingAt = Date.now();
    applyMedias();
    return render();
  }
  if (t.dataset.mute) return toggleMute(t.dataset.mute, t.dataset.target);
  if (t.dataset.tool) return tool(t.dataset.tool);
  if (t.dataset.game) return launchGame(t);
  if (t.id === "x-timer-chip") { S.timer = null; return render(); }
  if (t.dataset.min) {
    const min = +t.dataset.min;
    S.picking = false;
    if (min > 0) S.timer = { end: Date.now() + min * 60000, total: min * 60 };
    return render();
  }
  if (t.classList.contains("bar") && S.media && S.media.duration > 0) {
    const r = t.getBoundingClientRect();
    const frac = Math.min(1, Math.max(0, (e.clientX - r.left) / r.width));
    const sec = frac * S.media.duration;
    S.media.position = sec;
    S.media._at = Date.now();
    updateProgress();
    return invoke("media_seek", { id: S.media.id, seconds: sec });
  }
  if (t.id === "s-open" && S.current) {
    invoke("open_url", { url: S.current.data.link });
    return dismiss();
  }
  if (t.id === "s-copy" && S.current) {
    invoke("copy_text", { text: S.current.data.code });
    $("s-side").innerHTML = `<div class="copied"><div class="circle">${I.check}</div>Copié</div>`;
    return;
  }

  // clic sur une vue temporaire
  const v = t.dataset.view;
  if (v === "secret" && S.current) {
    const d = S.current.data;
    if (d.code) {
      invoke("copy_text", { text: d.code });
      const c = $("s-copied");
      if (c) { c.classList.remove("again"); void c.offsetWidth; c.classList.add("again"); }
      else $("s-side").innerHTML = `<div class="copied"><div class="circle">${I.check}</div>Copié</div>`;
    } else {
      invoke("copy_text", { text: d.link });
    }
  } else if (v === "shot" && S.current) {
    invoke("open_screenshot", { path: S.current.data.path || null });
    dismiss();
  } else if (v === "alert" || v === "charge") {
    const act = S.current && S.current.data.onClick;
    if (act) act();
    dismiss();
  }
});

function tool(name) {
  if (name !== "claude" && name !== "settings") S.cl.open = false;
  if (name !== "grab" && name !== "settings") S.gr.open = false;
  if (name !== "music" && name !== "settings") S.mu.open = false;
  if (name === "music") {
    S.mu.open = !S.mu.open;
    S.clipOpen = S.picking = S.mixerOpen = S.notifOpen = false;
    if (S.mu.open) muLoad();
    else releaseKeyboard();
    render();
    return;
  }
  if (name === "grab") {
    if (S.gr.open) {
      S.gr.open = false;
      releaseKeyboard();
      render();
    } else {
      grOpenPanel();
      setTimeout(() => { if (S.gr.open && S.hover && !S.gr.info && !S.gr.probing) grabKeyboard(grQ); }, 60);
    }
    return;
  }
  if (name === "claude") {
    S.cl.open = !S.cl.open;
    S.gr.open = false;
    S.clipOpen = S.picking = S.mixerOpen = S.notifOpen = false;
    if (S.cl.open) {
      clRefresh();
      render();
      setTimeout(() => { if (S.cl.open && S.hover) grabKeyboard(clQ); }, 60);
    } else {
      releaseKeyboard();
      render();
    }
    return;
  }
  if (name === "region" || name === "full") {
    S.hover = false;
    render();
    // petit délai pour que l'island se replie avant la capture
    setTimeout(() => invoke("screenshot", { kind: name }), name === "full" ? 260 : 120);
  } else if (name === "clip") {
    S.clipOpen = !S.clipOpen;
    if (!S.clipOpen) releaseKeyboard();
    S.picking = false;
    S.mixerOpen = false;
    S.notifOpen = false;
    render();
  } else if (name === "timer") {
    S.clipOpen = false;
    S.picking = !S.picking;
    S.mixerOpen = false;
    S.notifOpen = false;
    render();
  } else if (name === "notifs") {
    S.clipOpen = false;
    S.notifOpen = !S.notifOpen;
    S.mixerOpen = false;
    S.picking = false;
    S.unread = 0;
    render();
  } else if (name === "volume") {
    S.clipOpen = false;
    S.mixerOpen = !S.mixerOpen;
    S.notifOpen = false;
    S.picking = false;
    if (S.mixerOpen) refreshMixer();
    render();
  } else if (name === "settings") {
    invoke("open_settings");
  }
}

// grille du minuteur
$("tp-grid").innerHTML =
  [1, 3, 5, 10, 15, 25, 45].map((m) => `<button data-min="${m}">${m} min</button>`).join("") +
  `<button class="cancel" data-min="0">Annuler</button>`;

/* ============================================================
   Événements du backend Rust
   ============================================================ */
listen("media", (e) => {
  const now = Date.now();
  S.medias = (Array.isArray(e.payload) ? e.payload : e.payload ? [e.payload] : []).filter((m) => m && m.title);
  S.medias.forEach((m) => { m._at = now; attachArt(m); });
  for (const id in ARTS) if (!S.medias.some((m) => m.id === id)) delete ARTS[id];
  S.mediaAt = now;
  applyMedias();
  render();
});

/* Pochettes : envoyées une seule fois par morceau (commande media_art), pas à chaque mise à jour */
const ARTS = {}; // id → { key, art, loading }
function attachArt(m) {
  if (m.art !== undefined) return; // démo : pochette fournie directement
  const c = ARTS[m.id];
  m.art = c && c.art ? c.art : null; // l'ancienne pochette reste le temps de charger la nouvelle
  if (!m.art_key || (c && (c.key === m.art_key || c.loading === m.art_key))) return;
  ARTS[m.id] = { ...(c || {}), loading: m.art_key };
  invoke("media_art", { id: m.id }).then((r) => {
    if (!r) return;
    ARTS[m.id] = { key: r.key, art: r.art };
    for (const x of S.medias) if (x.id === m.id) x.art = r.art;
    applyMedias();
    render();
  }).catch(() => { if (ARTS[m.id]) ARTS[m.id].loading = 0; });
}

/* Choix de la source principale.
   Si tu en choisis une (clic sur sa ligne), elle reste en premier plan jusqu'à ce que tu en choisisses une autre.
   Sinon, la source en cours reste devant même pendant le blanc entre deux morceaux. */
function applyMedias() {
  const list = S.medias;
  const prev = S.media;
  const prevArt = prev && prev.art;
  const now = Date.now();
  let sel = S.mediaSel && list.find((m) => m.id === S.mediaSel);
  if (sel && sel.playing) S.selPlayingAt = now;
  const keep = prev && list.find((m) => m.id === prev.id);
  if (keep && keep.playing) S.keepPlayingAt = now;
  const playing = list.filter((m) => m.playing);
  // un choix manuel tombe quand la source n'existe plus ou reste arrêtée alors qu'autre chose joue
  if (S.mediaSel && (!sel || (!sel.playing && playing.length && now - (S.selPlayingAt || 0) > 15000))) { S.mediaSel = null; sel = null; }
  const keepAlive = keep && (keep.playing || now - (S.keepPlayingAt || 0) < 6000);
  // une musique qui joue passe devant un live / une vidéo (paroles, progression) ;
  // la source déjà affichée garde la main face à une source du même genre
  const music = playing.filter(isMusic);
  const keepOk = keepAlive && (isMusic(keep) || !music.length);
  const main = sel || (keepOk ? keep : null) || music.find((m) => m.current) || music[0] || playing.find((m) => m.current) || playing[0] || keep || list.find((m) => m.current) || list[0] || null;
  S.media = main;
  S.media2 = (main && playing.find((m) => m !== main)) || null;
  if (main && main.playing) S.lastPlayingAt = now;
  if (main && main.art && main.art !== S.accentArt) computeAccent(main.art);
  maybeFetchLyrics();
}

/* Autres sessions dans la vue étendue */
function otherMedias() {
  if (!hasMedia()) return [];
  return S.medias.filter((m) => m !== S.media).sort((a, b) => b.playing - a.playing).slice(0, 2);
}
function renderOthers() {
  const box = $("x-others");
  const others = otherMedias();
  const key = others.map((m) => [m.id, m.title, m.playing, (m.art || "").length].join("|")).join("~");
  setHTML(box, others.map((m) => {
    const art = m.art ? `<img class="o-art" src="${m.art}" alt="">` : `<div class="o-art fb">${I.note}</div>`;
    const sub = [m.artist, m.app || (m.duration > 0 ? "" : "En direct")].filter(Boolean).join(" · ");
    return `<div class="o-row" data-pick="${esc(m.id)}" title="Afficher cette source">${art}
      <div class="o-text"><b>${esc(m.title)}</b>${sub ? ` <span>— ${esc(sub)}</span>` : ""}</div>
      <div class="eq ${m.playing ? "" : "paused"}"><i></i><i></i><i></i></div>
      <button class="o-btn" data-media="play_pause" data-id="${esc(m.id)}">${m.playing ? I.pause : I.play}</button>
      <button class="o-btn" data-media="next" data-id="${esc(m.id)}">${I.next}</button></div>`;
  }).join(""), key);
}

/* ============================================================
   Mixeur : volume de chaque sortie audio et de chaque appli
   ============================================================ */
let mixTimer = null;
async function refreshMixer() {
  clearTimeout(mixTimer);
  if (!S.mixerOpen) return;
  try {
    S.mixer = await invoke("get_mixer");
  } catch (err) {
    S.mixer = { devices: [], apps: [], error: String(err) };
  }
  if (!S.mixer) S.mixer = { devices: [], apps: [] };
  if (S.mixerOpen && S.view === "expanded") render();
  mixTimer = setTimeout(refreshMixer, 1200);
}
// seulement la sortie par défaut
const mixerDevices = () => (S.mixer ? S.mixer.devices.filter((d) => d.default) : []);
// Spotify connecté : la ligne Spotifast/Soloist (volume Windows) est remplacée par le volume de l'appli
const spMixer = () => !!(S.spOn && S.sp && S.sp.connected && S.sp.volume != null && S.sp.supports_volume !== false);
const mixerApps = () => (S.mixer ? S.mixer.apps.filter((a) => !(spMixer() && isSpotifyApp(a.name + " " + a.key))) : []);
function mixerRows() {
  const m = S.mixer;
  if (!m) return 3;
  return Math.max(1, mixerDevices().length) + Math.max(1, mixerApps().length + (spMixer() ? 1 : 0));
}
function mixerHeight() {
  const panel = Math.min(236, 2 * 20 + 6 + mixerRows() * 36);
  return Math.min(318, 18 + panel + 14 + 52);
}
function isHeadset(name) {
  return /casque|headphone|headset|écouteur|ecouteur|airpods|buds|arctis|hyperx|wh-|wf-/i.test(name);
}
function renderMixer() {
  const box = $("x-mixer");
  const m = S.mixer;
  if (!m) return setHTML(box, `<div class="mx-empty">Chargement…</div>`, "loading");
  const devices = mixerDevices();
  const appList = mixerApps();
  const sp = spMixer();
  const devKey = devices.map((d) => d.id).join("|");
  const appKey = appList.map((a) => a.key + a.active).join("|");
  const key = devKey + "~" + appKey + (sp ? "~sp" : "") + (m.error || "");
  const row = (kind, k, icon, name, sub, cls) =>
    `<div class="mx-row ${cls}" data-row="${kind}:${esc(k)}">
      <div class="mx-ic">${icon}</div>
      <div class="mx-name" title="${esc(name)}">${esc(name)}${sub ? `<small${kind === "sp" ? ' class="sp-tag"' : ""}>${esc(sub)}</small>` : ""}</div>
      <input type="range" min="0" max="100" step="1" data-kind="${kind}" data-key="${esc(k)}">
      <div class="mx-pct"></div>
      <button class="mx-mute" data-mute="${kind}" data-target="${esc(k)}" title="Couper / rétablir le son"></button>
    </div>`;
  const devs = devices.map((d) => {
    const [main, sub] = d.name.split(" · ");
    return row("dev", d.id, isHeadset(d.name) ? I.headphones : I.speaker, main, sub || "", "def");
  }).join("");
  const letter = (a) => `<span style="background:${gradientFor(a.name)};width:22px;height:22px;border-radius:6px;display:grid;place-items:center">${esc(a.name.charAt(0).toUpperCase())}</span>`;
  let spRow = "";
  if (sp) {
    const src = m.apps.find((a) => /spotif/i.test(a.name + " " + a.key)) || m.apps.find((a) => isSpotifyApp(a.name + " " + a.key));
    const name = src && /spotif/i.test(src.name) ? src.name : "Spotifast";
    spRow = row("sp", "sp", src && src.icon ? `<img src="${src.icon}" alt="">` : letter({ name }), name, "Volume Spotify (appli)", "");
  }
  const apps = spRow + appList.map((a) =>
    row("app", a.key, a.icon ? `<img src="${a.icon}" alt="">` : letter(a), a.name, a.active ? "" : "inactif", a.active ? "" : "idle")
  ).join("");
  setHTML(box,
    `<div class="mx-h">Sortie audio</div>${devs || `<div class="mx-empty">${esc(m.error || "Aucune sortie active")}</div>`}
     <div class="mx-h">Applications</div>${apps || `<div class="mx-empty">Aucune appli ne joue de son</div>`}`,
    key);
  for (const d of devices) updMixRow("dev", d.id, d.volume, d.muted);
  if (sp) updMixRow("sp", "sp", S.sp.volume / 100, false);
  for (const a of appList) updMixRow("app", a.key, a.volume, a.muted);
}
function findRow(kind, key) {
  return [...document.querySelectorAll(".mx-row")].find((r) => r.dataset.row === kind + ":" + key);
}
function updMixRow(kind, key, vol, muted) {
  const r = findRow(kind, key);
  if (!r) return;
  const input = r.querySelector("input");
  const pct = Math.round(vol * 100);
  if (S.mixDrag !== kind + ":" + key) {
    input.value = pct;
    input.style.setProperty("--v", pct + "%");
    r.querySelector(".mx-pct").textContent = pct + "%";
  }
  r.classList.toggle("muted", !!muted);
  setHTML(r.querySelector(".mx-mute"), muted || pct === 0 ? I.mute : I.volume, muted || pct === 0 ? "m" : "v");
}
function mixItem(kind, key) {
  const m = S.mixer;
  if (kind === "sp") return S.sp ? { volume: (S.sp.volume || 0) / 100, muted: false } : null;
  if (!m) return null;
  return kind === "dev" ? m.devices.find((d) => d.id === key) : m.apps.find((a) => a.key === key);
}
let mixSendAt = 0, mixSendTimer = null;
function sendVolume(kind, key, vol) {
  if (kind === "sp") return setSpVolume(vol * 100, "sp");
  const it = mixItem(kind, key);
  if (it) { it.volume = vol; if (vol > 0) it.muted = false; }
  const go = () => {
    mixSendAt = Date.now();
    invoke(kind === "dev" ? "set_device_volume" : "set_app_volume", kind === "dev" ? { id: key, volume: vol } : { key, volume: vol });
  };
  clearTimeout(mixSendTimer);
  // limité à ~20 appels / s pendant le glissé, la dernière valeur part toujours
  if (Date.now() - mixSendAt > 50) go();
  else mixSendTimer = setTimeout(go, 50);
}
function toggleMute(kind, key) {
  if (kind === "sp") return toggleSpMute("sp");
  const it = mixItem(kind, key);
  if (!it) return;
  it.muted = !it.muted;
  updMixRow(kind, key, it.volume, it.muted);
  invoke(kind === "dev" ? "set_device_volume" : "set_app_volume", kind === "dev" ? { id: key, muted: it.muted } : { key, muted: it.muted });
}
function setRowValue(input, v) {
  input.value = v;
  input.style.setProperty("--v", v + "%");
  input.closest(".mx-row").querySelector(".mx-pct").textContent = v + "%";
  sendVolume(input.dataset.kind, input.dataset.key, v / 100);
  const it = mixItem(input.dataset.kind, input.dataset.key);
  if (it) updMixRow(input.dataset.kind, input.dataset.key, it.volume, it.muted);
}
const mixerBox = $("x-mixer");
let dragEnd = null;
mixerBox.addEventListener("input", (e) => {
  const t = e.target;
  if (t.type !== "range") return;
  S.mixDrag = t.dataset.kind + ":" + t.dataset.key;
  clearTimeout(dragEnd);
  setRowValue(t, +t.value);
});
mixerBox.addEventListener("change", () => {
  clearTimeout(dragEnd);
  dragEnd = setTimeout(() => {
    S.mixDrag = null;
    if (S.pointerIn === false) setHover(false);
  }, 600);
});
// molette sur une ligne = ±4 %
mixerBox.addEventListener("wheel", (e) => {
  const r = e.target.closest(".mx-row");
  if (!r) return;
  e.preventDefault();
  const input = r.querySelector("input");
  setRowValue(input, Math.max(0, Math.min(100, +input.value + (e.deltaY < 0 ? 4 : -4))));
}, { passive: false });

/* ============================================================
   Spotify (API Web) : volume DANS l'appli (Spotifast / Spotify), pas le volume Windows
   ============================================================ */
function isSpotifyApp(s) {
  return /spotif|soloist/i.test(s || "");
}
function spVisible() {
  return spMixer() && hasMedia() && isSpotifyApp((S.media.app || "") + " " + (S.media.id || ""));
}
let spTimer = null, spSendAt = 0, spSendTimer = null, spHoldUntil = 0, spPrev = 50;
async function refreshSpotify() {
  clearTimeout(spTimer);
  if (!S.spOn || !S.hover) return;
  try {
    const st = await invoke("spotify_state");
    // juste après un réglage, on garde la valeur locale (l'API met un moment à la refléter)
    if (st && S.sp && Date.now() < spHoldUntil) st.volume = S.sp.volume;
    S.sp = st;
  } catch {}
  if (S.hover) render();
  spTimer = setTimeout(refreshSpotify, 2000);
}
/* La barre de volume du lecteur règle le volume DANS l'appli : Ma musique (lecteur natif de l'île) ou Spotify (API). */
function isMuMedia() {
  return hasMedia() && S.media.app === "Ma musique";
}
function volSrc() {
  return isMuMedia() ? "mu" : spVisible() && S.sp ? "sp" : null;
}
function curVol() {
  return volSrc() === "mu" ? S.muVol : S.sp ? S.sp.volume : 0;
}
let muVolTimer = null;
function setSpVolume(v, src = volSrc()) {
  v = Math.max(0, Math.min(100, Math.round(v)));
  if (src === "mu") {
    S.muVol = v;
    renderSpVolume();
    // appel local très léger : on n'envoie que la dernière valeur toutes les 40 ms pendant le glissé
    clearTimeout(muVolTimer);
    muVolTimer = setTimeout(() => invoke("music_volume", { volume: S.muVol }).catch(() => {}), 40);
    return;
  }
  if (!S.sp) return;
  S.sp.volume = v;
  spHoldUntil = Date.now() + 3000;
  renderSpVolume();
  if (S.mixerOpen) updMixRow("sp", "sp", v / 100, false);
  const go = () => {
    spSendAt = Date.now();
    spHoldUntil = Date.now() + 3000;
    invoke("spotify_set_volume", { volume: v }).catch((err) => console.warn("spotify volume", err));
  };
  clearTimeout(spSendTimer);
  // l'API Spotify limite le débit : ~5 appels / s max pendant le glissé, la dernière valeur part toujours
  if (Date.now() - spSendAt > 200) go();
  else spSendTimer = setTimeout(go, 200);
}
function toggleSpMute(src = volSrc()) {
  if (!src) return;
  const v = src === "mu" ? S.muVol : S.sp ? S.sp.volume : 0;
  if (v > 0) { spPrev = v; setSpVolume(0, src); }
  else setSpVolume(spPrev || 50, src);
}
function renderSpVolume() {
  const on = !!volSrc();
  $("x-vol").classList.toggle("show", on);
  if (!on) return;
  const v = curVol();
  setHTML($("xv-ic"), v === 0 ? I.mute : I.volLow, v === 0 ? "m" : "v");
  const r = $("xv-range");
  if (S.mixDrag !== "sp:xv") r.value = v;
  r.style.setProperty("--v", r.value + "%");
  $("x-vol").title = `Volume de ${S.media.app || "Spotify"} : ${r.value} %`;
}
$("xv-hi").innerHTML = I.volume;

/* ---------- aléatoire / répéter ----------
   Lus dans les contrôles média de Windows quand l'appli les expose (ex. Ma musique),
   sinon via l'API Spotify pour Spotify / Spotifast (compte relié dans les réglages). */
const SP_REPEAT = { off: 0, track: 1, context: 2 };
function modeState() {
  const m = S.media;
  if (!m) return null;
  if (m.shuffle != null || m.repeat != null) return { src: "win", shuffle: m.shuffle, repeat: m.repeat };
  if (S.spOn && S.sp && S.sp.connected && S.sp.shuffle != null && isSpotifyApp((m.app || "") + " " + (m.id || "")))
    return { src: "sp", shuffle: S.sp.shuffle, repeat: SP_REPEAT[S.sp.repeat] ?? 0 };
  return null;
}
function renderModes() {
  const st = Date.now() < S.modeHold && S.modeLocal ? S.modeLocal : modeState();
  const sh = $("x-shuf"), rp = $("x-rep");
  sh.classList.toggle("na", !st || st.shuffle == null);
  rp.classList.toggle("na", !st || st.repeat == null);
  if (!st) return;
  sh.classList.toggle("on", !!st.shuffle);
  setHTML(sh, I.shuffle, "s");
  rp.classList.toggle("on", st.repeat > 0);
  setHTML(rp, st.repeat === 1 ? I.repeat1 : I.repeat, "r" + (st.repeat === 1));
  sh.title = st.shuffle ? "Lecture aléatoire : activée" : "Lecture aléatoire";
  rp.title = ["Répéter", "Répéter : ce morceau", "Répéter : toute la liste"][st.repeat || 0];
}
function toggleMode(kind) {
  const st = Date.now() < S.modeHold && S.modeLocal ? S.modeLocal : modeState();
  if (!st || !S.media) return;
  const next = { ...st };
  // répéter : non → toute la liste → ce morceau → non
  if (kind === "shuffle") next.shuffle = !st.shuffle;
  else next.repeat = st.repeat === 0 ? 2 : st.repeat === 2 ? 1 : 0;
  S.modeLocal = next;
  S.modeHold = Date.now() + 2500;
  renderModes();
  if (st.src === "win") return invoke("media_control", { id: S.media.id, action: kind }).catch(() => {});
  const args = kind === "shuffle" ? { shuffle: next.shuffle, repeat: null } : { shuffle: null, repeat: ["off", "track", "context"][next.repeat] };
  if (S.sp) { if (kind === "shuffle") S.sp.shuffle = next.shuffle; else S.sp.repeat = args.repeat; }
  invoke("spotify_mode", args).catch((err) => pushTransient("alert", { app: "Spotify", title: kind === "shuffle" ? "Lecture aléatoire" : "Répéter", body: String(err), svg: I.spotify }, 4000));
}
const xvRange = $("xv-range");
xvRange.addEventListener("input", () => {
  S.mixDrag = "sp:xv";
  clearTimeout(dragEnd);
  setSpVolume(+xvRange.value);
});
xvRange.addEventListener("change", () => {
  clearTimeout(dragEnd);
  dragEnd = setTimeout(() => {
    S.mixDrag = null;
    if (S.pointerIn === false) setHover(false);
  }, 600);
});
$("x-vol").addEventListener("wheel", (e) => {
  if (!volSrc()) return;
  e.preventDefault();
  setSpVolume(curVol() + (e.deltaY < 0 ? 4 : -4));
}, { passive: false });
$("xv-ic").addEventListener("click", () => toggleSpMute());
invoke("music_volume", { volume: null }).then((v) => { if (typeof v === "number") S.muVol = v; }).catch(() => {});


listen("notification", async (e) => {
  const n = e.payload;
  const color = n.icon ? await appColor(n.app, n.icon) : null;
  const data = { app: n.app, title: n.title, body: n.body, icon: n.icon, phone: n.phone, color, wid: n.id };
  addNotif(data);
  pushTransient("alert", data, 5500);
});

listen("mail", (e) => {
  const m = e.payload;
  const data = { app: "Gmail", sub: m.account, title: m.from, body: m.subject || m.snippet, svg: I.gmail, color: [4, 80] };
  addNotif(data);
  pushTransient("alert", data, 6000);
});

/* ============================================================
   Centre de notifications (comme sur iPhone) : regroupées par appli, effaçables
   ============================================================ */
const NOTIF_MAX = 60;
let notifSeq = 0;
function addNotif(d) {
  S.notifs.unshift({ uid: ++notifSeq, at: Date.now(), ...d, onClick: undefined });
  if (S.notifs.length > NOTIF_MAX) S.notifs.length = NOTIF_MAX;
  S.unread++;
  if (S.view === "expanded") render();
}
function notifGroups() {
  const groups = new Map();
  for (const n of S.notifs) {
    const k = (n.phone ? "p:" : "") + n.app;
    if (!groups.has(k)) groups.set(k, { key: k, app: n.app, phone: n.phone, icon: n.icon, svg: n.svg, color: n.color, items: [] });
    const g = groups.get(k);
    g.items.push(n);
    if (!g.icon && n.icon) { g.icon = n.icon; g.color = n.color; }
  }
  return [...groups.values()];
}
function clearNotifs(pred) {
  const gone = S.notifs.filter(pred);
  S.notifs = S.notifs.filter((n) => !pred(n));
  const ids = gone.map((n) => n.wid).filter((x) => x != null);
  // on les retire aussi du centre de notifications de Windows
  if (ids.length) invoke("clear_notifications", { ids }).catch(() => {});
  render();
}
function ago(t) {
  const s = (Date.now() - t) / 1000;
  if (s < 60) return "maintenant";
  if (s < 3600) return Math.floor(s / 60) + " min";
  if (s < 6 * 3600) return Math.floor(s / 3600) + " h";
  return new Date(t).toLocaleTimeString(LOC, { hour: "2-digit", minute: "2-digit" });
}
const appHue = (n) => (n.color ? `hsl(${n.color[0]} ${Math.max(55, n.color[1])}% 66%)` : "var(--fg2)");
function notifIcon(n, cls = "") {
  if (n.icon) return `<img class="${cls}" src="${n.icon}" alt="">`;
  if (n.svg) return `<span class="${cls} svg">${n.svg}</span>`;
  return `<span class="${cls} letter" style="background:${gradientFor(n.app || "")}">${esc((n.app || "?").charAt(0).toUpperCase())}</span>`;
}
function notifPanelHeight() {
  const g = notifGroups();
  let h = 30; // en-tête
  if (!g.length) h += 70;
  for (const gr of g) {
    const shown = S.notifExpanded.has(gr.key) ? gr.items.length : Math.min(2, gr.items.length);
    h += 34 + shown * 48 + (gr.items.length > shown ? 22 : 0) + 8;
  }
  return Math.min(318, 18 + Math.min(h, 236) + 14 + 52);
}
function renderNotifs() {
  const box = $("x-notifs");
  const groups = notifGroups();
  const key = S.notifs.map((n) => n.uid).join(",") + "|" + [...S.notifExpanded].join(",") + "|" + Math.floor(Date.now() / 30000);
  setHTML(box, `
    <div class="nc-head"><span>Notifications</span>${groups.length ? `<button class="nc-clear" data-nclear="all">Tout effacer</button>` : ""}</div>
    ${groups.length ? groups.map((g) => {
      const open = S.notifExpanded.has(g.key);
      const items = open ? g.items : g.items.slice(0, 2);
      const more = g.items.length - items.length;
      return `<div class="nc-group ${more > 0 ? "stacked" : ""}">
        <div class="nc-ghead">
          ${notifIcon(g, "nc-gicon")}
          <span class="nc-app" style="color:${appHue(g)}">${esc(g.app)}</span>
          ${g.phone ? `<span class="nc-src">iPhone</span>` : ""}
          <span class="nc-count">${g.items.length > 1 ? g.items.length : ""}</span>
          <span class="nc-time">${ago(g.items[0].at)}</span>
          <button class="nc-x" data-nclear="${esc(g.key)}" title="Effacer">${I.close}</button>
        </div>
        ${items.map((n) => `<div class="nc-item" data-nitem="${n.uid}">
          <div class="nc-t">${esc(n.title)}${n.sub ? ` <small>${esc(n.sub)}</small>` : ""}</div>
          <div class="nc-b">${esc(n.body || "")}</div>
          <button class="nc-ix" data-nrm="${n.uid}" title="Effacer">${I.close}</button>
        </div>`).join("")}
        ${more > 0 ? `<button class="nc-more" data-nmore="${esc(g.key)}">+ ${more} autre${more > 1 ? "s" : ""}</button>` : open && g.items.length > 2 ? `<button class="nc-more" data-nmore="${esc(g.key)}">Réduire</button>` : ""}
      </div>`;
    }).join("") : `<div class="nc-empty">${I.bell}<span>Aucune notification</span></div>`}`, key);
}
$("x-notifs").addEventListener("click", (e) => {
  const t = e.target.closest("[data-nclear], [data-nrm], [data-nmore]");
  if (!t) return;
  e.stopPropagation();
  if (t.dataset.nclear === "all") return clearNotifs(() => true);
  if (t.dataset.nclear) return clearNotifs((n) => (n.phone ? "p:" : "") + n.app === t.dataset.nclear);
  if (t.dataset.nrm) return clearNotifs((n) => n.uid === +t.dataset.nrm);
  if (t.dataset.nmore) {
    const k = t.dataset.nmore;
    S.notifExpanded.has(k) ? S.notifExpanded.delete(k) : S.notifExpanded.add(k);
    render();
  }
});
function updNotifBadge() {
  const b = document.querySelector('[data-tool="notifs"]');
  if (!b) return;
  b.classList.toggle("nc-on", S.notifOpen);
  let badge = b.querySelector(".nbadge");
  if (!badge) { badge = document.createElement("span"); badge.className = "nbadge"; b.appendChild(badge); }
  badge.textContent = S.unread > 9 ? "9+" : S.unread || "";
  badge.style.display = S.unread ? "" : "none";
}

/* Couleur dominante d'une icône d'appli (mise en cache par appli) → [teinte, saturation] */
const appColors = {};
function appColor(app, src) {
  if (appColors[app]) return Promise.resolve(appColors[app]);
  return dominantHsl(src).then((c) => (appColors[app] = c ? [c[0], c[1]] : null)).catch(() => null);
}

listen("secret", (e) => {
  const d = e.payload;
  pushTransient("secret", d, d.code ? 10000 : 16000, true);
});

listen("iphone", (e) => {
  const p = e.payload;
  const was = S.phone;
  S.phone = p;
  if (p.connected && p.error && !S.warned.has("ip:" + p.error)) {
    S.warned.add("ip:" + p.error);
    pushTransient("alert", {
      app: "iPhone détecté", title: p.name || "iPhone", body: p.error, svg: `<div style="width:26px">${I.phone}</div>`,
      onClick: p.error_kind ? () => invoke("open_settings") : null,
    }, 9000);
  } else if (p.connected && p.battery != null && (!was.connected || was.battery == null || (!was.charging && p.charging))) {
    pushTransient("charge", p, 4800);
  }
  render();
});

listen("screenshot", (e) => pushTransient("shot", e.payload, 5000, true));

listen("fullscreen", (e) => { S.fullscreen = !!e.payload; render(); });

listen("notif-access", (e) => {
  if (e.payload === "denied" && !S.warned.has("notif")) {
    S.warned.add("notif");
    pushTransient("alert", {
      app: "WowIsland", title: "Accès aux notifications désactivé",
      body: "Clique ici pour l'autoriser dans les paramètres Windows.", svg: `<div style="width:26px">${I.bell}</div>`,
      onClick: () => invoke("open_notification_settings"),
    }, 9000);
  }
});

listen("gmail-status", (e) => {
  const s = e.payload;
  if (!s.ok && !S.warned.has("gm:" + s.account + s.message)) {
    S.warned.add("gm:" + s.account + s.message);
    pushTransient("alert", { app: "Gmail", title: s.account, body: "Connexion impossible : " + s.message, svg: I.gmail, onClick: () => invoke("open_settings") }, 7000);
  }
});

listen("settings-changed", loadUiConfig);

async function loadUiConfig() {
  try {
    const c = await invoke("get_ui_config");
    if (c) {
      S.hideFs = !!c.hide_in_fullscreen;
      S.showTimer = !!c.show_timer;
      S.hiddenTools = new Set(c.hidden_tools || []);
      if (S.hiddenTools.has("music")) S.mu.open = false;
      if (S.hiddenTools.has("grab")) S.gr.open = false;
      if (S.hiddenTools.has("claude")) S.cl.open = false;
      S.lyricsCompact = !!c.lyrics_compact;
      if (!S.showTimer) S.picking = false;
      S.spOn = !!c.spotify;
      if (!S.spOn) S.sp = null;
    }
  } catch {}
  refreshSpotify();
  render();
}

/* ============================================================
   Minuteur / horloges
   ============================================================ */
const timerLeft = () => (S.timer ? Math.max(0, Math.ceil((S.timer.end - Date.now()) / 1000)) : 0);

setInterval(() => {
  if (S.timer && timerLeft() <= 0) {
    const total = S.timer.total;
    S.timer = null;
    chime();
    pushTransient("alert", { app: "Minuteur", title: "Minuteur terminé", body: `${Math.round(total / 60)} min écoulée${total >= 120 ? "s" : ""}`, svg: `<div style="width:26px;color:var(--orange)">${I.timer}</div>` }, 9000, true);
    return render();
  }
  if (S.timer && (S.view === "compact" || S.view === "expanded")) {
    const l = $("c-timer"); if (l) l.textContent = fmtTimer(timerLeft());
    const r = $("b-ring"); if (r && bubble.classList.contains("show")) setRing(r, timerLeft() / S.timer.total);
    const chip = $("x-timer-chip"); if (chip && S.view === "expanded") chip.innerHTML = `${I.timer}<span>${fmtTimer(timerLeft())}</span>`;
  }
  // la musique en pause finit par disparaître du compact
  if (S.view === "compact" || S.view === "idle") {
    const sig = activities().join();
    if (sig !== S._acts) { S._acts = sig; render(); }
  }
  if (S.view === "expanded") updateClock();
  if (S.view === "idle") {
    const it = $("i-time");
    if (it && it.textContent !== idleInfo().time) render();
  }
  if (S.game.running) {
    const gt = $("c-gtime"); if (gt && S.view === "compact") gt.textContent = playTime(S.game.running.since);
    if (S.view === "expanded" && !hasMedia() && !S.lol) renderGameCard();
  }
  // le coach alterne entre ses conseils toutes les 6 s
  if (S.lol && ++S._lolTick % 6 === 0) {
    S.lolRot++;
    if (S.view === "compact" && activities()[0] === "game") render();
  }
}, 1000);

setInterval(() => {
  if (S.view === "expanded") { updateProgress(); updateLyric(); }
  else if (S.view === "compact" && lyricsCompactOn() && activities()[0] === "media") {
    const mid = $("c-mid"), label = compactLabel();
    if (mid.textContent !== label) {
      if (lyricLine(0)) setKaraoke(mid, label);
      else mid.textContent = label;
      mid.classList.remove("lyr"); void mid.offsetWidth; mid.classList.add("lyr");
    }
    karaokeTick(mid);
  }
}, 200);

function chime() {
  try {
    const ctx = new AudioContext();
    [880, 1318.5, 1760].forEach((f, i) => {
      const o = ctx.createOscillator(), g = ctx.createGain();
      o.type = "sine";
      o.frequency.value = f;
      const t = ctx.currentTime + i * 0.16;
      g.gain.setValueAtTime(0, t);
      g.gain.linearRampToValueAtTime(0.18, t + 0.02);
      g.gain.exponentialRampToValueAtTime(0.001, t + 0.9);
      o.connect(g).connect(ctx.destination);
      o.start(t);
      o.stop(t + 1);
    });
  } catch {}
}

/* ============================================================
   Utilitaires
   ============================================================ */
function setHTML(el, html, key) {
  if (el.dataset.key === key) return;
  el.dataset.key = key;
  el.innerHTML = html;
}
function esc(s) {
  return String(s).replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));
}
function fmtTime(s) {
  s = Math.max(0, Math.floor(s));
  const h = Math.floor(s / 3600), m = Math.floor((s % 3600) / 60), sec = s % 60;
  return h ? `${h}:${String(m).padStart(2, "0")}:${String(sec).padStart(2, "0")}` : `${m}:${String(sec).padStart(2, "0")}`;
}
function fmtTimer(s) { return fmtTime(s); }
function batteryHTML(id) {
  return `<div class="battery" id="${id}"><div class="lvl"></div><svg class="bolt" viewBox="0 0 10 16"><path d="M6 0 0 9h4l-1 7 7-10H6l1-6z"/></svg></div>`;
}
function updBattery(el, p) {
  if (!el) return;
  const lvl = p.battery ?? 0;
  el.style.setProperty("--lvl", Math.max(4, lvl) + "%");
  el.classList.toggle("charging", !!p.charging);
  el.classList.toggle("low", lvl <= 20 && !p.charging);
}
function ringHTML(id, color) {
  const c = 2 * Math.PI * 9;
  return `<svg class="ring" id="${id}" viewBox="0 0 24 24"><circle class="bg" cx="12" cy="12" r="9"/><circle class="fg" cx="12" cy="12" r="9" stroke="${color}" stroke-dasharray="${c}" stroke-dashoffset="0"/></svg>`;
}
function setRing(svg, frac) {
  if (!svg) return;
  const c = 2 * Math.PI * 9;
  svg.querySelector(".fg").setAttribute("stroke-dashoffset", String(c * (1 - Math.max(0, Math.min(1, frac)))));
}
function gradientFor(name) {
  let h = 0;
  for (const ch of name) h = (h * 31 + ch.charCodeAt(0)) % 360;
  return `linear-gradient(145deg, hsl(${h} 75% 58%), hsl(${(h + 40) % 360} 70% 42%))`;
}

// Couleur dominante (saturée) d'une image → [teinte°, saturation%, luminosité 0-1]
function dominantHsl(src) {
  return new Promise((resolve) => {
    const img = new Image();
    img.onload = () => {
      try {
        // canvas neuf à chaque fois : un canvas réutilisé peut perdre son contexte (pilote graphique
        // réinitialisé pendant un jeu) et ne plus renvoyer que du noir
        const colorCanvas = document.createElement("canvas");
        colorCanvas.width = colorCanvas.height = 24;
        const g = colorCanvas.getContext("2d", { willReadFrequently: true });
        g.clearRect(0, 0, 24, 24);
        g.drawImage(img, 0, 0, 24, 24);
        const d = g.getImageData(0, 0, 24, 24).data;
        let r = 0, gg = 0, b = 0, ws = 0;
        for (let i = 0; i < d.length; i += 4) {
          if (d[i + 3] < 128) continue;
          const R = d[i], G = d[i + 1], B = d[i + 2];
          const mx = Math.max(R, G, B), mn = Math.min(R, G, B);
          const sat = mx === 0 ? 0 : (mx - mn) / mx;
          const lum = mx / 255;
          const w = sat * sat * (lum > 0.15 ? 1 : 0.1) + 0.002;
          r += R * w; gg += G * w; b += B * w; ws += w;
        }
        if (!ws) return resolve(null);
        const [h, s, l] = rgbToHsl(r / ws, gg / ws, b / ws);
        resolve([Math.round(h * 360), Math.round(Math.min(1, s) * 100), l]);
      } catch { resolve(null); }
    };
    img.onerror = () => resolve(null);
    img.src = src;
  });
}

// Couleur dominante de la pochette → égaliseur et teinte de l'island
function computeAccent(src) {
  if (S.accentPending === src) return;
  S.accentPending = src;
  dominantHsl(src).then((c) => {
    if (S.accentPending === src) S.accentPending = null;
    if (!c) return;
    S.accentArt = src;
    try {
      let [H, sat, l] = c;
      let s = sat / 100, h = H / 360;
      // fond de l'island : version sombre et saturée de la couleur de la pochette
      const root = document.documentElement.style;
      root.setProperty("--tint1", `hsl(${H} ${sat}% 42% / 0.62)`);
      root.setProperty("--tint2", `hsl(${H} ${Math.round(sat * 0.85)}% 17%)`);
      s = Math.max(s, 0.55);
      l = Math.min(Math.max(l, 0.58), 0.72);
      S.accent = `hsl(${Math.round(h * 360)} ${Math.round(s * 100)}% ${Math.round(l * 100)}%)`;
      root.setProperty("--accent", S.accent);
    } catch {}
  });
}
function rgbToHsl(r, g, b) {
  r /= 255; g /= 255; b /= 255;
  const mx = Math.max(r, g, b), mn = Math.min(r, g, b);
  let h = 0, s = 0;
  const l = (mx + mn) / 2;
  if (mx !== mn) {
    const d = mx - mn;
    s = l > 0.5 ? d / (2 - mx - mn) : d / (mx + mn);
    h = mx === r ? (g - b) / d + (g < b ? 6 : 0) : mx === g ? (b - r) / d + 2 : (r - g) / d + 4;
    h /= 6;
  }
  return [h, s, l];
}

/* ============================================================
   Démarrage
   ============================================================ */
// Icônes des boutons
document.querySelectorAll("[data-media]").forEach((b) => { b.innerHTML = I[b.dataset.media === "play_pause" ? "play" : b.dataset.media]; });
document.querySelectorAll("[data-tool]").forEach((b) => { b.innerHTML = b.dataset.tool === "grab" ? I.download : I[b.dataset.tool]; });
arrL.innerHTML = I.chevL;
arrR.innerHTML = I.chevR;
island.addEventListener("animationend", (e) => { if (e.animationName === "bump") island.classList.remove("bump"); });

applySize();
loadUiConfig();
listen("game", (e) => applyGame(e.payload));
S._lolTick = 0;
listen("lol", (e) => applyLol(e.payload));
invoke("lol_state").then(applyLol).catch(() => {});
invoke("game_state").then(applyGame).catch(() => {});
window.addEventListener("resize", reportRects);

// Mode démo dans un navigateur classique (sans Tauri) : ?demo
if (!T || location.search.includes("demo")) window.__demo = true;
