//! Notifications « seulement dans l'île » et son de notification plus doux.
//!
//! - Pour chaque appli connue de Windows (HKCU\…\Notifications\Settings\<appli>), on décoche
//!   « Afficher les bannières » et « Afficher dans le centre de notifications » (ShowBanner / ShowInActionCenter = 0).
//!   L'île lit quand même ces notifications (vérifié) et les affiche elle-même.
//!   Windows ne relit ces réglages qu'à l'ouverture de session : effet complet au prochain démarrage du PC.
//!   Les nouvelles applis sont prises en compte toutes les minutes.
//! - Le son « Notification » de Windows est remplacé par un carillon doux (fichier généré pour l'île).
//! Les valeurs d'origine sont sauvegardées (notif-backup.json) et remises quand on désactive l'option.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use windows::core::{HSTRING, PCWSTR};
use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::System::Registry::*;

const SETTINGS: &str = r"Software\Microsoft\Windows\CurrentVersion\Notifications\Settings";
const SOUND: &str = r"AppEvents\Schemes\Apps\.Default\Notification.Default\.Current";
static CHIME: &[u8] = include_bytes!("../assets/notif.wav");
static STARTED: AtomicBool = AtomicBool::new(false);

#[derive(Serialize, Deserialize, Default)]
#[serde(default)]
struct Backup {
    /// appli → (ShowBanner, ShowInActionCenter) d'origine (None = valeur absente, donc activé)
    apps: BTreeMap<String, (Option<u32>, Option<u32>)>,
    /// son d'origine (None = pas encore remplacé)
    sound: Option<String>,
}

fn backup_path() -> PathBuf {
    crate::config::dir().join("notif-backup.json")
}
fn load_backup() -> Backup {
    std::fs::read_to_string(backup_path()).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}
fn save_backup(b: &Backup) {
    if b.apps.is_empty() && b.sound.is_none() {
        let _ = std::fs::remove_file(backup_path());
    } else if let Ok(s) = serde_json::to_string_pretty(b) {
        let _ = std::fs::write(backup_path(), s);
    }
}

/* ---------------- registre (HKCU uniquement) ---------------- */

fn open(path: &str, write: bool) -> Option<HKEY> {
    let mut k = HKEY::default();
    let access = if write { KEY_READ | KEY_WRITE } else { KEY_READ };
    let r = unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, &HSTRING::from(path), Some(0), access, &mut k) };
    (r == ERROR_SUCCESS).then_some(k)
}
fn close(k: HKEY) {
    unsafe {
        let _ = RegCloseKey(k);
    }
}
fn subkeys(k: HKEY) -> Vec<String> {
    let mut v = vec![];
    for i in 0.. {
        let mut buf = [0u16; 512];
        let mut len = buf.len() as u32;
        let r = unsafe { RegEnumKeyExW(k, i, Some(windows::core::PWSTR(buf.as_mut_ptr())), &mut len, None, None, None, None) };
        if r != ERROR_SUCCESS {
            break;
        }
        v.push(String::from_utf16_lossy(&buf[..len as usize]));
    }
    v
}
fn get_dword(k: HKEY, name: &str) -> Option<u32> {
    let mut data = 0u32;
    let mut size = 4u32;
    let r = unsafe { RegGetValueW(k, None, &HSTRING::from(name), RRF_RT_REG_DWORD, None, Some(&mut data as *mut u32 as _), Some(&mut size)) };
    (r == ERROR_SUCCESS).then_some(data)
}
fn set_dword(k: HKEY, name: &str, v: u32) -> bool {
    unsafe { RegSetValueExW(k, &HSTRING::from(name), Some(0), REG_DWORD, Some(&v.to_le_bytes())) == ERROR_SUCCESS }
}
fn del_value(k: HKEY, name: &str) {
    unsafe {
        let _ = RegDeleteValueW(k, &HSTRING::from(name));
    }
}
fn get_default_str(k: HKEY) -> Option<String> {
    let mut buf = vec![0u16; 1024];
    let mut size = (buf.len() * 2) as u32;
    let r = unsafe { RegGetValueW(k, None, PCWSTR::null(), RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ | RRF_NOEXPAND, None, Some(buf.as_mut_ptr() as _), Some(&mut size)) };
    if r != ERROR_SUCCESS {
        return None;
    }
    let n = (size as usize / 2).saturating_sub(1);
    Some(String::from_utf16_lossy(&buf[..n]))
}
fn set_default_str(k: HKEY, s: &str, expand: bool) -> bool {
    let w: Vec<u16> = s.encode_utf16().chain(std::iter::once(0)).collect();
    let bytes = unsafe { std::slice::from_raw_parts(w.as_ptr() as *const u8, w.len() * 2) };
    unsafe { RegSetValueExW(k, PCWSTR::null(), Some(0), if expand { REG_EXPAND_SZ } else { REG_SZ }, Some(bytes)) == ERROR_SUCCESS }
}

/* ---------------- bannières ---------------- */

/// Décoche les 2 cases pour toutes les applis ; renvoie le nombre d'applis modifiées.
fn apply_apps(b: &mut Backup) -> usize {
    let Some(root) = open(SETTINGS, false) else { return 0 };
    let names = subkeys(root);
    close(root);
    let mut changed = 0;
    for name in names {
        let Some(k) = open(&format!("{SETTINGS}\\{name}"), true) else { continue };
        let (banner, center) = (get_dword(k, "ShowBanner"), get_dword(k, "ShowInActionCenter"));
        if banner != Some(0) || center != Some(0) {
            b.apps.entry(name.clone()).or_insert((banner, center));
            if set_dword(k, "ShowBanner", 0) & set_dword(k, "ShowInActionCenter", 0) {
                changed += 1;
            }
        }
        close(k);
    }
    changed
}

fn restore_apps(b: &mut Backup) -> usize {
    let mut n = 0;
    for (name, (banner, center)) in std::mem::take(&mut b.apps) {
        let Some(k) = open(&format!("{SETTINGS}\\{name}"), true) else { continue };
        for (v, field) in [(banner, "ShowBanner"), (center, "ShowInActionCenter")] {
            match v {
                Some(x) => {
                    set_dword(k, field, x);
                }
                None => del_value(k, field),
            }
        }
        close(k);
        n += 1;
    }
    n
}

/* ---------------- son ---------------- */

fn chime_path() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(crate::config::dir);
    let d = base.join("DynamicIsland");
    let _ = std::fs::create_dir_all(&d);
    d.join("notification-ile.wav")
}

fn apply_sound(b: &mut Backup) -> bool {
    let wav = chime_path();
    if std::fs::read(&wav).map(|d| d != CHIME).unwrap_or(true) && std::fs::write(&wav, CHIME).is_err() {
        return false;
    }
    let target = wav.to_string_lossy().to_string();
    let Some(k) = open(SOUND, true) else { return false };
    let cur = get_default_str(k).unwrap_or_default();
    let mut ok = true;
    if !cur.eq_ignore_ascii_case(&target) {
        b.sound.get_or_insert(cur);
        ok = set_default_str(k, &target, false);
    }
    close(k);
    ok
}

fn restore_sound(b: &mut Backup) {
    if let Some(old) = b.sound.take() {
        if let Some(k) = open(SOUND, true) {
            let old = if old.is_empty() { r"%SystemRoot%\media\Windows Notify System Generic.wav".to_string() } else { old };
            set_default_str(k, &old, old.contains('%'));
            close(k);
        }
    }
}

/* ---------------- API ---------------- */

/// Applique les réglages de la config (au démarrage, puis toutes les minutes pour les nouvelles applis).
pub fn start() {
    sync(true);
    if STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    std::thread::Builder::new()
        .name("winnotif".into())
        .spawn(|| loop {
            std::thread::sleep(Duration::from_secs(60));
            sync(false);
        })
        .ok();
}

/// Met le registre en accord avec les options (appelé aussi quand on change un réglage).
pub fn sync(verbose: bool) {
    let c = crate::config::get();
    let mut b = load_backup();
    let mut msg = vec![];
    if c.notif_island_only {
        let n = apply_apps(&mut b);
        if n > 0 || verbose {
            msg.push(format!("bannières et centre désactivés pour {n} appli(s) de plus ({} au total)", b.apps.len()));
        }
    } else if !b.apps.is_empty() {
        let n = restore_apps(&mut b);
        msg.push(format!("réglages d'origine remis pour {n} appli(s)"));
    }
    if c.notif_sound {
        let had = b.sound.is_some();
        if apply_sound(&mut b) && !had && b.sound.is_some() {
            msg.push("son de notification remplacé".into());
        }
    } else if b.sound.is_some() {
        restore_sound(&mut b);
        msg.push("son de notification d'origine remis".into());
    }
    save_backup(&b);
    if !msg.is_empty() {
        crate::log(&format!("notifications Windows : {}", msg.join(", ")));
    }
}

/// Fait entendre le carillon (aperçu dans les réglages).
pub fn preview() {
    use windows::Win32::Media::Audio::{PlaySoundW, SND_ASYNC, SND_FILENAME, SND_NODEFAULT};
    let p = chime_path();
    if !p.exists() {
        let _ = std::fs::write(&p, CHIME);
    }
    unsafe {
        let _ = PlaySoundW(&HSTRING::from(p.as_os_str()), None, SND_FILENAME | SND_ASYNC | SND_NODEFAULT);
    }
}
