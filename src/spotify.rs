//! Volume interne de Spotify / Spotifast via l'API Spotify Connect (Web API officielle).
//! Connexion OAuth « PKCE » avec le Client ID de l'appli développeur de l'utilisateur
//! (la même que celle créée pour Spotifast) ; aucun secret client n'est nécessaire.
//! Le jeton de rafraîchissement est chiffré DPAPI dans la config.

use crate::config;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD as B64URL, Engine};
use once_cell::sync::Lazy;
use parking_lot::Mutex;
use serde::Serialize;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::sync::Arc;
use std::time::{Duration, Instant};

pub const REDIRECT_URI: &str = "http://127.0.0.1:8990/callback";
/// lecture / volume + Titres likés (bouton Spotify du téléchargeur) + playlists privées (liens à télécharger)
const SCOPES: &str = "user-read-playback-state user-modify-playback-state user-library-read user-library-modify playlist-read-private playlist-read-collaborative";

struct Token {
    access: String,
    expires: Instant,
    /// droits accordés (renvoyés avec chaque jeton)
    #[allow(dead_code)]
    scope: String,
}
static TOKEN: Lazy<Mutex<Option<Token>>> = Lazy::new(Default::default);
static AGENT: Lazy<ureq::Agent> = Lazy::new(|| {
    let mut b = ureq::AgentBuilder::new().timeout(Duration::from_secs(8));
    if let Ok(tls) = native_tls::TlsConnector::new() {
        b = b.tls_connector(Arc::new(tls));
    }
    b.build()
});

#[derive(Serialize, Clone, Default)]
pub struct State {
    pub connected: bool,
    /// volume 0-100 de l'appareil actif (None si rien ne joue / non réglable)
    pub volume: Option<u8>,
    pub device: String,
    pub supports_volume: bool,
    pub error: Option<String>,
    /// lecture aléatoire / répétition ("off", "context", "track") de l'appareil actif
    pub shuffle: Option<bool>,
    pub repeat: Option<String>,
}

pub fn is_connected() -> bool {
    !config::get().spotify_refresh_enc.is_empty()
}

pub(crate) fn enc(s: &str) -> String {
    let mut o = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => o.push(b as char),
            _ => o.push_str(&format!("%{b:02X}")),
        }
    }
    o
}

fn random_b64(n: usize) -> Result<String, String> {
    use ring::rand::SecureRandom;
    let mut buf = vec![0u8; n];
    ring::rand::SystemRandom::new().fill(&mut buf).map_err(|_| "aléa indisponible".to_string())?;
    Ok(B64URL.encode(buf))
}

/// Lance la connexion : ouvre la page Spotify dans le navigateur et attend le retour sur 127.0.0.1:8990.
pub fn connect(client_id: &str) -> Result<(), String> {
    let client_id = client_id.trim().to_string();
    if client_id.len() < 16 || !client_id.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Err("Client ID invalide (32 caractères, lettres et chiffres)".into());
    }
    let verifier = random_b64(48)?;
    let challenge = B64URL.encode(ring::digest::digest(&ring::digest::SHA256, verifier.as_bytes()));
    let state = random_b64(12)?;
    let listener = TcpListener::bind("127.0.0.1:8990").map_err(|e| format!("Port 8990 occupé : {e}"))?;
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;

    let url = format!(
        "https://accounts.spotify.com/authorize?client_id={}&response_type=code&redirect_uri={}&code_challenge_method=S256&code_challenge={}&scope={}&state={}",
        enc(&client_id),
        enc(REDIRECT_URI),
        challenge,
        enc(SCOPES),
        state
    );
    crate::shell_open(&url)?;

    // Attente du retour du navigateur (3 min max)
    let started = Instant::now();
    let code = loop {
        if started.elapsed() > Duration::from_secs(180) {
            return Err("Temps écoulé : la connexion Spotify n'a pas été validée".into());
        }
        match listener.accept() {
            Ok((mut stream, _)) => {
                let _ = stream.set_nonblocking(false);
                let mut line = String::new();
                let _ = BufReader::new(&stream).read_line(&mut line);
                let path = line.split_whitespace().nth(1).unwrap_or("").to_string();
                if !path.starts_with("/callback") {
                    let _ = stream.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n");
                    continue;
                }
                let q = path.split_once('?').map(|x| x.1).unwrap_or("");
                let get = |k: &str| q.split('&').find_map(|p| p.split_once('=').filter(|(a, _)| *a == k).map(|(_, v)| v.to_string()));
                let (ok, msg) = match (get("code"), get("state"), get("error")) {
                    (Some(c), Some(s), _) if s == state => (Some(c), "Spotify est connecté à WowIsland. Tu peux fermer cet onglet."),
                    (_, _, Some(_)) => (None, "Connexion refusée. Tu peux fermer cet onglet."),
                    _ => (None, "Réponse invalide. Réessaie depuis les réglages."),
                };
                let body = format!(
                    "<!doctype html><meta charset=utf-8><title>WowIsland</title><body style=\"font:16px Segoe UI;background:#111;color:#eee;display:grid;place-items:center;height:90vh\"><p>{msg}</p></body>"
                );
                let _ = stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes());
                match ok {
                    Some(c) => break c,
                    None => return Err(msg.into()),
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(150)),
            Err(e) => return Err(e.to_string()),
        }
    };

    let resp = AGENT
        .post("https://accounts.spotify.com/api/token")
        .send_form(&[
            ("grant_type", "authorization_code"),
            ("code", &code),
            ("redirect_uri", REDIRECT_URI),
            ("client_id", &client_id),
            ("code_verifier", &verifier),
        ])
        .map_err(http_err)?;
    let v: serde_json::Value = resp.into_json().map_err(|e| e.to_string())?;
    store_tokens(&client_id, &v)
}

fn store_tokens(client_id: &str, v: &serde_json::Value) -> Result<(), String> {
    let access = v["access_token"].as_str().ok_or("jeton manquant")?.to_string();
    let expires_in = v["expires_in"].as_u64().unwrap_or(3600);
    let scope = v["scope"].as_str().unwrap_or("").to_string();
    *TOKEN.lock() = Some(Token { access, expires: Instant::now() + Duration::from_secs(expires_in.saturating_sub(60)), scope });
    // Spotify remplace le jeton de rafraîchissement à chaque renouvellement : il faut l'enregistrer à coup sûr
    let enc = match v["refresh_token"].as_str() {
        Some(r) => Some(config::protect(r)?),
        None => None,
    };
    config::update(|c| {
        c.spotify_client_id = client_id.to_string();
        if let Some(e) = enc {
            c.spotify_refresh_enc = e;
        }
    })
}

pub fn disconnect() -> Result<(), String> {
    *TOKEN.lock() = None;
    config::update(|c| c.spotify_refresh_enc.clear())
}

fn http_err(e: ureq::Error) -> String {
    match e {
        ureq::Error::Status(code, r) => {
            let body = r.into_string().unwrap_or_default();
            let msg = serde_json::from_str::<serde_json::Value>(&body)
                .ok()
                .and_then(|v| v["error_description"].as_str().or(v["error"]["message"].as_str()).map(String::from))
                .unwrap_or(body);
            format!("Spotify {code} : {msg}")
        }
        other => other.to_string(),
    }
}

fn access_token() -> Result<String, String> {
    if let Some(t) = TOKEN.lock().as_ref() {
        if Instant::now() < t.expires {
            return Ok(t.access.clone());
        }
    }
    let c = config::get();
    if c.spotify_refresh_enc.is_empty() {
        return Err("Spotify non connecté".into());
    }
    let refresh = config::unprotect(&c.spotify_refresh_enc)?;
    let resp = AGENT
        .post("https://accounts.spotify.com/api/token")
        .send_form(&[("grant_type", "refresh_token"), ("refresh_token", &refresh), ("client_id", &c.spotify_client_id)])
        .map_err(|e| {
            let m = http_err(e);
            if m.contains("invalid_grant") || m.contains("revoked") {
                crate::log(&format!("spotify : jeton refusé ({m}), déconnecté"));
                // seulement si personne ne l'a remplacé entre-temps
                let stale = c.spotify_refresh_enc.clone();
                let _ = config::update(|c| if c.spotify_refresh_enc == stale { c.spotify_refresh_enc.clear() });
            }
            m
        })?;
    let v: serde_json::Value = resp.into_json().map_err(|e| e.to_string())?;
    store_tokens(&c.spotify_client_id, &v)?;
    Ok(TOKEN.lock().as_ref().map(|t| t.access.clone()).unwrap_or_default())
}

pub fn state() -> State {
    if !is_connected() {
        return State::default();
    }
    let run = || -> Result<State, String> {
        let token = access_token()?;
        let resp = AGENT.get("https://api.spotify.com/v1/me/player").set("Authorization", &format!("Bearer {token}")).call().map_err(http_err)?;
        if resp.status() == 204 {
            return Ok(State { connected: true, ..Default::default() });
        }
        let v: serde_json::Value = resp.into_json().map_err(|e| e.to_string())?;
        let d = &v["device"];
        Ok(State {
            connected: true,
            volume: d["volume_percent"].as_u64().map(|x| x.min(100) as u8),
            device: d["name"].as_str().unwrap_or("").to_string(),
            supports_volume: d["supports_volume"].as_bool().unwrap_or(true),
            error: None,
            shuffle: v["shuffle_state"].as_bool(),
            repeat: v["repeat_state"].as_str().map(String::from),
        })
    };
    run().unwrap_or_else(|e| State { connected: is_connected(), error: Some(e), ..Default::default() })
}

pub fn set_volume(pct: u8) -> Result<(), String> {
    let token = access_token()?;
    AGENT
        .put(&format!("https://api.spotify.com/v1/me/player/volume?volume_percent={}", pct.min(100)))
        .set("Authorization", &format!("Bearer {token}"))
        .send_bytes(&[])
        .map_err(http_err)?;
    Ok(())
}

/// Lecture aléatoire (on) / répétition ("off", "context", "track") de l'appareil Spotify actif.
pub fn set_mode(shuffle: Option<bool>, repeat: Option<&str>) -> Result<(), String> {
    let token = access_token()?;
    let url = match (shuffle, repeat) {
        (Some(s), _) => format!("https://api.spotify.com/v1/me/player/shuffle?state={s}"),
        (_, Some(r)) if matches!(r, "off" | "context" | "track") => format!("https://api.spotify.com/v1/me/player/repeat?state={r}"),
        _ => return Err("mode inconnu".into()),
    };
    AGENT.put(&url).set("Authorization", &format!("Bearer {token}")).send_bytes(&[]).map_err(http_err)?;
    Ok(())
}

/* ---------------- Web API (téléchargeur : liens Spotify) ---------------- */

/// Refait l'autorisation (nouveaux droits) avec le Client ID déjà enregistré.
pub fn reconnect() -> Result<(), String> {
    let id = config::get().spotify_client_id;
    if id.is_empty() {
        return Err("Spotify n'est pas configuré (Réglages → Spotify)".into());
    }
    *TOKEN.lock() = None;
    connect(&id)
}

pub fn api_get(path: &str) -> Result<serde_json::Value, String> {
    let token = access_token()?;
    let url = if path.starts_with("http") { path.to_string() } else { format!("https://api.spotify.com/v1{path}") };
    let resp = AGENT.get(&url).set("Authorization", &format!("Bearer {token}")).call().map_err(http_err)?;
    resp.into_json().map_err(|e| e.to_string())
}
