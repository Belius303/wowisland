//! Gmail en temps réel via IMAP IDLE (push : le serveur prévient dès qu'un mail arrive).
//! Authentification par « mot de passe d'application » Google (nécessite la validation en 2 étapes).

use crate::{config, otp};
use once_cell::sync::Lazy;
use regex::Regex;
use serde::Serialize;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tauri::{AppHandle, Emitter};

static GENERATION: AtomicU64 = AtomicU64::new(0);
static STATUS: Lazy<parking_lot::Mutex<std::collections::HashMap<String, GmailStatus>>> = Lazy::new(Default::default);

pub fn statuses() -> Vec<GmailStatus> {
    STATUS.lock().values().cloned().collect()
}

fn set_status(app: &AppHandle, account: &str, ok: bool, message: impl Into<String>) {
    let st = GmailStatus { account: account.into(), ok, message: message.into() };
    STATUS.lock().insert(account.into(), st.clone());
    let _ = app.emit("gmail-status", st);
}

#[derive(Serialize, Clone)]
pub struct MailEvent {
    pub account: String,
    pub from: String,
    pub subject: String,
    pub snippet: String,
}

#[derive(Serialize, Clone)]
pub struct GmailStatus {
    pub account: String,
    pub ok: bool,
    pub message: String,
}

/// (Re)démarre un thread IDLE par compte configuré.
pub fn restart(app: &AppHandle) {
    let gen = GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
    for acc in config::get().gmail {
        let app = app.clone();
        std::thread::Builder::new()
            .name(format!("gmail-{}", acc.email))
            .spawn(move || account_loop(app, acc, gen))
            .ok();
    }
}

fn alive(gen: u64) -> bool {
    GENERATION.load(Ordering::SeqCst) == gen
}

fn account_loop(app: AppHandle, acc: config::GmailAccount, gen: u64) {
    // Déchiffrement DPAPI : s'il échoue (ex. juste après l'ouverture de session), on réessaie au lieu d'abandonner.
    let mut tries = 0u32;
    let password = loop {
        match config::unprotect(&acc.password_enc) {
            Ok(p) => break p,
            Err(e) => {
                if tries == 0 {
                    crate::log(&format!("gmail {} : mot de passe illisible pour l'instant ({e}), nouvel essai", acc.email));
                }
                tries += 1;
                set_status(&app, &acc.email, false, "Mot de passe pas encore lisible, nouvel essai…");
                std::thread::sleep(Duration::from_secs(10));
                if !alive(gen) {
                    return;
                }
            }
        }
    };
    if tries > 0 {
        crate::log(&format!("gmail {} : mot de passe lu après {tries} essai(s)", acc.email));
    }
    let mut backoff = 5u64;
    let mut last_err = String::new();
    while alive(gen) {
        match session_loop(&app, &acc.email, &password, gen) {
            Ok(()) => return,
            Err(e) => {
                let auth = e.contains("AUTHENTICATIONFAILED") || e.contains("Invalid credentials");
                set_status(&app, &acc.email, false, e.clone());
                if e != last_err {
                    crate::log(&format!("gmail {} : {e}", acc.email));
                    last_err = e.clone();
                }
                if auth {
                    return; // inutile d'insister avec un mauvais mot de passe
                }
                std::thread::sleep(Duration::from_secs(backoff));
                backoff = (backoff * 2).min(300);
            }
        }
    }
}

fn connect(email: &str, password: &str) -> Result<imap::Session<native_tls::TlsStream<std::net::TcpStream>>, String> {
    let tls = native_tls::TlsConnector::builder().build().map_err(|e| e.to_string())?;
    let client = imap::connect(("imap.gmail.com", 993), "imap.gmail.com", &tls).map_err(|e| e.to_string())?;
    client.login(email, password).map_err(|(e, _)| e.to_string())
}

/// Vérifie des identifiants (bouton « Tester » des réglages).
pub fn test_login(email: &str, password: &str) -> Result<(), String> {
    let mut s = connect(email, password)?;
    s.select("INBOX").map_err(|e| e.to_string())?;
    let _ = s.logout();
    Ok(())
}

fn session_loop(app: &AppHandle, email: &str, password: &str, gen: u64) -> Result<(), String> {
    let mut session = connect(email, password)?;
    let mbox = session.select("INBOX").map_err(|e| e.to_string())?;
    let mut last_uid = mbox.uid_next.unwrap_or(1).saturating_sub(1);
    set_status(app, email, true, "Connecté · en écoute (IDLE)");

    while alive(gen) {
        // Nouveaux messages depuis le dernier UID vu
        let uids = session.uid_search(format!("UID {}:*", last_uid + 1)).map_err(|e| e.to_string())?;
        let mut uids: Vec<u32> = uids.into_iter().filter(|u| *u > last_uid).collect();
        uids.sort_unstable();
        for uid in uids {
            last_uid = last_uid.max(uid);
            let fetches = session.uid_fetch(uid.to_string(), "BODY.PEEK[]").map_err(|e| e.to_string())?;
            for f in fetches.iter() {
                if let Some(raw) = f.body() {
                    if alive(gen) {
                        process_mail(app, email, raw);
                    }
                }
            }
        }
        // Attente push (IDLE). Le push IDLE de Gmail arrive souvent avec 20-60 s de retard :
        // on coupe donc l'IDLE toutes les 4 s pour refaire une recherche (commande légère),
        // ce qui fait remonter les nouveaux mails bien plus vite. Le push reste actif entre deux.
        let idle = session.idle().map_err(|e| e.to_string())?;
        idle.wait_with_timeout(Duration::from_secs(4)).map_err(|e| e.to_string())?;
        session.noop().map_err(|e| e.to_string())?;
    }
    let _ = session.logout();
    Ok(())
}

fn process_mail(app: &AppHandle, account: &str, raw: &[u8]) {
    let Ok(parsed) = mailparse::parse_mail(raw) else { return };
    use mailparse::MailHeaderMap;
    let subject = parsed.headers.get_first_value("Subject").unwrap_or_default();
    let from_raw = parsed.headers.get_first_value("From").unwrap_or_default();
    let from = display_name(&from_raw);

    let mut plain = String::new();
    let mut html = String::new();
    collect_parts(&parsed, &mut plain, &mut html);
    let links = extract_links(&html);
    let text = if !plain.trim().is_empty() { plain } else { html_to_text(&html) };
    let text = squash_ws(&text);

    let found = otp::analyze(&subject, &text, &links);
    if let Some(found) = found {
        crate::alerts::found_secret(app, crate::alerts::Source::Gmail, &from, &subject, found);
        return;
    }
    if config::get().show_all_mails {
        let snippet: String = text.chars().take(160).collect();
        let _ = app.emit("mail", MailEvent { account: account.into(), from, subject, snippet });
    }
}

fn collect_parts(part: &mailparse::ParsedMail, plain: &mut String, html: &mut String) {
    if part.subparts.is_empty() {
        let ct = part.ctype.mimetype.to_lowercase();
        let is_attachment = part
            .get_content_disposition()
            .disposition == mailparse::DispositionType::Attachment;
        if is_attachment {
            return;
        }
        if ct == "text/plain" && plain.is_empty() {
            *plain = part.get_body().unwrap_or_default();
        } else if ct == "text/html" && html.is_empty() {
            *html = part.get_body().unwrap_or_default();
        }
    } else {
        for p in &part.subparts {
            collect_parts(p, plain, html);
        }
    }
}

fn display_name(from: &str) -> String {
    // "Google <no-reply@accounts.google.com>" → "Google"
    let name = from.split('<').next().unwrap_or(from).trim().trim_matches('"').trim();
    if name.is_empty() {
        from.trim_matches(['<', '>', ' ']).to_string()
    } else {
        name.to_string()
    }
}

static RE_STRIP_BLOCKS: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?is)<(style|script|head|title)[^>]*>.*?</(style|script|head|title)>").unwrap());
static RE_BREAKS: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?i)<br\s*/?>|</(p|div|tr|td|th|li|h[1-6]|table)>").unwrap());
static RE_TAGS: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?s)<[^>]+>").unwrap());
static RE_ANCHOR: Lazy<Regex> = Lazy::new(|| Regex::new(r#"(?is)<a\b[^>]*?href\s*=\s*["']([^"']+)["'][^>]*>(.*?)</a>"#).unwrap());
static RE_WS: Lazy<Regex> = Lazy::new(|| Regex::new(r"[ \t\u{a0}\u{200b}\u{200c}\u{34f}]+").unwrap());
static RE_NL: Lazy<Regex> = Lazy::new(|| Regex::new(r"\s*\n\s*(\n\s*)+").unwrap());

pub fn html_to_text(html: &str) -> String {
    let s = RE_STRIP_BLOCKS.replace_all(html, " ");
    let s = RE_BREAKS.replace_all(&s, "\n");
    let s = RE_TAGS.replace_all(&s, " ");
    decode_entities(&s)
}

fn extract_links(html: &str) -> Vec<(String, String)> {
    RE_ANCHOR
        .captures_iter(html)
        .filter_map(|c| {
            let url = decode_entities(c.get(1)?.as_str());
            if !url.starts_with("http") {
                return None;
            }
            let text = squash_ws(&decode_entities(&RE_TAGS.replace_all(c.get(2)?.as_str(), " ")));
            Some((text, url))
        })
        .collect()
}

fn squash_ws(s: &str) -> String {
    let s = RE_WS.replace_all(s, " ");
    RE_NL.replace_all(&s, "\n").trim().to_string()
}

fn decode_entities(s: &str) -> String {
    let mut out = s
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&eacute;", "é")
        .replace("&egrave;", "è")
        .replace("&agrave;", "à")
        .replace("&ccedil;", "ç")
        .replace("&ecirc;", "ê");
    static NUM: Lazy<Regex> = Lazy::new(|| Regex::new(r"&#(x?)([0-9a-fA-F]+);").unwrap());
    out = NUM
        .replace_all(&out, |c: &regex::Captures| {
            let radix = if &c[1] == "x" { 16 } else { 10 };
            u32::from_str_radix(&c[2], radix).ok().and_then(char::from_u32).map(String::from).unwrap_or_default()
        })
        .into_owned();
    out
}
