// Captures d'écran du README : lance Edge (ou Chrome) sans fenêtre, joue chaque scène de dev/demo.html
// et enregistre l'île en PNG (×2) dans docs/shots/<lang>/<scene>.png.
//   python -m http.server 8765      (à la racine du dépôt)
//   node dev/shots.mjs [en|fr] [scene…]
import { spawn } from "node:child_process";
import { mkdirSync, writeFileSync, existsSync, mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const lang = process.argv[2] || "en";
const all = ["lyrics", "player", "live", "otp", "notif", "discord", "home", "mixer", "download", "music", "lol", "lolcompact", "qr", "battery"];
const scenes = (process.argv.length > 3 ? process.argv.slice(3) : all).filter((s) => s !== "banner"); // « banner » seul : juste la bannière
const BASE = "http://localhost:8765/dev/demo.html";
const browsers = [
  "C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe",
  "C:/Program Files/Google/Chrome/Application/chrome.exe",
  "/usr/bin/chromium",
  "/usr/bin/google-chrome",
];
const exe = browsers.find((b) => existsSync(b));
const port = 9333;
const profile = mkdtempSync(join(tmpdir(), "island-shots-"));
const proc = spawn(exe, ["--headless=new", "--disable-gpu", "--hide-scrollbars", "--disable-renderer-backgrounding", "--disable-background-timer-throttling", "--disable-backgrounding-occluded-windows", `--remote-debugging-port=${port}`, `--user-data-dir=${profile}`, "--window-size=640,470", "about:blank"], { stdio: "ignore" });
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

async function target() {
  for (let i = 0; i < 50; i++) {
    try {
      const list = await (await fetch(`http://127.0.0.1:${port}/json`)).json();
      const page = list.find((t) => t.type === "page");
      if (page) return page.webSocketDebuggerUrl;
    } catch {}
    await sleep(200);
  }
  throw new Error("navigateur injoignable");
}

const ws = new WebSocket(await target());
await new Promise((r) => ws.addEventListener("open", r, { once: true }));
let seq = 0;
const pending = new Map();
ws.addEventListener("message", (e) => {
  const m = JSON.parse(e.data);
  if (m.id && pending.has(m.id)) { pending.get(m.id)(m); pending.delete(m.id); }
});
const cdp = (method, params = {}) => new Promise((res, rej) => {
  const id = ++seq;
  pending.set(id, (m) => (m.error ? rej(new Error(method + ": " + m.error.message)) : res(m.result)));
  ws.send(JSON.stringify({ id, method, params }));
});
const evaluate = async (expr) => (await cdp("Runtime.evaluate", { expression: expr, awaitPromise: true, returnByValue: true })).result.value;

await cdp("Page.enable");
await cdp("Emulation.setDeviceMetricsOverride", { width: 640, height: 470, deviceScaleFactor: 2, mobile: false });
const out = join("docs", "shots", lang);
mkdirSync(out, { recursive: true });
// premier chargement à froid : les animations y sont saccadées, on le jette
await cdp("Page.navigate", { url: `${BASE}?scene=home&lang=${lang}` });
await sleep(4000);
for (const s of scenes) {
  await cdp("Page.navigate", { url: `${BASE}?scene=${s}&lang=${lang}` });
  let ok = false;
  for (let i = 0; i < 80 && !ok; i++) { await sleep(150); ok = await evaluate("!!window.__sceneDone").catch(() => false); }
  // attendre que l'île ait fini de bouger (ressorts) : animation terminée et même taille sur 3 mesures.
  // Sans fenêtre, le navigateur peut cesser de produire des images (requestAnimationFrame figé) :
  // une mini-capture à chaque tour en force une.
  let last = "", same = 0;
  for (let i = 0; i < 60 && same < 3; i++) {
    await Promise.race([cdp("Page.captureScreenshot", { format: "jpeg", quality: 1, clip: { x: 0, y: 0, width: 1, height: 1, scale: 1 } }), sleep(400)]).catch(() => {});
    await sleep(200);
    const r = await evaluate("(() => { const r = document.getElementById('island').getBoundingClientRect(); const busy = (typeof animating !== 'undefined' && animating) || document.getAnimations().some((a) => a.playState === 'running' && a.effect && a.effect.getTiming().iterations !== Infinity); return (busy ? 'anim' : '') + [r.x, r.y, r.width, r.height].map(Math.round).join(','); })()");
    same = r === last && !r.startsWith("anim") ? same + 1 : 0;
    last = r;
  }
  for (let i = 0; i < 4; i++) { await Promise.race([cdp("Page.captureScreenshot", { format: "jpeg", quality: 1, clip: { x: 0, y: 0, width: 1, height: 1, scale: 1 } }), sleep(400)]).catch(() => {}); await sleep(150); }
  // cadre : toute la largeur, hauteur = bas de l'île (et de ses bulles) + marge
  const h = await evaluate(`(() => { let b = 0; for (const e of document.querySelectorAll('#frame *')) { const r = e.getBoundingClientRect(); const cs = getComputedStyle(e); if (r.width && r.height && cs.visibility !== 'hidden' && +cs.opacity > 0.05 && r.bottom < 470) b = Math.max(b, r.bottom); } return Math.ceil(b); })()`);
  const height = Math.min(470, Math.max(90, h + 28));
  const shot = await cdp("Page.captureScreenshot", { format: "png", clip: { x: 0, y: 0, width: 640, height, scale: 1 } });
  writeFileSync(join(out, `${s}.png`), Buffer.from(shot.data, "base64"));
  // la même île seule, sur fond transparent (pour la bannière)
  if (s === "player" || s === "lyrics") {
    await evaluate("for (const e of [document.documentElement, document.body]) e.style.setProperty('background', 'transparent', 'important')");
    await cdp("Emulation.setDefaultBackgroundColorOverride", { color: { r: 0, g: 0, b: 0, a: 0 } });
    const b = await evaluate(`(() => { let x0 = 1e9, y0 = 1e9, x1 = 0, y1 = 0; for (const e of document.querySelectorAll('#frame *')) { const r = e.getBoundingClientRect(); const cs = getComputedStyle(e); if (r.width && r.height && cs.visibility !== 'hidden' && +cs.opacity > 0.05 && r.bottom < 470) { x0 = Math.min(x0, r.left); y0 = Math.min(y0, r.top); x1 = Math.max(x1, r.right); y1 = Math.max(y1, r.bottom); } } return [x0, y0, x1, y1]; })()`);
    const p = 26, x = Math.max(0, b[0] - p), y = Math.max(0, b[1] - p);
    const crop = await cdp("Page.captureScreenshot", { format: "png", clip: { x, y, width: Math.min(640, b[2] + p) - x, height: Math.min(470, b[3] + p) - y, scale: 1 } });
    writeFileSync(join(out, `${s}-crop.png`), Buffer.from(crop.data, "base64"));
    await cdp("Emulation.setDefaultBackgroundColorOverride", {});
  }
  console.log(`${s}: ${ok ? "ok" : "délai dépassé"} (${height}px)`);
}
// bannière du README
await cdp("Emulation.setDeviceMetricsOverride", { width: 1200, height: 600, deviceScaleFactor: 2, mobile: false });
await cdp("Page.navigate", { url: `http://localhost:8765/docs/banner.html?lang=${lang}` });
for (let i = 0; i < 40 && !(await evaluate("!!window.__sceneDone").catch(() => false)); i++) await sleep(150);
await sleep(800);
const banner = await cdp("Page.captureScreenshot", { format: "png" });
writeFileSync(join("docs", `banner-${lang}.png`), Buffer.from(banner.data, "base64"));
console.log("bannière ok");
ws.close();
proc.kill();
