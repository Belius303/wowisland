//! Configuration persistée dans %USERPROFILE%\.dynamicisland\config.json (avant : %APPDATA%\DynamicIsland).
//! Les mots de passe d'application Gmail sont chiffrés avec DPAPI (liés à ta session Windows).
//!
//! Pourquoi pas AppData : quand l'île est lancée par une appli « empaquetée » (MSIX, ex. l'appli Claude qui
//! compile et relance l'île), Windows redirige en silence toutes ses écritures dans AppData vers un dossier
//! privé de cette appli (%LOCALAPPDATA%\Packages\<appli>\LocalCache\Roaming\…). L'île lancée par Windows au
//! démarrage lisait le vrai AppData, sans les comptes enregistrés entre-temps : comptes « perdus » au
//! redémarrage. Le dossier personnel, lui, n'est jamais redirigé.

use base64::{engine::general_purpose::STANDARD as B64, Engine};
use once_cell::sync::Lazy;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Serialize, Deserialize, Default)]
pub struct GmailAccount {
    pub email: String,
    /// Mot de passe d'application chiffré DPAPI, encodé base64.
    #[serde(default)]
    pub password_enc: String,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub gmail: Vec<GmailAccount>,
    pub auto_copy_otp: bool,
    pub show_pc_notifications: bool,
    pub show_all_mails: bool,
    pub hide_in_fullscreen: bool,
    /// Noms d'applis dont on ignore les notifications (insensible à la casse).
    pub notif_blocklist: Vec<String>,
    /// Efface le presse-papiers 90 s après la copie d'un code (s'il n'a pas changé).
    pub clear_otp_clipboard: bool,
    /// Client ID de l'appli Spotify (développeur) de l'utilisateur, pour le volume Spotify Connect.
    pub spotify_client_id: String,
    /// Jeton de rafraîchissement Spotify chiffré DPAPI.
    pub spotify_refresh_enc: String,
    /// Bouton minuteur dans l'île (désactivé par défaut).
    pub show_timer: bool,
    /// Appli développeur Discord de l'utilisateur (salon vocal via le RPC local).
    pub discord_client_id: String,
    /// Secret client Discord chiffré DPAPI.
    pub discord_secret_enc: String,
    /// Jeton de rafraîchissement Discord chiffré DPAPI.
    pub discord_refresh_enc: String,
    /// Volume (touches) et luminosité affichés dans l'île.
    pub osd: bool,
    /// Paroles synchronisées à la place du titre dans la vue compacte.
    pub lyrics_compact: bool,
    /// Dossier de travail de la mini-session Claude Code (vide = Documents).
    pub claude_dir: String,
    /// Notifications Windows seulement dans l'île : bannières et centre de notifications décochés pour toutes les applis.
    pub notif_island_only: bool,
    /// Son de notification de Windows remplacé par le carillon de l'île.
    pub notif_sound: bool,
    /// Langue de l'interface : "fr", "en" ou vide (= langue de Windows).
    pub lang: String,
    /// boutons de l'île masqués (réglages)
    #[serde(default)]
    pub hidden_tools: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            gmail: vec![],
            auto_copy_otp: true,
            show_pc_notifications: true,
            show_all_mails: true,
            hide_in_fullscreen: true,
            notif_blocklist: vec![],
            clear_otp_clipboard: false,
            spotify_client_id: String::new(),
            spotify_refresh_enc: String::new(),
            show_timer: false,
            discord_client_id: String::new(),
            discord_secret_enc: String::new(),
            discord_refresh_enc: String::new(),
            osd: true,
            lyrics_compact: false,
            claude_dir: String::new(),
            notif_island_only: true,
            notif_sound: true,
            hidden_tools: vec![],
            lang: String::new(),
        }
    }
}

/// Langue effective de l'interface ("fr" ou "en") : le réglage, sinon la langue d'affichage de Windows.
pub fn lang() -> &'static str {
    match get().lang.as_str() {
        "fr" => "fr",
        "en" => "en",
        _ => {
            let id = unsafe { windows::Win32::Globalization::GetUserDefaultUILanguage() };
            if id & 0x3ff == 0x0c { "fr" } else { "en" }
        }
    }
}

pub static CONFIG: Lazy<RwLock<Config>> = Lazy::new(|| RwLock::new(load()));

/// Dossier des données de l'île (réglages, journal, historique, outils).
static ROOT: Lazy<PathBuf> = Lazy::new(|| {
    let d = home().join(".dynamicisland");
    let _ = std::fs::create_dir_all(&d);
    migrate(&d);
    d
});

pub fn dir() -> PathBuf {
    let d = ROOT.clone();
    let _ = std::fs::create_dir_all(&d);
    d
}

fn home() -> PathBuf {
    std::env::var_os("USERPROFILE").map(PathBuf::from).filter(|p| p.is_dir()).unwrap_or_else(|| PathBuf::from("."))
}

fn path() -> PathBuf {
    dir().join("config.json")
}

/// Copie de secours, réécrite après chaque sauvegarde réussie.
fn backup_path() -> PathBuf {
    dir().join("config.backup.json")
}

/// Seconde copie de secours, dans un sous-dossier.
fn backup2_path() -> PathBuf {
    let d = dir().join("secours");
    let _ = std::fs::create_dir_all(&d);
    d.join("config.json")
}

/* ---------------- reprise des anciens emplacements (AppData) ---------------- */

/// Anciens dossiers : le AppData que voit ce processus, et les copies privées des applis empaquetées
/// (%LOCALAPPDATA%\Packages\*\LocalCache\…\DynamicIsland), où ont pu atterrir les réglages.
fn legacy_dirs() -> Vec<PathBuf> {
    let mut v = vec![];
    if let Some(a) = std::env::var_os("APPDATA") {
        v.push(PathBuf::from(a).join("DynamicIsland"));
    }
    if let Some(l) = std::env::var_os("LOCALAPPDATA").map(PathBuf::from) {
        v.push(l.join("DynamicIsland"));
        for e in std::fs::read_dir(l.join("Packages")).into_iter().flatten().flatten() {
            for sub in ["Roaming", "Local"] {
                let d = e.path().join("LocalCache").join(sub).join("DynamicIsland");
                if d.is_dir() {
                    v.push(d);
                }
            }
        }
    }
    v.into_iter().filter(|d| d.is_dir()).collect()
}

/// Valeur d'une config : plus elle contient d'identifiants, mieux c'est.
fn score(c: &Config) -> usize {
    let (g, s, ds, dr) = has_secrets(c);
    g * 10 + s as usize * 3 + ds as usize * 3 + dr as usize + (!c.gmail.is_empty()) as usize + (!c.spotify_client_id.is_empty()) as usize
}

/// Premier lancement avec le nouveau dossier : on reprend la meilleure config trouvée (la plus complète,
/// puis la plus récente) et les autres fichiers. Les anciens dossiers ne sont pas touchés.
/// (Pas de crate::log ici : log() appelle dir(), qui est en train d'être initialisé.)
fn migrate(d: &std::path::Path) {
    if d.join("config.json").exists() {
        return;
    }
    let olds = legacy_dirs();
    // meilleure config : (score, date) le plus grand
    let mut best: Option<(usize, std::time::SystemTime, PathBuf)> = None;
    for o in &olds {
        for f in ["config.json", "config.backup.json"] {
            let p = o.join(f);
            let Ok(c) = read(&p) else { continue };
            let t = std::fs::metadata(&p).and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH);
            let s = score(&c);
            if best.as_ref().map(|(bs, bt, _)| (s, t) > (*bs, *bt)).unwrap_or(true) {
                best = Some((s, t, p));
            }
        }
    }
    let mut notes = vec![];
    // autres fichiers : d'abord ceux du dossier de la meilleure config, puis les autres (sans écraser)
    let mut order = olds.clone();
    if let Some((_, _, p)) = &best {
        if let Some(i) = order.iter().position(|o| Some(o.as_path()) == p.parent()) {
            let o = order.remove(i);
            order.insert(0, o);
        }
    }
    for o in &order {
        copy_missing(o, d);
    }
    if let Some((s, _, p)) = &best {
        // la config en dernier : c'est elle qui marque la reprise comme faite
        let _ = std::fs::copy(p, d.join("config.backup.json"));
        let tmp = d.join("config.json.tmp");
        if std::fs::copy(p, &tmp).is_ok() && std::fs::rename(&tmp, d.join("config.json")).is_ok() {
            notes.push(format!("config reprise de {} (score {s})", p.display()));
        }
    }
    notes.push(format!("anciens dossiers examinés : {}", olds.iter().map(|o| o.display().to_string()).collect::<Vec<_>>().join(" ; ")));
    let mut log = std::fs::read_to_string(d.join("log.txt")).unwrap_or_default();
    let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
    for n in notes {
        log.push_str(&format!("{now} nouveau dossier de données {} : {n}\n", d.display()));
    }
    let _ = std::fs::write(d.join("log.txt"), log);
}

/// Copie les fichiers (et sous-dossiers) absents de `to`. Les configs sont choisies à part.
fn copy_missing(from: &std::path::Path, to: &std::path::Path) {
    for e in std::fs::read_dir(from).into_iter().flatten().flatten() {
        let p = e.path();
        let name = e.file_name();
        let n = name.to_string_lossy();
        if n == "config.json" || n == "config.backup.json" || n.ends_with(".tmp") || n == "notification-ile.wav" {
            continue;
        }
        let dest = to.join(&name);
        if p.is_dir() {
            let _ = std::fs::create_dir_all(&dest);
            copy_missing(&p, &dest);
        } else if !dest.exists() {
            let _ = std::fs::copy(&p, &dest);
        }
    }
}

/// Les écritures de ce processus dans AppData sont-elles redirigées vers le dossier privé d'une appli
/// empaquetée (île lancée depuis l'appli Claude, par ex.) ? Test direct : un fichier sonde écrit dans AppData
/// réapparaît-il dans %LOCALAPPDATA%\Packages\<appli>\LocalCache\Roaming ? Renvoie le nom de l'appli.
/// (L'identité de paquet ne suffit pas : un processus lancé depuis le terminal de Claude n'en a pas, mais
/// ses écritures sont quand même redirigées.)
pub fn appdata_redirect() -> Option<String> {
    let appdata = PathBuf::from(std::env::var_os("APPDATA")?);
    let local = PathBuf::from(std::env::var_os("LOCALAPPDATA")?);
    let name = format!(".ile-sonde-{}", std::process::id());
    let probe = appdata.join(&name);
    let hit = if std::fs::write(&probe, b"x").is_ok() {
        let h = std::fs::read_dir(local.join("Packages"))
            .into_iter()
            .flatten()
            .flatten()
            .find(|e| e.path().join("LocalCache").join("Roaming").join(&name).exists())
            .map(|e| e.file_name().to_string_lossy().to_string());
        let _ = std::fs::remove_file(&probe);
        h
    } else {
        None
    };
    hit.or_else(package_family)
}

/// Identité de paquet MSIX de ce processus, s'il en a une.
pub fn package_family() -> Option<String> {
    #[link(name = "kernel32")]
    extern "system" {
        fn GetCurrentPackageFamilyName(len: *mut u32, name: *mut u16) -> i32;
    }
    let mut len = 0u32;
    // ERROR_INSUFFICIENT_BUFFER (122) = il y a un paquet ; APPMODEL_ERROR_NO_PACKAGE (15700) = aucun
    if unsafe { GetCurrentPackageFamilyName(&mut len, std::ptr::null_mut()) } != 122 || len == 0 {
        return None;
    }
    let mut buf = vec![0u16; len as usize];
    if unsafe { GetCurrentPackageFamilyName(&mut len, buf.as_mut_ptr()) } != 0 {
        return None;
    }
    Some(String::from_utf16_lossy(&buf[..(len as usize).saturating_sub(1)]))
}

/// La config n'a pas pu être lue au démarrage : on ne doit pas l'écraser par une config vide.
static LOAD_FAILED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn read(p: &std::path::Path) -> Result<Config, String> {
    let s = std::fs::read_to_string(p).map_err(|e| e.to_string())?;
    let s = s.trim_start_matches('\u{feff}');
    serde_json::from_str(s).map_err(|e| e.to_string())
}

fn load() -> Config {
    let main = path();
    if !main.exists() && !backup_path().exists() && !backup2_path().exists() {
        return Config::default(); // tout premier lancement
    }
    // Au démarrage de Windows, le fichier peut être brièvement verrouillé (antivirus, indexation…) :
    // on insiste quelques secondes au lieu de repartir sur une config vide.
    let mut err = String::new();
    for _ in 0..20 {
        match read(&main) {
            Ok(c) => return c,
            Err(e) => err = e,
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
    crate::log(&format!("config illisible ({err}), restauration depuis la copie de secours"));
    for b in [backup_path(), backup2_path()] {
        if let Ok(c) = read(&b) {
            return c;
        }
    }
    LOAD_FAILED.store(true, std::sync::atomic::Ordering::SeqCst);
    crate::log("config : aucune copie lisible, réglages par défaut (le fichier d'origine est conservé)");
    Config::default()
}

pub fn get() -> Config {
    CONFIG.read().clone()
}

/// Écrit d'abord un fichier temporaire puis le renomme : jamais de config.json à moitié écrite.
fn write_atomic(p: &std::path::Path, data: &str) -> Result<(), String> {
    use std::io::Write;
    let tmp = p.with_extension("json.tmp");
    {
        let mut f = std::fs::File::create(&tmp).map_err(|e| e.to_string())?;
        f.write_all(data.as_bytes()).map_err(|e| e.to_string())?;
        f.sync_all().map_err(|e| e.to_string())?;
    }
    std::fs::rename(&tmp, p).map_err(|e| e.to_string())
}

/// Un seul enregistrement à la fois : sans ce verrou, deux modules qui lisent puis réécrivent la config
/// en même temps (ex. Spotify et Discord qui renouvellent leur jeton) font perdre la modif de l'autre,
/// et un jeton déjà remplacé (donc invalide) peut revenir dans le fichier.
static SAVE_LOCK: Lazy<parking_lot::ReentrantMutex<()>> = Lazy::new(|| parking_lot::ReentrantMutex::new(()));

/// Lire-modifier-écrire la config de façon atomique (à utiliser partout au lieu de get() + save()).
pub fn update(f: impl FnOnce(&mut Config)) -> Result<(), String> {
    let _g = SAVE_LOCK.lock();
    let mut c = get();
    f(&mut c);
    save(c)
}

fn has_secrets(c: &Config) -> (usize, bool, bool, bool) {
    (
        c.gmail.iter().filter(|a| !a.password_enc.is_empty()).count(),
        !c.spotify_refresh_enc.is_empty(),
        !c.discord_secret_enc.is_empty(),
        !c.discord_refresh_enc.is_empty(),
    )
}

/// Si un enregistrement fait disparaître des identifiants, on garde l'ancien fichier de côté (5 derniers).
fn keep_if_secrets_dropped(main: &std::path::Path, new: &Config) {
    let Ok(old) = read(main) else { return };
    let (og, os, ods, odr) = has_secrets(&old);
    let (ng, ns, nds, ndr) = has_secrets(new);
    if ng >= og && (ns || !os) && (nds || !ods) && (ndr || !odr) {
        return;
    }
    let keep = dir().join(format!("config.avant-{}.json", chrono::Local::now().format("%Y%m%d-%H%M%S")));
    let _ = std::fs::copy(main, &keep);
    crate::log(&format!(
        "config : identifiants retirés (gmail {og}→{ng}, spotify {os}→{ns}, discord secret {ods}→{nds}, discord jeton {odr}→{ndr}), ancienne version gardée"
    ));
    let mut olds: Vec<_> = std::fs::read_dir(dir())
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.file_name().map(|n| n.to_string_lossy().starts_with("config.avant-")).unwrap_or(false))
        .collect();
    olds.sort();
    while olds.len() > 5 {
        let _ = std::fs::remove_file(olds.remove(0));
    }
}

/// Une ligne de diagnostic au démarrage (aucune valeur secrète, seulement présence / déchiffrement).
pub fn startup_report() {
    let c = get();
    let (g, s, ds, dr) = has_secrets(&c);
    let dec = |enc: &str| -> &'static str {
        if enc.is_empty() { "-" } else if unprotect(enc).is_ok() { "ok" } else { "ÉCHEC" }
    };
    let gmail_dec: Vec<&str> = c.gmail.iter().map(|a| dec(&a.password_enc)).collect();
    let dpapi = match protect("test").and_then(|e| unprotect(&e)) {
        Ok(v) if v == "test" => "ok".to_string(),
        Ok(_) => "incohérent".to_string(),
        Err(e) => format!("ÉCHEC ({e})"),
    };
    if let Some(pkg) = appdata_redirect() {
        crate::log(&format!(
            "ATTENTION : île lancée depuis l'appli empaquetée « {pkg} » : Windows redirige ses écritures dans AppData et le registre \
             vers les données privées de cette appli (réglages Windows, Spotifast…). Les données de l'île, elles, sont dans {} et ne sont pas touchées. \
             Relance l'île toi-même (menu Démarrer ou redémarrage) pour qu'elle tourne normalement.",
            dir().display()
        ));
    }
    crate::log(&format!(
        "démarrage : config {} ({}), gmail {} compte(s) {:?}, spotify {} [{}], discord secret {} [{}] jeton {} [{}], DPAPI {}",
        path().display(),
        if LOAD_FAILED.load(std::sync::atomic::Ordering::SeqCst) { "ILLISIBLE, défaut" } else { "lue" },
        g,
        gmail_dec,
        s,
        dec(&c.spotify_refresh_enc),
        ds,
        dec(&c.discord_secret_enc),
        dr,
        dec(&c.discord_refresh_enc),
        dpapi
    ));
}

pub fn save(cfg: Config) -> Result<(), String> {
    let _g = SAVE_LOCK.lock();
    let json = serde_json::to_string_pretty(&cfg).map_err(|e| e.to_string())?;
    let main = path();
    keep_if_secrets_dropped(&main, &cfg);
    // config illisible au démarrage : on met l'ancien fichier de côté avant de le remplacer
    if LOAD_FAILED.swap(false, std::sync::atomic::Ordering::SeqCst) && main.exists() {
        let keep = dir().join(format!("config.illisible-{}.json", chrono::Local::now().format("%Y%m%d-%H%M%S")));
        let _ = std::fs::copy(&main, keep);
    }
    write_atomic(&main, &json)?;
    for b in [backup_path(), backup2_path()] {
        let _ = write_atomic(&b, &json);
    }
    *CONFIG.write() = cfg;
    Ok(())
}

/// Crée les copies de secours si elles n'existent pas encore (config déjà enregistrée avant cette version).
pub fn ensure_backup() {
    let c = get();
    if c.gmail.is_empty() && c.spotify_client_id.is_empty() {
        return;
    }
    if let Ok(json) = serde_json::to_string_pretty(&c) {
        for b in [backup_path(), backup2_path()] {
            if !b.exists() {
                let _ = write_atomic(&b, &json);
            }
        }
    }
}

// ---------- DPAPI ----------

pub fn protect(plain: &str) -> Result<String, String> {
    use windows::Win32::Foundation::{LocalFree, HLOCAL};
    use windows::Win32::Security::Cryptography::{CryptProtectData, CRYPT_INTEGER_BLOB};
    let bytes = plain.as_bytes();
    let input = CRYPT_INTEGER_BLOB { cbData: bytes.len() as u32, pbData: bytes.as_ptr() as *mut u8 };
    let mut out = CRYPT_INTEGER_BLOB::default();
    unsafe {
        CryptProtectData(&input, None, None, None, None, 0, &mut out).map_err(|e| e.to_string())?;
        let data = std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec();
        let _ = LocalFree(Some(HLOCAL(out.pbData as _)));
        Ok(B64.encode(data))
    }
}

pub fn unprotect(enc: &str) -> Result<String, String> {
    use windows::Win32::Foundation::{LocalFree, HLOCAL};
    use windows::Win32::Security::Cryptography::{CryptUnprotectData, CRYPT_INTEGER_BLOB};
    let bytes = B64.decode(enc).map_err(|e| e.to_string())?;
    let input = CRYPT_INTEGER_BLOB { cbData: bytes.len() as u32, pbData: bytes.as_ptr() as *mut u8 };
    let mut out = CRYPT_INTEGER_BLOB::default();
    unsafe {
        CryptUnprotectData(&input, None, None, None, None, 0, &mut out).map_err(|e| e.to_string())?;
        let data = std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec();
        let _ = LocalFree(Some(HLOCAL(out.pbData as _)));
        String::from_utf8(data).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore]
    fn real_config_parses() {
        let s = std::fs::read_to_string(super::path()).unwrap();
        let c: super::Config = serde_json::from_str(&s).map_err(|e| e.to_string()).unwrap();
        println!("gmail {} comptes, spotify id {} car.", c.gmail.len(), c.spotify_client_id.len());
        for a in &c.gmail {
            assert!(super::unprotect(&a.password_enc).is_ok());
        }
    }
}

#[cfg(test)]
mod migration_tests {
    /// Reprise des anciens dossiers vers un dossier d'essai (n'affiche que des nombres, aucun secret).
    #[test]
    #[ignore]
    fn migrate_into_scratch() {
        println!("paquet de ce processus : {:?} · AppData redirigé vers : {:?}", super::package_family(), super::appdata_redirect());
        let d = std::env::temp_dir().join(format!("ile-migration-essai-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        for o in super::legacy_dirs() {
            for f in ["config.json", "config.backup.json"] {
                if let Ok(c) = super::read(&o.join(f)) {
                    println!("candidat {} : score {} {:?}", o.join(f).display(), super::score(&c), super::has_secrets(&c));
                }
            }
        }
        super::migrate(&d);
        let c = super::read(&d.join("config.json")).expect("config reprise");
        let (g, s, ds, dr) = super::has_secrets(&c);
        println!("reprise : gmail {g} (déchiffrables {}), spotify {s}, discord secret {ds}, jeton {dr}",
            c.gmail.iter().filter(|a| super::unprotect(&a.password_enc).is_ok()).count());
        let mut names: Vec<String> = std::fs::read_dir(&d).unwrap().flatten().map(|e| e.file_name().to_string_lossy().to_string()).collect();
        names.sort();
        println!("fichiers : {names:?}");
        println!("outils : {:?}", std::fs::read_dir(d.join("tools")).map(|r| r.flatten().map(|e| e.file_name().to_string_lossy().to_string()).collect::<Vec<_>>()).unwrap_or_default());
        let log = std::fs::read_to_string(d.join("log.txt")).unwrap();
        for l in log.lines().rev().take(2) {
            println!("journal : {l}");
        }
        std::fs::remove_dir_all(&d).unwrap();
        assert!(g >= 4);
    }
}

#[cfg(test)]
mod diag {
    /// Vérifie que les secrets enregistrés se déchiffrent (n'affiche que des longueurs).
    #[test]
    #[ignore]
    fn secrets_readable() {
        let c = super::read(&super::path()).unwrap();
        let chk = |name: &str, enc: &str| {
            if enc.is_empty() {
                println!("{name}: vide");
            } else {
                println!("{name}: {}", super::unprotect(enc).map(|s| format!("ok ({} car.)", s.len())).unwrap_or_else(|e| format!("ÉCHEC {e}")));
            }
        };
        chk("spotify_refresh", &c.spotify_refresh_enc);
        chk("discord_secret", &c.discord_secret_enc);
        chk("discord_refresh", &c.discord_refresh_enc);
        for (i, g) in c.gmail.iter().enumerate() {
            chk(&format!("gmail[{i}]"), &g.password_enc);
        }
    }
}
