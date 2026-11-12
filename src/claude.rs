//! Mini-session Claude Code dans l'île : on lance la CLI `claude` installée sur le PC (le compte
//! Claude Code de l'utilisateur, aucune clé ici), en mode non interactif, et on relaie la réponse en
//! direct à l'interface. La conversation continue avec `--resume <session>`.
//! Mode « tout autorisé » (bypassPermissions) : Claude peut modifier des fichiers et lancer des commandes
//! sans demander, dans le dossier de travail choisi (Documents par défaut).
//!
//! Quotas : lus via l'API d'usage du compte Claude, avec le jeton que la CLI garde dans
//! ~/.claude/.credentials.json — lu en mémoire seulement, jamais affiché ni écrit ailleurs.

use once_cell::sync::Lazy;
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter};

const NO_WINDOW: u32 = 0x0800_0000;

#[derive(Default)]
struct St {
    session: Option<String>,
    pid: Option<u32>,
    model: Option<String>,
    stopped: bool,
    usage: Option<(Instant, Value)>,
}

static ST: Lazy<Mutex<St>> = Lazy::new(Default::default);

fn exe() -> Option<PathBuf> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    for dir in std::env::split_paths(&path) {
        for name in ["claude.exe", "claude.cmd"] {
            let p = dir.join(name);
            if p.is_file() {
                return Some(p);
            }
        }
    }
    let home = PathBuf::from(std::env::var("USERPROFILE").unwrap_or_default());
    [home.join(".local\\bin\\claude.exe"), home.join("AppData\\Roaming\\npm\\claude.cmd")].into_iter().find(|p| p.is_file())
}

/// Dossier où Claude travaille.
pub fn workdir() -> PathBuf {
    let c = crate::config::get().claude_dir;
    if !c.trim().is_empty() && std::path::Path::new(c.trim()).is_dir() {
        return PathBuf::from(c.trim());
    }
    let home = PathBuf::from(std::env::var("USERPROFILE").unwrap_or_default());
    let docs = home.join("Documents");
    if docs.is_dir() { docs } else { home }
}

/// Commande `claude` avec un environnement propre : si l'île a été lancée depuis une session
/// Claude Code, ses variables (proxy, mode « simple »…) empêcheraient la CLI d'utiliser le compte.
fn cmd(exe: &PathBuf) -> Command {
    let mut c = Command::new(exe);
    for (k, _) in std::env::vars() {
        if (k.starts_with("CLAUDE") && k != "CLAUDE_CONFIG_DIR") || k == "ANTHROPIC_BASE_URL" {
            c.env_remove(k);
        }
    }
    c.creation_flags(NO_WINDOW);
    c
}

fn send_ev(app: &AppHandle, v: Value) {
    let _ = app.emit("claude", v);
}

/// État pour l'interface : CLI trouvée, connectée, dossier, modèle, session en cours.
pub fn status() -> Value {
    let Some(exe) = exe() else {
        return json!({ "installed": false });
    };
    let mut logged = false;
    let mut plan = Value::Null;
    if let Ok(out) = cmd(&exe).args(["auth", "status", "--json"]).stdin(Stdio::null()).output() {
        if let Ok(v) = serde_json::from_slice::<Value>(&out.stdout) {
            logged = v.get("loggedIn").and_then(|x| x.as_bool()).unwrap_or(false);
            plan = v.get("subscriptionType").cloned().unwrap_or(Value::Null);
        }
    }
    let st = ST.lock();
    json!({
        "installed": true,
        "logged_in": logged,
        "plan": plan,
        "dir": workdir().to_string_lossy(),
        "model": st.model,
        "running": st.pid.is_some(),
        "has_session": st.session.is_some(),
    })
}

/// Ouvre une console avec `claude auth login` (connexion dans le navigateur, faite par l'utilisateur).
pub fn login() -> Result<(), String> {
    let exe = exe().ok_or("Claude Code n'est pas installé")?;
    let mut c = Command::new("cmd");
    for (k, _) in std::env::vars() {
        if (k.starts_with("CLAUDE") && k != "CLAUDE_CONFIG_DIR") || k == "ANTHROPIC_BASE_URL" {
            c.env_remove(k);
        }
    }
    c.args(["/c", "start", "Claude Code - connexion", "cmd", "/k"]).arg(&exe).args(["auth", "login"]);
    c.creation_flags(NO_WINDOW).spawn().map(|_| ()).map_err(|e| e.to_string())
}

pub fn reset() {
    ST.lock().session = None;
}

pub fn stop() {
    let pid = {
        let mut st = ST.lock();
        st.stopped = true;
        st.pid
    };
    if let Some(pid) = pid {
        // /T : aussi les commandes lancées par Claude
        let _ = Command::new("taskkill").args(["/T", "/F", "/PID", &pid.to_string()]).creation_flags(NO_WINDOW).output();
    }
}

pub fn send(app: &AppHandle, prompt: String) -> Result<(), String> {
    let prompt = prompt.trim().to_string();
    if prompt.is_empty() {
        return Err("Message vide".into());
    }
    let exe = exe().ok_or("Claude Code n'est pas installé sur ce PC")?;
    let session = {
        let st = ST.lock();
        if st.pid.is_some() {
            return Err("Claude répond déjà".into());
        }
        st.session.clone()
    };
    let mut c = cmd(&exe);
    c.args(["-p", "--output-format", "stream-json", "--verbose", "--include-partial-messages", "--permission-mode", "bypassPermissions"]);
    if let Some(s) = &session {
        c.args(["--resume", s]);
    }
    c.current_dir(workdir()).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = c.spawn().map_err(|e| format!("Impossible de lancer Claude Code : {e}"))?;
    {
        let mut st = ST.lock();
        st.pid = Some(child.id());
        st.stopped = false;
    }
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(prompt.as_bytes());
    }
    let app = app.clone();
    std::thread::spawn(move || {
        let started = Instant::now();
        send_ev(&app, json!({ "t": "start" }));
        let mut stderr = child.stderr.take();
        let err_thread = std::thread::spawn(move || {
            let mut s = String::new();
            if let Some(e) = stderr.as_mut() {
                let _ = e.read_to_string(&mut s);
            }
            s
        });
        let mut done = false;
        if let Some(out) = child.stdout.take() {
            for line in BufReader::new(out).lines() {
                let Ok(line) = line else { break };
                let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
                if handle(&app, &v) {
                    done = true;
                }
            }
        }
        let _ = child.wait();
        let err = err_thread.join().unwrap_or_default();
        let stopped = {
            let mut st = ST.lock();
            st.pid = None;
            std::mem::take(&mut st.stopped)
        };
        if !done {
            let msg = if stopped {
                "Arrêté".to_string()
            } else {
                err.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("Claude Code s'est arrêté").chars().take(200).collect()
            };
            send_ev(&app, json!({ "t": "done", "error": msg, "stopped": stopped, "ms": started.elapsed().as_millis() as u64 }));
        }
        // quotas après chaque réponse
        ST.lock().usage = None;
        if let Some(u) = usage() {
            send_ev(&app, json!({ "t": "usage", "usage": u }));
        }
    });
    Ok(())
}

/// Traite une ligne du flux ; renvoie true sur le résultat final.
fn handle(app: &AppHandle, v: &Value) -> bool {
    let t = v.get("type").and_then(|x| x.as_str()).unwrap_or("");
    match t {
        "system" if v.get("subtype").and_then(|x| x.as_str()) == Some("init") => {
            let model = v.get("model").and_then(|x| x.as_str()).map(String::from);
            {
                let mut st = ST.lock();
                if let Some(s) = v.get("session_id").and_then(|x| x.as_str()) {
                    st.session = Some(s.to_string());
                }
                if model.is_some() {
                    st.model = model.clone();
                }
            }
            send_ev(app, json!({ "t": "init", "model": model }));
        }
        "stream_event" => {
            let e = &v["event"];
            match e.get("type").and_then(|x| x.as_str()) {
                Some("content_block_start") if e["content_block"]["type"] == "text" && v["parent_tool_use_id"].is_null() => {
                    send_ev(app, json!({ "t": "block" }));
                }
                Some("content_block_delta") if e["delta"]["type"] == "text_delta" && v["parent_tool_use_id"].is_null() => {
                    if let Some(s) = e["delta"]["text"].as_str() {
                        send_ev(app, json!({ "t": "text", "s": s }));
                    }
                }
                _ => {}
            }
        }
        "assistant" => {
            if v.get("error").and_then(|x| x.as_str()) == Some("authentication_failed") {
                send_ev(app, json!({ "t": "login" }));
            }
            if let Some(m) = v["message"]["model"].as_str().filter(|m| m.starts_with("claude")) {
                let changed = ST.lock().model.as_deref() != Some(m);
                if changed {
                    ST.lock().model = Some(m.to_string());
                    send_ev(app, json!({ "t": "init", "model": m }));
                }
            }
            if !v["parent_tool_use_id"].is_null() {
                return false;
            }
            for c in v["message"]["content"].as_array().into_iter().flatten() {
                if c["type"] == "tool_use" {
                    let name = c["name"].as_str().unwrap_or("outil");
                    send_ev(app, json!({ "t": "tool", "name": name, "detail": tool_detail(&c["input"]) }));
                }
            }
        }
        "rate_limit_event" => {
            send_ev(app, json!({ "t": "rate", "info": v.get("rate_limit_info").cloned().unwrap_or(Value::Null) }));
        }
        "result" => {
            let is_err = v["is_error"].as_bool().unwrap_or(false);
            let text = v["result"].as_str().unwrap_or("");
            send_ev(
                app,
                json!({
                    "t": "done",
                    "error": if is_err { Value::String(text.chars().take(300).collect()) } else { Value::Null },
                    "login": is_err && text.contains("/login"),
                    "ms": v["duration_ms"].as_u64(),
                    "cost": v["total_cost_usd"].as_f64(),
                    "turns": v["num_turns"].as_u64(),
                }),
            );
            return true;
        }
        _ => {}
    }
    false
}

/// « npm run build », « src/main.rs »… : ce qu'on montre à côté du nom de l'outil.
fn tool_detail(input: &Value) -> String {
    for k in ["command", "file_path", "path", "pattern", "url", "query", "description", "prompt", "skill"] {
        if let Some(s) = input.get(k).and_then(|x| x.as_str()) {
            let s = s.lines().next().unwrap_or("").trim();
            let home = std::env::var("USERPROFILE").unwrap_or_default();
            let s = if !home.is_empty() { s.replace(&home, "~") } else { s.to_string() };
            return s.chars().take(90).collect();
        }
    }
    String::new()
}

/// Quotas du compte (fenêtre de 5 h, semaine…). Gardés 60 s.
pub fn usage() -> Option<Value> {
    if let Some((at, v)) = &ST.lock().usage {
        if at.elapsed() < Duration::from_secs(60) {
            return Some(v.clone());
        }
    }
    let dir = std::env::var("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(std::env::var("USERPROFILE").unwrap_or_default()).join(".claude"));
    let creds: Value = serde_json::from_str(&std::fs::read_to_string(dir.join(".credentials.json")).ok()?).ok()?;
    let oauth = creds.get("claudeAiOauth")?;
    let token = oauth.get("accessToken")?.as_str()?;
    let exp = oauth.get("expiresAt").and_then(|x| x.as_f64()).unwrap_or(0.0);
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok()?.as_millis() as f64;
    if exp > 0.0 && exp < now {
        return None; // la CLI le renouvelle au prochain message
    }
    let r: Value = crate::http()
        .get("https://api.anthropic.com/api/oauth/usage")
        .set("Authorization", &format!("Bearer {token}"))
        .set("anthropic-beta", "oauth-2025-04-20")
        .timeout(Duration::from_secs(8))
        .call()
        .map_err(|e| crate::log(&format!("claude : quotas indisponibles ({})", short_err(&e))))
        .ok()?
        .into_json()
        .ok()?;
    let mut out = vec![];
    if let Some(o) = r.as_object() {
        for (k, v) in o {
            let Some(u) = v.get("utilization").and_then(|x| x.as_f64()) else { continue };
            out.push(json!({ "key": k, "pct": u, "resets_at": v.get("resets_at").cloned().unwrap_or(Value::Null) }));
        }
    }
    let v = Value::Array(out);
    ST.lock().usage = Some((Instant::now(), v.clone()));
    Some(v)
}

fn short_err(e: &ureq::Error) -> String {
    match e {
        ureq::Error::Status(code, _) => format!("HTTP {code}"),
        ureq::Error::Transport(t) => t.kind().to_string(),
    }
}
