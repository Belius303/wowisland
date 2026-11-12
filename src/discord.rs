//! Salon vocal Discord : qui est là, qui parle, micro / casque coupés, quitter le salon.
//! Passe par l'appli Discord du PC (RPC local, tube nommé \\.\pipe\discord-ipc-N), donc ça marche
//! dans n'importe quel serveur, groupe ou appel privé. Il faut une appli développeur Discord
//! (Client ID + secret) : seul son propriétaire peut utiliser les droits vocaux, ce qui suffit ici.
//! Le secret et le jeton de renouvellement sont chiffrés DPAPI dans la config.

use crate::config;
use once_cell::sync::Lazy;
use parking_lot::Mutex;
use serde::Serialize;
use serde_json::{json, Value};
use std::fs::File;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter};

pub const REDIRECT_URI: &str = "http://localhost";

#[derive(Serialize, Clone, Default, PartialEq)]
pub struct Member {
    pub id: String,
    pub name: String,
    pub avatar: String,
    pub speaking: bool,
    pub mute: bool,
    pub deaf: bool,
}

#[derive(Serialize, Clone, Default, PartialEq)]
pub struct Channel {
    pub id: String,
    pub name: String,
    pub guild: String,
    /// 1 = appel privé, 3 = groupe, 2 = salon de serveur
    pub kind: u64,
}

#[derive(Serialize, Clone, Default, PartialEq)]
pub struct Voice {
    /// connecté à Discord et autorisé
    pub linked: bool,
    pub channel: Option<Channel>,
    pub members: Vec<Member>,
    pub mute: bool,
    pub deaf: bool,
    /// mon identifiant (pour me repérer dans la liste)
    pub me: String,
    pub error: Option<String>,
}

enum Cmd {
    ToggleMute,
    ToggleDeaf,
    Leave,
}

static STATE: Lazy<Mutex<Voice>> = Lazy::new(Default::default);
static CMDS: Lazy<Mutex<Vec<Cmd>>> = Lazy::new(Default::default);
/// L'utilisateur a cliqué « Connecter » dans les réglages : on demande l'autorisation à Discord.
static WANT_AUTH: AtomicBool = AtomicBool::new(false);
static AUTH_ERR: Lazy<Mutex<Option<String>>> = Lazy::new(Default::default);
/// Change quand les identifiants changent : la session en cours se termine.
static GEN: AtomicU32 = AtomicU32::new(0);
static APP: Lazy<Mutex<Option<AppHandle>>> = Lazy::new(Default::default);

pub fn state() -> Voice {
    STATE.lock().clone()
}

pub fn is_configured() -> bool {
    let c = config::get();
    !c.discord_client_id.is_empty() && !c.discord_secret_enc.is_empty()
}

pub fn is_linked() -> bool {
    !config::get().discord_refresh_enc.is_empty()
}

pub fn action(a: &str) -> Result<(), String> {
    if !STATE.lock().linked {
        return Err("Discord n'est pas connecté".into());
    }
    let cmd = match a {
        "mute" => Cmd::ToggleMute,
        "deaf" => Cmd::ToggleDeaf,
        "leave" => Cmd::Leave,
        _ => return Err("action inconnue".into()),
    };
    CMDS.lock().push(cmd);
    Ok(())
}

fn set_state(f: impl FnOnce(&mut Voice)) {
    let mut s = STATE.lock();
    let before = s.clone();
    f(&mut s);
    if *s != before {
        let snap = s.clone();
        drop(s);
        if let Some(app) = APP.lock().as_ref() {
            let _ = app.emit("discord", &snap);
        }
    }
}

/// Réglages : enregistre les identifiants et demande l'autorisation dans Discord (fenêtre « Autoriser »).
pub fn connect(client_id: &str, secret: &str) -> Result<(), String> {
    let client_id = client_id.trim();
    if client_id.len() < 15 || !client_id.chars().all(|c| c.is_ascii_digit()) {
        return Err("Client ID invalide (un long nombre, ex. 1234567890123456789)".into());
    }
    let secret = secret.trim();
    let secret_enc = if !secret.is_empty() {
        Some(config::protect(secret)?)
    } else if config::get().discord_secret_enc.is_empty() {
        return Err("Colle aussi le secret client (Client Secret)".into());
    } else {
        None
    };
    config::update(|c| {
        if let Some(e) = secret_enc {
            c.discord_secret_enc = e;
        }
        c.discord_client_id = client_id.to_string();
        c.discord_refresh_enc.clear();
    })?;
    *AUTH_ERR.lock() = None;
    GEN.fetch_add(1, Ordering::SeqCst);
    WANT_AUTH.store(true, Ordering::SeqCst);
    // on attend que l'utilisateur accepte dans Discord
    let t = Instant::now();
    while t.elapsed() < Duration::from_secs(120) {
        std::thread::sleep(Duration::from_millis(250));
        if let Some(e) = AUTH_ERR.lock().take() {
            WANT_AUTH.store(false, Ordering::SeqCst);
            return Err(e);
        }
        if STATE.lock().linked && !WANT_AUTH.load(Ordering::SeqCst) {
            return Ok(());
        }
    }
    WANT_AUTH.store(false, Ordering::SeqCst);
    Err("Pas de réponse : accepte la fenêtre « Autoriser » dans Discord".into())
}

pub fn disconnect() -> Result<(), String> {
    config::update(|c| c.discord_refresh_enc.clear())?;
    GEN.fetch_add(1, Ordering::SeqCst);
    set_state(|s| *s = Voice::default());
    Ok(())
}

pub fn start(app: AppHandle) {
    *APP.lock() = Some(app);
    std::thread::Builder::new()
        .name("discord".into())
        .spawn(|| loop {
            let c = config::get();
            let ready = !c.discord_client_id.is_empty() && !c.discord_secret_enc.is_empty();
            if !ready || (c.discord_refresh_enc.is_empty() && !WANT_AUTH.load(Ordering::SeqCst)) {
                set_state(|s| { s.linked = false; s.channel = None; s.members.clear(); });
                std::thread::sleep(Duration::from_millis(700));
                continue;
            }
            let Some(pipe) = open_pipe() else {
                // Discord fermé
                if WANT_AUTH.load(Ordering::SeqCst) {
                    *AUTH_ERR.lock() = Some("Discord n'est pas ouvert sur ce PC".into());
                }
                set_state(|s| { s.linked = false; s.channel = None; s.members.clear(); });
                std::thread::sleep(Duration::from_secs(4));
                continue;
            };
            let gen = GEN.load(Ordering::SeqCst);
            match Session::new(pipe).run(gen) {
                Ok(()) => set_state(|s| { s.linked = false; s.channel = None; s.members.clear(); }),
                Err(e) => {
                    crate::log(&format!("discord : {e}"));
                    if WANT_AUTH.load(Ordering::SeqCst) {
                        *AUTH_ERR.lock() = Some(e.clone());
                    }
                    set_state(|s| { s.linked = false; s.channel = None; s.members.clear(); s.error = Some(e); });
                }
            }
            std::thread::sleep(Duration::from_secs(3));
        })
        .ok();
}

fn open_pipe() -> Option<File> {
    for i in 0..10 {
        if let Ok(f) = std::fs::OpenOptions::new().read(true).write(true).open(format!(r"\\.\pipe\discord-ipc-{i}")) {
            return Some(f);
        }
    }
    None
}

struct Session {
    pipe: File,
    nonce: u64,
    channel_id: Option<String>,
}

impl Session {
    fn new(pipe: File) -> Self {
        Self { pipe, nonce: 0, channel_id: None }
    }

    fn write(&mut self, op: u32, v: &Value) -> Result<(), String> {
        let body = v.to_string();
        let mut buf = Vec::with_capacity(8 + body.len());
        buf.extend_from_slice(&op.to_le_bytes());
        buf.extend_from_slice(&(body.len() as u32).to_le_bytes());
        buf.extend_from_slice(body.as_bytes());
        self.pipe.write_all(&buf).map_err(|e| format!("Discord déconnecté ({e})"))
    }

    fn cmd(&mut self, cmd: &str, args: Value) -> Result<(), String> {
        self.nonce += 1;
        let n = self.nonce.to_string();
        self.write(1, &json!({ "cmd": cmd, "args": args, "nonce": n }))
    }

    fn sub(&mut self, evt: &str, args: Value) -> Result<(), String> {
        self.nonce += 1;
        let n = self.nonce.to_string();
        self.write(1, &json!({ "cmd": "SUBSCRIBE", "evt": evt, "args": args, "nonce": n }))
    }

    fn unsub(&mut self, evt: &str, args: Value) -> Result<(), String> {
        self.nonce += 1;
        let n = self.nonce.to_string();
        self.write(1, &json!({ "cmd": "UNSUBSCRIBE", "evt": evt, "args": args, "nonce": n }))
    }

    /// Octets disponibles dans le tube (lecture non bloquante : un ReadFile bloquant gèlerait aussi les écritures).
    fn available(&self) -> Result<u32, String> {
        use std::os::windows::io::AsRawHandle;
        use windows::Win32::Foundation::HANDLE;
        use windows::Win32::System::Pipes::PeekNamedPipe;
        let mut avail = 0u32;
        unsafe { PeekNamedPipe(HANDLE(self.pipe.as_raw_handle() as _), None, 0, None, Some(&mut avail), None) }
            .map_err(|_| "Discord fermé".to_string())?;
        Ok(avail)
    }

    /// Lit un message si disponible.
    fn poll(&mut self) -> Result<Option<(u32, Value)>, String> {
        if self.available()? < 8 {
            return Ok(None);
        }
        let mut h = [0u8; 8];
        self.pipe.read_exact(&mut h).map_err(|e| e.to_string())?;
        let op = u32::from_le_bytes(h[0..4].try_into().unwrap());
        let len = u32::from_le_bytes(h[4..8].try_into().unwrap()) as usize;
        if len > 8 << 20 {
            return Err("message Discord invalide".into());
        }
        let mut body = vec![0u8; len];
        self.pipe.read_exact(&mut body).map_err(|e| e.to_string())?;
        let v: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
        if op == 2 {
            let msg = v.get("message").and_then(|m| m.as_str()).unwrap_or("connexion fermée");
            return Err(format!("Discord : {msg}"));
        }
        if op == 3 {
            // ping → pong
            self.write(4, &v)?;
            return Ok(None);
        }
        Ok(Some((op, v)))
    }

    /// Attend une réponse à `cmd` (ou un événement `evt`) ; les autres messages sont traités au passage.
    fn wait(&mut self, cmd: Option<&str>, evt: Option<&str>, timeout: Duration) -> Result<Value, String> {
        let t = Instant::now();
        while t.elapsed() < timeout {
            match self.poll()? {
                Some((_, v)) => {
                    let c = v.get("cmd").and_then(|x| x.as_str());
                    let e = v.get("evt").and_then(|x| x.as_str());
                    if (cmd.is_some() && c == cmd) || (evt.is_some() && e == evt && c == Some("DISPATCH")) {
                        if e == Some("ERROR") {
                            let msg = v.pointer("/data/message").and_then(|m| m.as_str()).unwrap_or("erreur");
                            return Err(msg.to_string());
                        }
                        return Ok(v);
                    }
                    self.handle(&v)?;
                }
                None => std::thread::sleep(Duration::from_millis(15)),
            }
        }
        Err("Discord ne répond pas".into())
    }

    fn run(mut self, gen: u32) -> Result<(), String> {
        let c = config::get();
        let client_id = c.discord_client_id.clone();
        let secret = config::unprotect(&c.discord_secret_enc).map_err(|_| "Secret illisible : reconnecte Discord dans les réglages".to_string())?;

        self.write(0, &json!({ "v": 1, "client_id": client_id }))?;
        self.wait(None, Some("READY"), Duration::from_secs(8))?;

        // jeton d'accès
        let mut access = None;
        if !c.discord_refresh_enc.is_empty() {
            // jeton illisible (DPAPI) : on réessaie plus tard, sans jamais l'effacer
            let refresh = config::unprotect(&c.discord_refresh_enc).map_err(|e| format!("jeton illisible pour l'instant ({e}), nouvel essai bientôt"))?;
            match token(&client_id, &secret, &[("grant_type", "refresh_token"), ("refresh_token", &refresh)]) {
                Ok(t) => access = Some(t),
                // refus explicite de Discord (jeton révoqué) : là seulement on l'oublie
                Err(e) if e.starts_with("Discord a refusé") => {
                    crate::log(&format!("discord : renouvellement refusé ({e}), jeton oublié"));
                    let stale = c.discord_refresh_enc.clone();
                    // seulement s'il n'a pas été remplacé entre-temps
                    config::update(|c| if c.discord_refresh_enc == stale { c.discord_refresh_enc.clear() })?;
                    return Err("Connexion Discord expirée : reconnecte-toi dans les réglages".into());
                }
                Err(e) => return Err(format!("renouvellement impossible pour l'instant ({e})")),
            }
        }
        if access.is_none() {
            if !WANT_AUTH.load(Ordering::SeqCst) {
                return Ok(());
            }
            // fenêtre « Autoriser » dans Discord
            self.cmd("AUTHORIZE", json!({ "client_id": client_id, "scopes": ["rpc", "rpc.voice.read", "rpc.voice.write"] }))?;
            let r = self.wait(Some("AUTHORIZE"), None, Duration::from_secs(110)).map_err(|e| {
                if e.contains("cancel") || e.contains("denied") { "Autorisation refusée dans Discord".to_string() } else { e }
            })?;
            let code = r.pointer("/data/code").and_then(|x| x.as_str()).ok_or("Discord n'a pas renvoyé de code")?.to_string();
            access = Some(token(&client_id, &secret, &[("grant_type", "authorization_code"), ("code", &code), ("redirect_uri", REDIRECT_URI)])?);
        }
        let (access_token, refresh) = access.unwrap();
        if !refresh.is_empty() {
            let enc = config::protect(&refresh)?;
            config::update(|c| c.discord_refresh_enc = enc)?;
        }

        let r = self.wait_auth(&access_token)?;
        let me = r.pointer("/data/user/id").and_then(|x| x.as_str()).unwrap_or("").to_string();
        WANT_AUTH.store(false, Ordering::SeqCst);
        set_state(|s| { s.linked = true; s.error = None; s.me = me; });
        crate::log("discord : connecté");

        self.sub("VOICE_CHANNEL_SELECT", json!({}))?;
        self.sub("VOICE_SETTINGS_UPDATE", json!({}))?;
        self.cmd("GET_VOICE_SETTINGS", json!({}))?;
        self.cmd("GET_SELECTED_VOICE_CHANNEL", json!({}))?;

        loop {
            if GEN.load(Ordering::SeqCst) != gen {
                return Ok(());
            }
            let cmds: Vec<Cmd> = std::mem::take(&mut *CMDS.lock());
            for c in cmds {
                let (mute, deaf) = { let s = STATE.lock(); (s.mute, s.deaf) };
                match c {
                    Cmd::ToggleMute => self.cmd("SET_VOICE_SETTINGS", json!({ "mute": !mute }))?,
                    Cmd::ToggleDeaf => self.cmd("SET_VOICE_SETTINGS", json!({ "deaf": !deaf }))?,
                    Cmd::Leave => self.cmd("SELECT_VOICE_CHANNEL", json!({ "channel_id": null, "force": true }))?,
                }
            }
            let mut any = false;
            while let Some((_, v)) = self.poll()? {
                self.handle(&v)?;
                any = true;
            }
            if !any {
                std::thread::sleep(Duration::from_millis(30));
            }
        }
    }

    fn wait_auth(&mut self, access_token: &str) -> Result<Value, String> {
        self.cmd("AUTHENTICATE", json!({ "access_token": access_token }))?;
        self.wait(Some("AUTHENTICATE"), None, Duration::from_secs(10))
    }

    fn handle(&mut self, v: &Value) -> Result<(), String> {
        let cmd = v.get("cmd").and_then(|x| x.as_str()).unwrap_or("");
        let evt = v.get("evt").and_then(|x| x.as_str()).unwrap_or("");
        let data = v.get("data").cloned().unwrap_or(Value::Null);
        if evt == "ERROR" {
            return Ok(());
        }
        match (cmd, evt) {
            ("GET_VOICE_SETTINGS", _) | ("SET_VOICE_SETTINGS", _) | ("DISPATCH", "VOICE_SETTINGS_UPDATE") => {
                let mute = data.get("mute").and_then(|x| x.as_bool());
                let deaf = data.get("deaf").and_then(|x| x.as_bool());
                set_state(|s| {
                    if let Some(m) = mute { s.mute = m; }
                    if let Some(d) = deaf { s.deaf = d; }
                });
            }
            ("GET_SELECTED_VOICE_CHANNEL", _) | ("GET_CHANNEL", _) => self.set_channel(&data)?,
            ("DISPATCH", "VOICE_CHANNEL_SELECT") => {
                match data.get("channel_id").and_then(|x| x.as_str()) {
                    Some(id) => self.cmd("GET_CHANNEL", json!({ "channel_id": id }))?,
                    None => self.set_channel(&Value::Null)?,
                }
            }
            ("GET_GUILD", _) => {
                let name = data.get("name").and_then(|x| x.as_str()).unwrap_or("").to_string();
                set_state(|s| if let Some(c) = s.channel.as_mut() { c.guild = name; });
            }
            ("DISPATCH", "VOICE_STATE_CREATE") | ("DISPATCH", "VOICE_STATE_UPDATE") => {
                let m = member(&data);
                set_state(|s| match s.members.iter_mut().find(|x| x.id == m.id) {
                    Some(x) => { let sp = x.speaking; *x = m; x.speaking = sp; }
                    None => s.members.push(m),
                });
            }
            ("DISPATCH", "VOICE_STATE_DELETE") => {
                let id = data.pointer("/user/id").and_then(|x| x.as_str()).unwrap_or("").to_string();
                set_state(|s| s.members.retain(|x| x.id != id));
            }
            ("DISPATCH", "SPEAKING_START") | ("DISPATCH", "SPEAKING_STOP") => {
                let id = data.get("user_id").and_then(|x| x.as_str()).unwrap_or("").to_string();
                let on = evt == "SPEAKING_START";
                set_state(|s| if let Some(x) = s.members.iter_mut().find(|x| x.id == id) { x.speaking = on; });
            }
            _ => {}
        }
        Ok(())
    }

    fn set_channel(&mut self, data: &Value) -> Result<(), String> {
        let id = data.get("id").and_then(|x| x.as_str()).map(String::from);
        // désabonnement de l'ancien salon
        if let Some(old) = self.channel_id.take() {
            if Some(&old) != id.as_ref() {
                for e in ["VOICE_STATE_CREATE", "VOICE_STATE_UPDATE", "VOICE_STATE_DELETE", "SPEAKING_START", "SPEAKING_STOP"] {
                    let _ = self.unsub(e, json!({ "channel_id": old }));
                }
            } else {
                self.channel_id = Some(old);
            }
        }
        let Some(id) = id else {
            set_state(|s| { s.channel = None; s.members.clear(); });
            return Ok(());
        };
        let kind = data.get("type").and_then(|x| x.as_u64()).unwrap_or(2);
        let members: Vec<Member> = data.get("voice_states").and_then(|x| x.as_array()).map(|a| a.iter().map(member).collect()).unwrap_or_default();
        let mut name = data.get("name").and_then(|x| x.as_str()).unwrap_or("").to_string();
        if name.is_empty() {
            // appel privé / groupe sans nom : les autres participants
            let me = STATE.lock().me.clone();
            let others: Vec<&str> = members.iter().filter(|m| m.id != me).map(|m| m.name.as_str()).collect();
            name = if others.is_empty() { "Appel".into() } else { others.join(", ") };
        }
        let guild_id = data.get("guild_id").and_then(|x| x.as_str()).map(String::from);
        let kind_label = if kind == 1 { "Appel privé" } else if kind == 3 { "Groupe" } else { "" };
        set_state(|s| {
            let keep = s.channel.as_ref().map(|c| c.id == id).unwrap_or(false);
            let guild = if keep { s.channel.as_ref().map(|c| c.guild.clone()).unwrap_or_default() } else { kind_label.to_string() };
            s.channel = Some(Channel { id: id.clone(), name, guild, kind });
            // on garde « qui parle » pour les membres déjà connus
            let speaking: Vec<String> = s.members.iter().filter(|m| m.speaking).map(|m| m.id.clone()).collect();
            s.members = members;
            for m in s.members.iter_mut() {
                m.speaking = speaking.contains(&m.id);
            }
        });
        if self.channel_id.as_deref() != Some(id.as_str()) {
            for e in ["VOICE_STATE_CREATE", "VOICE_STATE_UPDATE", "VOICE_STATE_DELETE", "SPEAKING_START", "SPEAKING_STOP"] {
                self.sub(e, json!({ "channel_id": id }))?;
            }
            self.channel_id = Some(id);
            if let Some(g) = guild_id {
                self.cmd("GET_GUILD", json!({ "guild_id": g }))?;
            }
        }
        Ok(())
    }
}

fn member(v: &Value) -> Member {
    let u = v.get("user").cloned().unwrap_or(Value::Null);
    let id = u.get("id").and_then(|x| x.as_str()).unwrap_or("").to_string();
    let name = v.get("nick").and_then(|x| x.as_str()).filter(|s| !s.is_empty())
        .or_else(|| u.get("global_name").and_then(|x| x.as_str()))
        .or_else(|| u.get("username").and_then(|x| x.as_str()))
        .unwrap_or("?")
        .to_string();
    let avatar = match u.get("avatar").and_then(|x| x.as_str()) {
        Some(a) if !a.is_empty() => format!("https://cdn.discordapp.com/avatars/{id}/{a}.png?size=64"),
        _ => format!("https://cdn.discordapp.com/embed/avatars/{}.png", (id.parse::<u64>().unwrap_or(0) >> 22) % 6),
    };
    let vs = v.get("voice_state").cloned().unwrap_or(Value::Null);
    let b = |k: &str| vs.get(k).and_then(|x| x.as_bool()).unwrap_or(false);
    Member { id, name, avatar, speaking: false, mute: b("self_mute") || b("mute"), deaf: b("self_deaf") || b("deaf") }
}

/// Échange OAuth2 → (jeton d'accès, jeton de renouvellement).
fn token(client_id: &str, secret: &str, extra: &[(&str, &str)]) -> Result<(String, String), String> {
    let mut form: Vec<(&str, &str)> = vec![("client_id", client_id), ("client_secret", secret)];
    form.extend_from_slice(extra);
    let resp = crate::http().post("https://discord.com/api/oauth2/token").timeout(Duration::from_secs(10)).send_form(&form);
    let v: Value = match resp {
        Ok(r) => r.into_json().map_err(|e| e.to_string())?,
        Err(ureq::Error::Status(_, r)) => {
            let body: Value = r.into_json().unwrap_or(Value::Null);
            let err = body.get("error").and_then(|x| x.as_str()).unwrap_or("");
            let desc = body.get("error_description").and_then(|x| x.as_str()).unwrap_or(err);
            return Err(match err {
                "invalid_client" => "Client ID ou secret incorrect".to_string(),
                "invalid_grant" if desc.contains("redirect") => format!("Ajoute {REDIRECT_URI} dans OAuth2 → Redirects de ton appli Discord"),
                _ if desc.contains("redirect") => format!("Ajoute {REDIRECT_URI} dans OAuth2 → Redirects de ton appli Discord"),
                _ => format!("Discord a refusé ({desc})"),
            });
        }
        Err(e) => return Err(format!("Discord injoignable ({e})")),
    };
    let access = v.get("access_token").and_then(|x| x.as_str()).ok_or("Discord n'a pas renvoyé de jeton")?.to_string();
    let refresh = v.get("refresh_token").and_then(|x| x.as_str()).unwrap_or("").to_string();
    Ok((access, refresh))
}
