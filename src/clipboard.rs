//! Historique du presse-papiers : les dernières copies (texte et images), gardées en mémoire seulement.
//! Seuls les éléments épinglés sont sauvegardés (chiffrés DPAPI dans pins.dat) pour survivre au redémarrage. Les contenus marqués « privés » par les gestionnaires de mots de passe
//! sont ignorés, comme dans l'historique Windows (Win+V), ainsi que les codes copiés par l'île.

use base64::{engine::general_purpose::STANDARD as B64, Engine};
use once_cell::sync::Lazy;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tauri::{AppHandle, Emitter};

const MAX: usize = 15;

#[derive(Serialize, Clone)]
pub struct Clip {
    pub id: u32,
    /// "text" ou "image"
    pub kind: &'static str,
    pub text: String,
    /// aperçu (data URL) pour une image
    pub thumb: Option<String>,
    pub w: u32,
    pub h: u32,
    /// ms depuis 1970
    pub at: i64,
    /// épinglé : reste en haut, n'est pas effacé
    pub pinned: bool,
}

const MAX_PINS: usize = 12;

#[derive(Serialize, Deserialize)]
struct PinDisk {
    image: bool,
    text: String,
    png: Option<String>,
    w: u32,
    h: u32,
    at: i64,
}

struct Entry {
    clip: Clip,
    /// texte complet, ou image encodée en PNG
    full_text: String,
    png: Option<Vec<u8>>,
    hash: u64,
}

static HIST: Lazy<Mutex<Vec<Entry>>> = Lazy::new(|| Mutex::new(vec![]));
static SEQ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);
/// Texte copié par l'île elle-même (code de vérification…) : pas dans l'historique.
static IGNORE: Lazy<Mutex<Option<String>>> = Lazy::new(|| Mutex::new(None));

pub fn ignore_text(t: &str) {
    *IGNORE.lock() = Some(t.to_string());
}

pub fn list() -> Vec<Clip> {
    let h = HIST.lock();
    h.iter().filter(|e| e.clip.pinned).chain(h.iter().filter(|e| !e.clip.pinned)).map(|e| e.clip.clone()).collect()
}

/// « Tout effacer » garde les épingles.
pub fn clear(app: &AppHandle) {
    HIST.lock().retain(|e| e.clip.pinned);
    let _ = app.emit("clipboard", list());
}

pub fn remove(app: &AppHandle, id: u32) {
    let was_pinned = {
        let mut h = HIST.lock();
        let p = h.iter().any(|e| e.clip.id == id && e.clip.pinned);
        h.retain(|e| e.clip.id != id);
        p
    };
    if was_pinned {
        save_pins();
    }
    let _ = app.emit("clipboard", list());
}

pub fn pin(app: &AppHandle, id: u32, on: bool) -> Result<(), String> {
    {
        let mut h = HIST.lock();
        let count = h.iter().filter(|e| e.clip.pinned).count();
        let e = h.iter_mut().find(|e| e.clip.id == id).ok_or("Entrée introuvable")?;
        if on && !e.clip.pinned {
            if count >= MAX_PINS {
                return Err(format!("{MAX_PINS} épingles au maximum"));
            }
            if e.png.as_ref().map(|p| p.len() > 6 << 20).unwrap_or(false) {
                return Err("Image trop lourde pour être épinglée".into());
            }
        }
        e.clip.pinned = on;
    }
    save_pins();
    let _ = app.emit("clipboard", list());
    Ok(())
}

fn pins_path() -> std::path::PathBuf {
    crate::config::dir().join("pins.dat")
}

fn save_pins() {
    let pins: Vec<PinDisk> = HIST
        .lock()
        .iter()
        .filter(|e| e.clip.pinned)
        .map(|e| PinDisk {
            image: e.png.is_some(),
            text: e.full_text.clone(),
            png: e.png.as_ref().map(|p| B64.encode(p)),
            w: e.clip.w,
            h: e.clip.h,
            at: e.clip.at,
        })
        .collect();
    if pins.is_empty() {
        let _ = std::fs::remove_file(pins_path());
        return;
    }
    let Ok(json) = serde_json::to_string(&pins) else { return };
    match crate::config::protect(&json) {
        Ok(enc) => {
            let tmp = pins_path().with_extension("tmp");
            if std::fs::write(&tmp, enc).is_ok() {
                let _ = std::fs::rename(&tmp, pins_path());
            }
        }
        Err(e) => crate::log(&format!("presse-papiers : épingles non sauvegardées ({e})")),
    }
}

fn load_pins() {
    let Ok(enc) = std::fs::read_to_string(pins_path()) else { return };
    let Ok(json) = crate::config::unprotect(enc.trim()) else { return };
    let Ok(pins) = serde_json::from_str::<Vec<PinDisk>>(&json) else { return };
    let mut h = HIST.lock();
    for p in pins {
        let id = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if p.image {
            let Some(png) = p.png.and_then(|b| B64.decode(b).ok()) else { continue };
            let thumb = image::load_from_memory(&png).ok().and_then(|img| thumb_of(&img));
            h.push(Entry {
                clip: Clip { id, kind: "image", text: format!("Image {} × {}", p.w, p.h), thumb, w: p.w, h: p.h, at: p.at, pinned: true },
                full_text: String::new(),
                hash: hash_of(&png),
                png: Some(png),
            });
        } else {
            let preview: String = p.text.trim().chars().take(300).collect();
            h.push(Entry {
                clip: Clip { id, kind: "text", text: preview, thumb: None, w: 0, h: 0, at: p.at, pinned: true },
                hash: hash_of(p.text.as_bytes()),
                full_text: p.text,
                png: None,
            });
        }
    }
}

fn thumb_of(img: &image::DynamicImage) -> Option<String> {
    let t = img.thumbnail(320, 160).to_rgb8();
    let mut out = std::io::Cursor::new(Vec::new());
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 85).encode_image(&t).ok().map(|_| format!("data:image/jpeg;base64,{}", B64.encode(out.into_inner())))
}

/// Remet une entrée dans le presse-papiers (elle remonte en tête de liste).
pub fn restore(id: u32) -> Result<(), String> {
    let (text, png) = {
        let h = HIST.lock();
        let e = h.iter().find(|e| e.clip.id == id).ok_or("Entrée introuvable")?;
        (e.full_text.clone(), e.png.clone())
    };
    match png {
        Some(png) => {
            let img = image::load_from_memory(&png).map_err(|e| e.to_string())?.to_rgba8();
            crate::alerts::copy_image(None, img.width(), img.height(), img.as_raw(), Some(&png))
        }
        None => {
            for _ in 0..5 {
                if let Ok(mut cb) = arboard::Clipboard::new() {
                    if cb.set_text(text.clone()).is_ok() {
                        return Ok(());
                    }
                }
                std::thread::sleep(Duration::from_millis(60));
            }
            Err("Presse-papiers indisponible".into())
        }
    }
}

pub fn start(app: AppHandle) {
    load_pins();
    std::thread::Builder::new()
        .name("clipboard".into())
        .spawn(move || {
            use windows::Win32::System::DataExchange::GetClipboardSequenceNumber;
            let mut last = unsafe { GetClipboardSequenceNumber() };
            loop {
                std::thread::sleep(Duration::from_millis(400));
                let seq = unsafe { GetClipboardSequenceNumber() };
                if seq == last {
                    continue;
                }
                last = seq;
                // laisse l'appli source finir d'écrire tous ses formats
                std::thread::sleep(Duration::from_millis(120));
                if capture() {
                    let _ = app.emit("clipboard", list());
                }
            }
        })
        .ok();
}

fn hash_of(b: &[u8]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    b.hash(&mut h);
    h.finish()
}

/// Lit le presse-papiers et l'ajoute à l'historique. true si la liste a changé.
fn capture() -> bool {
    if is_private() {
        return false;
    }
    let Ok(mut cb) = arboard::Clipboard::new() else { return false };
    if let Ok(t) = cb.get_text() {
        if t.trim().is_empty() {
            return false;
        }
        if IGNORE.lock().as_deref() == Some(t.as_str()) {
            return false;
        }
        let full: String = t.chars().take(20_000).collect();
        let hash = hash_of(full.as_bytes());
        let preview: String = t.trim().chars().take(300).collect();
        return add(Entry {
            clip: Clip { id: 0, kind: "text", text: preview, thumb: None, w: 0, h: 0, at: 0, pinned: false },
            full_text: full,
            png: None,
            hash,
        });
    }
    if let Ok(img) = cb.get_image() {
        let (w, h) = (img.width as u32, img.height as u32);
        if w == 0 || h == 0 || (w as u64) * (h as u64) > 40_000_000 {
            return false;
        }
        let hash = hash_of(&img.bytes);
        // déjà en tête (ex. capture recopiée) : on ne ré-encode pas
        if HIST.lock().first().map(|e| e.hash == hash).unwrap_or(false) {
            return false;
        }
        let Some(rgba) = image::RgbaImage::from_raw(w, h, img.bytes.into_owned()) else { return false };
        let dynimg = image::DynamicImage::ImageRgba8(rgba);
        let mut png = std::io::Cursor::new(Vec::new());
        if dynimg.write_to(&mut png, image::ImageFormat::Png).is_err() {
            return false;
        }
        let thumb = thumb_of(&dynimg);
        return add(Entry {
            clip: Clip { id: 0, kind: "image", text: format!("Image {w} × {h}"), thumb, w, h, at: 0, pinned: false },
            full_text: String::new(),
            png: Some(png.into_inner()),
            hash,
        });
    }
    false
}

fn add(mut e: Entry) -> bool {
    let mut h = HIST.lock();
    // déjà présent : on le remonte en tête
    if let Some(i) = h.iter().position(|x| x.hash == e.hash) {
        let mut old = h.remove(i);
        old.clip.at = chrono::Utc::now().timestamp_millis();
        h.insert(0, old);
        return true;
    }
    e.clip.id = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    e.clip.at = chrono::Utc::now().timestamp_millis();
    h.insert(0, e);
    // épingles en plus des 15 dernières copies ; les images pèsent lourd : 6 au plus (hors épingles)
    let (mut n, mut imgs) = (0, 0);
    h.retain(|x| {
        if x.clip.pinned {
            return true;
        }
        n += 1;
        if x.png.is_some() {
            imgs += 1;
        }
        n <= MAX && (x.png.is_none() || imgs <= 6)
    });
    true
}

/// Contenu marqué comme privé (gestionnaires de mots de passe, KeePass, Bitwarden, 1Password…).
fn is_private() -> bool {
    use windows::core::w;
    use windows::Win32::System::DataExchange::*;
    use windows::Win32::System::Memory::{GlobalLock, GlobalUnlock};
    unsafe {
        let excl = RegisterClipboardFormatW(w!("ExcludeClipboardContentFromMonitorProcessing"));
        if excl != 0 && IsClipboardFormatAvailable(excl).is_ok() {
            return true;
        }
        let hist = RegisterClipboardFormatW(w!("CanIncludeInClipboardHistory"));
        if hist == 0 || IsClipboardFormatAvailable(hist).is_err() {
            return false;
        }
        // valeur DWORD : 0 = ne pas garder dans l'historique
        if OpenClipboard(None).is_err() {
            return true; // dans le doute, on ne garde pas
        }
        let mut private = true;
        if let Ok(h) = GetClipboardData(hist) {
            let g = windows::Win32::Foundation::HGLOBAL(h.0);
            let p = GlobalLock(g) as *const u32;
            if !p.is_null() {
                private = *p == 0;
                let _ = GlobalUnlock(g);
            }
        }
        let _ = CloseClipboard();
        private
    }
}
