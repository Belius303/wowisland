//! Téléchargements en cours (Zen / Firefox, et navigateurs Chromium) : on suit les fichiers temporaires
//! `.part` / `.crdownload` dans les dossiers de téléchargement. Le navigateur n'indique pas la taille totale
//! dans ces fichiers : on affiche la taille reçue et la vitesse.

use serde::Serialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};
use tauri::{AppHandle, Emitter};

#[derive(Serialize, Clone, PartialEq)]
pub struct Download {
    pub id: u32,
    pub name: String,
    /// octets reçus
    pub size: u64,
    /// octets / s (lissé)
    pub speed: u64,
}

#[derive(Serialize, Clone)]
pub struct Done {
    pub name: String,
    pub path: String,
    pub size: u64,
    pub thumb: Option<String>,
}

struct Track {
    id: u32,
    size: u64,
    speed: f64,
    last_change: Instant,
    seen: Instant,
}

const TEMP_EXT: [&str; 2] = [".part", ".crdownload"];

pub fn start(app: AppHandle) {
    std::thread::Builder::new()
        .name("downloads".into())
        .spawn(move || run(app))
        .ok();
}

fn run(app: AppHandle) {
    let mut dirs = watched_dirs();
    let mut dirs_at = Instant::now();
    let mut tracks: HashMap<PathBuf, Track> = HashMap::new();
    let mut seq = 1u32;
    let mut last: Vec<Download> = vec![];
    let mut tick = Instant::now();
    loop {
        std::thread::sleep(Duration::from_millis(600));
        let dt = tick.elapsed().as_secs_f64().max(0.05);
        tick = Instant::now();
        if dirs_at.elapsed() > Duration::from_secs(10) {
            dirs = watched_dirs();
            dirs_at = Instant::now();
        }

        // fichiers temporaires présents
        let mut present: HashMap<PathBuf, (u64, SystemTime)> = HashMap::new();
        for d in &dirs {
            let Ok(rd) = std::fs::read_dir(d) else { continue };
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().to_lowercase();
                if !TEMP_EXT.iter().any(|x| name.ends_with(x)) {
                    continue;
                }
                if let Ok(m) = e.metadata() {
                    present.insert(e.path(), (m.len(), m.modified().unwrap_or(SystemTime::UNIX_EPOCH)));
                }
            }
        }

        // nouveaux / mis à jour
        for (p, (size, mtime)) in &present {
            match tracks.get_mut(p) {
                Some(t) => {
                    let inst = (size.saturating_sub(t.size)) as f64 / dt;
                    t.speed = t.speed * 0.6 + inst * 0.4;
                    if *size != t.size {
                        t.size = *size;
                        t.last_change = Instant::now();
                    }
                    t.seen = Instant::now();
                }
                None => {
                    // un vieux .part abandonné ne compte pas : seulement ceux écrits à l'instant
                    let fresh = mtime.elapsed().map(|e| e < Duration::from_secs(20)).unwrap_or(false);
                    if fresh {
                        tracks.insert(p.clone(), Track { id: seq, size: *size, speed: 0.0, last_change: Instant::now(), seen: Instant::now() });
                        seq += 1;
                    }
                }
            }
        }

        // terminés (le fichier temporaire a disparu)
        let gone: Vec<PathBuf> = tracks.keys().filter(|p| !present.contains_key(*p)).cloned().collect();
        for p in gone {
            let t = tracks.remove(&p).unwrap();
            if let Some(done) = finished(&p, t.size) {
                crate::log(&format!("téléchargement terminé : {}", done.name));
                let _ = app.emit("download-done", done);
            }
        }

        // en cours = a reçu des données ces 15 dernières secondes
        let mut list: Vec<Download> = tracks
            .iter()
            .filter(|(_, t)| t.last_change.elapsed() < Duration::from_secs(15))
            .map(|(p, t)| Download { id: t.id, name: display_name(p), size: t.size, speed: if t.last_change.elapsed() > Duration::from_secs(3) { 0 } else { t.speed as u64 } })
            .collect();
        list.sort_by_key(|d| d.id);
        // pas de liste figée sur des fichiers abandonnés depuis longtemps
        tracks.retain(|_, t| t.last_change.elapsed() < Duration::from_secs(600));
        if list != last {
            let _ = app.emit("downloads", &list);
            last = list;
        }
    }
}

fn strip_temp(p: &Path) -> PathBuf {
    let s = p.to_string_lossy();
    for x in TEMP_EXT {
        if s.to_lowercase().ends_with(x) {
            return PathBuf::from(&s[..s.len() - x.len()]);
        }
    }
    p.to_path_buf()
}

fn display_name(p: &Path) -> String {
    let n = strip_temp(p).file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    // Chrome : « Unconfirmed 123456 » tant que le nom n'est pas connu
    if n.starts_with("Unconfirmed ") { "Téléchargement…".into() } else { n }
}

/// Fichier final d'un téléchargement terminé (None = annulé).
fn finished(tmp: &Path, size: u64) -> Option<Done> {
    let mut fin = strip_temp(tmp);
    if !fin.is_file() || fin.metadata().map(|m| m.len() == 0).unwrap_or(true) {
        // Chrome renomme vers un nom différent : le fichier le plus récent du dossier
        let dir = tmp.parent()?;
        let recent = std::fs::read_dir(dir).ok()?.flatten().filter_map(|e| {
            let m = e.metadata().ok()?;
            let age = m.modified().ok()?.elapsed().ok()?;
            let name = e.file_name().to_string_lossy().to_lowercase();
            (m.is_file() && age < Duration::from_secs(4) && m.len() > 0 && !TEMP_EXT.iter().any(|x| name.ends_with(x))).then(|| (age, e.path()))
        }).min_by_key(|(a, _)| *a)?;
        fin = recent.1;
    }
    let len = fin.metadata().ok()?.len();
    if len == 0 {
        return None;
    }
    let name = fin.file_name()?.to_string_lossy().to_string();
    let ext = fin.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    let thumb = if matches!(ext.as_str(), "png" | "jpg" | "jpeg") && len < 30 << 20 {
        std::fs::read(&fin).ok().and_then(|b| crate::media::shrink(&b, 96))
    } else {
        None
    };
    Some(Done { name, path: fin.to_string_lossy().to_string(), size: len.max(size), thumb })
}

/// Dossier Téléchargements de Windows + dossiers choisis dans Zen / Firefox + dossier temporaire.
pub fn watched_dirs() -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = vec![];
    let mut add = |p: PathBuf| {
        if p.is_dir() && !v.iter().any(|x| x.to_string_lossy().eq_ignore_ascii_case(&p.to_string_lossy())) {
            v.push(p);
        }
    };
    if let Some(d) = known_downloads() {
        add(d);
    }
    if let Some(app) = std::env::var_os("APPDATA").map(PathBuf::from) {
        for browser in [r"zen\Profiles", r"Mozilla\Firefox\Profiles"] {
            let Ok(rd) = std::fs::read_dir(app.join(browser)) else { continue };
            for prof in rd.flatten() {
                let Ok(prefs) = std::fs::read_to_string(prof.path().join("prefs.js")) else { continue };
                for key in ["browser.download.dir", "browser.download.lastDir"] {
                    if let Some(p) = pref_str(&prefs, key) {
                        add(PathBuf::from(p));
                    }
                }
            }
        }
    }
    // avec « toujours demander où enregistrer », le navigateur commence dans le dossier temporaire
    if let Some(t) = std::env::var_os("TEMP") {
        add(PathBuf::from(t));
    }
    v
}

fn pref_str(prefs: &str, key: &str) -> Option<String> {
    let needle = format!("user_pref(\"{key}\", \"");
    let i = prefs.find(&needle)? + needle.len();
    let rest = &prefs[i..];
    let end = rest.find("\");")?;
    Some(rest[..end].replace("\\\\", "\\"))
}

pub fn known_downloads() -> Option<PathBuf> {
    use windows::Win32::System::Com::CoTaskMemFree;
    use windows::Win32::UI::Shell::{FOLDERID_Downloads, SHGetKnownFolderPath, KF_FLAG_DEFAULT};
    unsafe {
        let p = SHGetKnownFolderPath(&FOLDERID_Downloads, KF_FLAG_DEFAULT, None).ok()?;
        let s = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as _));
        s.map(PathBuf::from)
    }
}

/// Le fichier doit être dans un dossier surveillé (on n'ouvre pas n'importe quel chemin).
pub fn reveal(path: &str) -> Result<(), String> {
    let p = PathBuf::from(path);
    let ok = p.exists() && p.parent().map(|d| watched_dirs().iter().any(|w| w.to_string_lossy().eq_ignore_ascii_case(&d.to_string_lossy()))).unwrap_or(false);
    if !ok {
        return Err("Fichier introuvable".into());
    }
    use std::os::windows::process::CommandExt;
    std::process::Command::new("explorer")
        .raw_arg(format!("/select,\"{}\"", p.display()))
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore]
    fn dirs() {
        println!("{:?}", super::watched_dirs());
    }
}
