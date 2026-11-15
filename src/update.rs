//! Mises à jour. Au premier lancement de la journée, l'île regarde la dernière version publiée dans les
//! Releases GitHub du projet. S'il y en a une plus récente, elle la propose avec ses nouveautés. Si on accepte :
//! téléchargement de WowIsland.exe, vérification du SHA-256 publié avec, remplacement de l'exe en place
//! (Windows permet de renommer un exe en cours d'exécution), puis relance sur la nouvelle version.
//!
//! Seul api.github.com / github.com est contacté, sans rien envoyer d'autre que la requête elle-même.

use once_cell::sync::Lazy;
use parking_lot::Mutex;
use serde::Serialize;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tauri::{AppHandle, Emitter};

const REPO: &str = "Belius303/wowisland";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Clone, Serialize, Debug, PartialEq)]
pub struct Note {
    /// titre en gras de la puce (peut être vide)
    pub t: String,
    /// le reste du texte
    pub d: String,
}

#[derive(Clone, Serialize)]
pub struct Update {
    pub version: String,
    pub notes: Vec<Note>,
    /// nombre de nouveautés en plus de celles affichées
    pub more: usize,
    /// page de la version sur GitHub
    pub page: String,
    pub size: u64,
    #[serde(skip)]
    exe_url: String,
    #[serde(skip)]
    sha_url: String,
}

static FOUND: Lazy<Mutex<Option<Update>>> = Lazy::new(|| Mutex::new(None));
/// version d'avant, quand on vient d'être relancé par une mise à jour
static UPDATED_FROM: Lazy<Mutex<Option<String>>> = Lazy::new(|| Mutex::new(None));
static BUSY: AtomicBool = AtomicBool::new(false);

/// Au démarrage : ménage après une mise à jour, puis vérification (une fois par jour).
pub fn start(app: AppHandle) {
    let args: Vec<String> = std::env::args().collect();
    if let Some(i) = args.iter().position(|a| a == "--updated") {
        *UPDATED_FROM.lock() = Some(args.get(i + 1).cloned().unwrap_or_default());
    }
    std::thread::spawn(move || {
        // laisse l'île finir de démarrer (et l'ancienne instance se fermer)
        std::thread::sleep(Duration::from_secs(6));
        cleanup();
        if dev_build() {
            return;
        }
        let c = crate::config::get();
        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        if !c.update_check || c.update_last == today {
            return;
        }
        // juste après l'ouverture de session, le réseau n'est pas toujours prêt
        for attempt in 1..=3u64 {
            match check() {
                Ok(found) => {
                    let _ = crate::config::update(|c| c.update_last = today.clone());
                    if let Some(u) = found {
                        crate::log(&format!("mise à jour : {} disponible (actuelle {VERSION})", u.version));
                        let _ = app.emit("update", &u);
                    }
                    return;
                }
                Err(e) => {
                    crate::log(&format!("mise à jour : vérification impossible ({e})"));
                    std::thread::sleep(Duration::from_secs(40 * attempt));
                }
            }
        }
    });
}

/// L'île tourne-t-elle depuis un dossier de compilation (cargo) ? Alors elle est mise à jour par le développeur,
/// pas par GitHub.
fn dev_build() -> bool {
    std::env::current_exe().ok().map(|exe| exe.ancestors().skip(1).any(|d| d.join("Cargo.toml").is_file())).unwrap_or(false)
}

fn paths() -> Option<(PathBuf, PathBuf, PathBuf)> {
    let exe = std::env::current_exe().ok()?;
    let name = exe.file_name()?.to_string_lossy().to_string();
    Some((exe.with_file_name(format!("{name}.new")), exe.with_file_name(format!("{name}.old")), exe))
}

/// Supprime l'ancien exe gardé lors de la dernière mise à jour (et un téléchargement interrompu).
fn cleanup() {
    if let Some((new, old, _)) = paths() {
        for p in [new, old] {
            if p.exists() {
                match std::fs::remove_file(&p) {
                    Ok(()) => crate::log(&format!("mise à jour : {} supprimé", p.display())),
                    Err(e) => crate::log(&format!("mise à jour : {} non supprimé ({e})", p.display())),
                }
            }
        }
    }
}

/// "v0.1.12" → [0, 1, 12]
fn parse_version(s: &str) -> Vec<u64> {
    s.trim().trim_start_matches(['v', 'V']).split(['.', '-', '+']).take(3).map(|p| p.parse().unwrap_or(0)).collect()
}

fn newer(remote: &str, local: &str) -> bool {
    let (mut r, mut l) = (parse_version(remote), parse_version(local));
    r.resize(3, 0);
    l.resize(3, 0);
    r > l
}

/// Retire le markdown d'un morceau de texte (gras, italique, code, liens).
fn plain(s: &str) -> String {
    static LINK: Lazy<regex::Regex> = Lazy::new(|| regex::Regex::new(r"\[([^\]]*)\]\([^)]*\)").unwrap());
    let s = LINK.replace_all(s, "$1");
    s.replace("**", "").replace('`', "").replace('*', "").replace("__", "").trim().to_string()
}

/// Les nouveautés d'une version, en puces : celles avant la section « Install ».
/// Les notes peuvent avoir une version française cachée dans un commentaire `<!-- fr … -->`.
fn parse_notes(body: &str, fr: bool) -> Vec<Note> {
    static BOLD: Lazy<regex::Regex> = Lazy::new(|| regex::Regex::new(r"^(?:[^\p{L}\p{N}*]*\s)?\*\*(.+?)\*\*[\s:.\-–—]*(.*)$").unwrap());
    let mut text = body;
    if fr {
        if let Some(i) = body.find("<!-- fr") {
            let rest = &body[i + 7..];
            if let Some(j) = rest.find("-->") {
                text = &rest[..j];
            }
        }
    } else if let Some(i) = body.find("<!-- fr") {
        // en anglais, on ignore le bloc français
        text = &body[..i];
    }
    let mut out = vec![];
    for line in text.lines() {
        let l = line.trim();
        if l.starts_with('#') {
            let h = l.to_lowercase();
            if h.contains("install") || h.contains("sha-256") || h.contains("changelog") {
                break;
            }
            continue;
        }
        let Some(item) = l.strip_prefix("- ").or_else(|| l.strip_prefix("* ")) else { continue };
        let note = match BOLD.captures(item) {
            Some(c) => Note { t: plain(&c[1]), d: plain(&c[2]) },
            None => Note { t: String::new(), d: plain(item) },
        };
        if !note.t.is_empty() || !note.d.is_empty() {
            out.push(note);
        }
    }
    out
}

/// Interroge GitHub. `Some` si une version plus récente que celle-ci est publiée (et téléchargeable).
pub fn check() -> Result<Option<Update>, String> {
    let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
    let r: serde_json::Value = crate::http()
        .get(&url)
        .set("Accept", "application/vnd.github+json")
        .timeout(Duration::from_secs(15))
        .call()
        .map_err(|e| e.to_string())?
        .into_json()
        .map_err(|e| e.to_string())?;
    let tag = r["tag_name"].as_str().unwrap_or_default().to_string();
    if tag.is_empty() || r["draft"].as_bool() == Some(true) || r["prerelease"].as_bool() == Some(true) || !newer(&tag, VERSION) {
        *FOUND.lock() = None;
        return Ok(None);
    }
    let prefix = format!("https://github.com/{REPO}/releases/download/");
    let asset = |name: &str| {
        r["assets"].as_array().into_iter().flatten().find(|a| a["name"].as_str() == Some(name)).and_then(|a| {
            let u = a["browser_download_url"].as_str()?;
            u.starts_with(&prefix).then(|| (u.to_string(), a["size"].as_u64().unwrap_or(0)))
        })
    };
    let (Some((exe_url, size)), Some((sha_url, _))) = (asset("WowIsland.exe"), asset("WowIsland.exe.sha256")) else {
        return Err(format!("{tag} : WowIsland.exe ou son SHA-256 manque dans la version"));
    };
    let mut notes = parse_notes(r["body"].as_str().unwrap_or_default(), crate::config::lang() == "fr");
    let more = notes.len().saturating_sub(4);
    notes.truncate(4);
    let u = Update {
        version: tag.trim_start_matches(['v', 'V']).to_string(),
        notes,
        more,
        page: r["html_url"].as_str().filter(|u| u.starts_with("https://github.com/")).unwrap_or(&format!("https://github.com/{REPO}/releases/latest")).to_string(),
        size,
        exe_url,
        sha_url,
    };
    *FOUND.lock() = Some(u.clone());
    Ok(Some(u))
}

/// Pour l'île au chargement : mise à jour en attente, et version d'avant si on vient d'être mis à jour.
pub fn pending() -> serde_json::Value {
    serde_json::json!({
        "version": VERSION,
        "update": FOUND.lock().clone(),
        "updated": UPDATED_FROM.lock().take(),
    })
}

fn progress(app: &AppHandle, state: &str, pct: u32, msg: &str) {
    let _ = app.emit("update-progress", serde_json::json!({ "state": state, "pct": pct, "msg": msg }));
}

/// Télécharge, vérifie et installe la mise à jour trouvée, puis relance l'île.
pub fn install(app: AppHandle) -> Result<(), String> {
    if FOUND.lock().is_none() {
        return Err("Aucune mise à jour à installer".into());
    }
    if BUSY.swap(true, Ordering::SeqCst) {
        return Ok(());
    }
    std::thread::spawn(move || {
        match download_and_swap(&app) {
            Ok(exe) => {
                crate::log("mise à jour : installée, relance");
                progress(&app, "restart", 100, "");
                std::thread::sleep(Duration::from_millis(900));
                use std::os::windows::process::CommandExt;
                match std::process::Command::new(&exe).args(["--wait", "--updated", VERSION]).creation_flags(0x0000_0008 /* DETACHED_PROCESS */).spawn() {
                    Ok(_) => app.exit(0),
                    Err(e) => {
                        crate::log(&format!("mise à jour : relance impossible ({e})"));
                        progress(&app, "error", 0, "Installée, mais la relance a échoué : relance WowIsland à la main.");
                    }
                }
            }
            Err(e) => {
                crate::log(&format!("mise à jour : échec ({e})"));
                progress(&app, "error", 0, &e);
                BUSY.store(false, Ordering::SeqCst);
            }
        }
    });
    Ok(())
}

fn download_and_swap(app: &AppHandle) -> Result<PathBuf, String> {
    let u = FOUND.lock().clone().ok_or("Aucune mise à jour à installer")?;
    let (new, old, exe) = paths().ok_or("Exe introuvable")?;
    let dir = exe.parent().map(|d| d.display().to_string()).unwrap_or_default();
    progress(app, "download", 0, "");

    // SHA-256 publié avec la version : « <hash>  WowIsland.exe »
    let sha = crate::http().get(&u.sha_url).timeout(Duration::from_secs(20)).call().map_err(|e| e.to_string())?.into_string().map_err(|e| e.to_string())?;
    let want = sha.split_whitespace().next().unwrap_or_default().to_lowercase();
    if want.len() != 64 || !want.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err("SHA-256 publié illisible".into());
    }

    let resp = crate::http().get(&u.exe_url).timeout(Duration::from_secs(600)).call().map_err(|e| e.to_string())?;
    let total = resp.header("Content-Length").and_then(|v| v.parse::<u64>().ok()).unwrap_or(u.size).max(1);
    let mut reader = resp.into_reader();
    let mut f = std::fs::File::create(&new).map_err(|e| format!("Impossible d'écrire dans {dir} ({e})"))?;
    let mut ctx = ring::digest::Context::new(&ring::digest::SHA256);
    let mut buf = vec![0u8; 64 * 1024];
    let (mut done, mut last, mut head) = (0u64, u32::MAX, [0u8; 2]);
    loop {
        let n = match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) => {
                drop(f);
                let _ = std::fs::remove_file(&new);
                return Err(format!("Téléchargement interrompu ({e})"));
            }
        };
        if done == 0 && n >= 2 {
            head = [buf[0], buf[1]];
        }
        if let Err(e) = f.write_all(&buf[..n]) {
            drop(f);
            let _ = std::fs::remove_file(&new);
            return Err(format!("Impossible d'écrire dans {dir} ({e})"));
        }
        ctx.update(&buf[..n]);
        done += n as u64;
        let pct = ((done * 100) / total).min(99) as u32;
        if pct != last {
            last = pct;
            progress(app, "download", pct, "");
        }
    }
    let _ = f.sync_all();
    drop(f);

    let got: String = ctx.finish().as_ref().iter().map(|b| format!("{b:02x}")).collect();
    if got != want || &head != b"MZ" || done < 1_000_000 {
        let _ = std::fs::remove_file(&new);
        return Err("Le fichier téléchargé ne correspond pas au SHA-256 publié : mise à jour annulée".into());
    }

    // L'exe en cours ne peut pas être écrasé, mais il peut être renommé : on le met de côté (supprimé au prochain
    // démarrage) et on met le nouveau à sa place, sous le même nom (le démarrage auto pointe toujours dessus).
    let _ = std::fs::remove_file(&old);
    std::fs::rename(&exe, &old).map_err(|e| {
        let _ = std::fs::remove_file(&new);
        format!("Impossible de remplacer l'exe dans {dir} ({e})")
    })?;
    if let Err(e) = std::fs::rename(&new, &exe) {
        let _ = std::fs::rename(&old, &exe);
        let _ = std::fs::remove_file(&new);
        return Err(format!("Impossible de remplacer l'exe dans {dir} ({e})"));
    }
    crate::log(&format!("mise à jour : {VERSION} → {} ({} octets, SHA-256 vérifié)", u.version, done));
    Ok(exe)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert!(newer("v0.1.2", "0.1.1"));
        assert!(newer("v0.2.0", "0.1.9"));
        assert!(newer("v0.1.10", "0.1.9"));
        assert!(!newer("v0.1.1", "0.1.1"));
        assert!(!newer("v0.1.0", "0.1.1"));
        assert!(!newer("garbage", "0.1.1"));
    }

    #[test]
    fn notes() {
        let body = "### Fixes\n- ⬇️ **YouTube \"not a bot\" check**: the downloader used to say *\"Private\"*. See [docs](https://x.y).\n- 🍪 New **Browser cookies** setting\n\n### Install\n- not a note\n<!-- fr\n### Corrections\n- ⬇️ **Vérification YouTube** : message plus clair.\n-->";
        let en = parse_notes(body, false);
        assert_eq!(en.len(), 2);
        assert_eq!(en[0], Note { t: "YouTube \"not a bot\" check".into(), d: "the downloader used to say \"Private\". See docs.".into() });
        assert_eq!(en[1].t, "");
        assert_eq!(en[1].d, "🍪 New Browser cookies setting");
        let fr = parse_notes(body, true);
        assert_eq!(fr, vec![Note { t: "Vérification YouTube".into(), d: "message plus clair.".into() }]);
        // sans bloc français : les notes anglaises
        assert_eq!(parse_notes("- **A**: b", true), vec![Note { t: "A".into(), d: "b".into() }]);
    }
}
