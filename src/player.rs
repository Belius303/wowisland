//! « Ma musique » : lecteur des musiques de l'île, natif (Rust + moteur de lecture de Windows).
//!
//! - Bibliothèque = Musique\Spotify (Île) + Téléchargements\Île (fichiers audio), dédoublonnée par nom.
//! - Lecture par le MediaPlayer de Windows (décodage par le système, aucun navigateur) : la session
//!   apparaît dans les contrôles média de Windows, donc dans l'île comme Spotify, touches multimédia comprises.
//! - Optimisé : les tags ne sont lus qu'une fois par fichier (cache disque chemin + date + taille),
//!   la file d'attente est construite sans ouvrir les fichiers, et la pochette n'est chargée que pour
//!   le morceau qui joue (et pour les lignes affichées dans le panneau, gardée en mémoire ensuite).

use once_cell::sync::Lazy;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Emitter};
use windows::core::{Interface, Ref, HSTRING};
use windows::Foundation::{TypedEventHandler, Uri};
use windows::Media::Core::MediaSource;
use windows::Media::MediaPlaybackType;
use windows::Media::Playback::{CurrentMediaPlaybackItemChangedEventArgs, MediaPlaybackItem, MediaPlaybackList, MediaPlayer, MediaPlayerAudioCategory};
use windows::Storage::FileProperties::{StorageItemThumbnail, ThumbnailMode, ThumbnailOptions, ThumbnailType};
use windows::Storage::StorageFile;
use windows::Storage::Streams::{DataReader, IInputStream, RandomAccessStreamReference};

#[derive(Serialize, Clone)]
pub struct Track {
    /// chemin complet du fichier (sert d'identifiant)
    pub id: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    /// secondes
    pub duration: f64,
    /// date d'ajout (ms)
    pub added: i64,
}

#[derive(Serialize, Deserialize, Clone, Default)]
struct Tags {
    mtime: i64,
    size: u64,
    title: String,
    artist: String,
    album: String,
    duration: f64,
}

static TAGS: Lazy<Mutex<Option<HashMap<String, Tags>>>> = Lazy::new(|| Mutex::new(None));
static COVERS: Lazy<Mutex<HashMap<String, Option<String>>>> = Lazy::new(Default::default);
static PLAYER: Lazy<Mutex<Option<MediaPlayer>>> = Lazy::new(|| Mutex::new(None));
static NOW: Lazy<Mutex<Option<String>>> = Lazy::new(|| Mutex::new(None));
static APP: Lazy<Mutex<Option<AppHandle>>> = Lazy::new(|| Mutex::new(None));

pub fn init(app: AppHandle) {
    *APP.lock() = Some(app);
}

/* ---------- volume du lecteur (indépendant du volume Windows), mémorisé entre deux lancements ---------- */
static VOLUME: Lazy<Mutex<u8>> = Lazy::new(|| {
    let v = std::fs::read_to_string(crate::config::dir().join("music-volume"))
        .ok()
        .and_then(|s| s.trim().parse::<u8>().ok())
        .unwrap_or(100);
    Mutex::new(v.min(100))
});

pub fn volume() -> u8 {
    *VOLUME.lock()
}

pub fn set_volume(v: u8) {
    let v = v.min(100);
    *VOLUME.lock() = v;
    if let Some(p) = PLAYER.lock().as_ref() {
        let _ = p.SetVolume(v as f64 / 100.0);
    }
    let _ = std::fs::write(crate::config::dir().join("music-volume"), v.to_string());
}

fn cache_path() -> PathBuf {
    crate::config::dir().join("music-cache.json")
}

fn folders() -> Vec<PathBuf> {
    // les MP3 ajoutés avec le bouton « Ma musique » du téléchargeur, puis ceux envoyés vers Spotify
    vec![crate::grab::music_dir(), crate::grab::spotify_dir()]
}

fn stat(p: &Path) -> Option<(i64, u64)> {
    let m = std::fs::metadata(p).ok()?;
    let t = m.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_millis() as i64;
    Some((t, m.len()))
}

fn file(path: &str) -> windows::core::Result<StorageFile> {
    StorageFile::GetFileFromPathAsync(&HSTRING::from(path))?.join()
}

fn read_tags(path: &str, mtime: i64, size: u64) -> Tags {
    let r = (|| -> windows::core::Result<Tags> {
        let m = file(path)?.Properties()?.GetMusicPropertiesAsync()?.join()?;
        Ok(Tags {
            mtime,
            size,
            title: m.Title()?.to_string().trim().to_string(),
            artist: m.Artist()?.to_string().trim().to_string(),
            album: m.Album()?.to_string().trim().to_string(),
            duration: m.Duration()?.Duration as f64 / 1e7,
        })
    })();
    r.unwrap_or(Tags { mtime, size, ..Default::default() })
}

/// « Artiste - Titre [idYouTube].mp3 » → (titre, artiste)
fn from_name(p: &Path) -> (String, String) {
    let mut s = p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    if let Some(i) = s.rfind(" [") {
        if s.ends_with(']') && s.len() - i <= 16 {
            s.truncate(i);
        }
    }
    let s = s.replace('｜', "|").replace("||", "|").trim().to_string();
    match s.split_once(" - ").or_else(|| s.split_once("- ")) {
        Some((a, t)) if !a.trim().is_empty() && !t.trim().is_empty() && a.len() < 60 => (t.trim().to_string(), a.trim().to_string()),
        _ => (s, String::new()),
    }
}

/// La bibliothèque, la plus récente d'abord. Rapide : seuls les nouveaux fichiers sont lus.
pub fn library() -> Vec<Track> {
    let mut guard = TAGS.lock();
    let tags = guard.get_or_insert_with(|| std::fs::read_to_string(cache_path()).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default());
    let mut seen = HashSet::new();
    let mut out = vec![];
    let mut dirty = false;
    for dir in folders() {
        for p in crate::grab::audio_files(&dir) {
            let name = p.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
            if !seen.insert(name) {
                continue;
            }
            let Some((mtime, size)) = stat(&p) else { continue };
            let id = p.to_string_lossy().to_string();
            let t = match tags.get(&id) {
                Some(t) if t.mtime == mtime && t.size == size => t.clone(),
                _ => {
                    dirty = true;
                    let t = read_tags(&id, mtime, size);
                    tags.insert(id.clone(), t.clone());
                    t
                }
            };
            let (nt, na) = from_name(&p);
            out.push(Track {
                title: if t.title.is_empty() { nt } else { t.title },
                artist: if t.artist.is_empty() { na } else { t.artist },
                album: t.album,
                duration: t.duration,
                added: mtime,
                id,
            });
        }
    }
    let before = tags.len();
    tags.retain(|k, _| out.iter().any(|t| &t.id == k));
    if dirty || tags.len() != before {
        if let Ok(s) = serde_json::to_string(&*tags) {
            let _ = std::fs::write(cache_path(), s);
        }
    }
    out.sort_by(|a, b| b.added.cmp(&a.added));
    out
}

pub fn now() -> Option<String> {
    NOW.lock().clone()
}

fn thumbnail(path: &str, size: u32) -> Option<StorageItemThumbnail> {
    let t = file(path).ok()?.GetThumbnailAsync(ThumbnailMode::MusicView, size, ThumbnailOptions::UseCurrentScale).ok()?.join().ok()?;
    // pas de pochette dans le fichier : Windows renvoie une icône générique, on n'en veut pas
    (t.Type().ok()? == ThumbnailType::Image).then_some(t)
}

/// Pochette réduite pour la liste (data URL), mise en cache.
pub fn cover(id: &str) -> Option<String> {
    if let Some(c) = COVERS.lock().get(id) {
        return c.clone();
    }
    let art = (|| {
        let t = thumbnail(id, 96)?;
        let size = t.Size().ok()? as u32;
        let reader = DataReader::CreateDataReader(&t.cast::<IInputStream>().ok()?).ok()?;
        reader.LoadAsync(size).ok()?.join().ok()?;
        let mut buf = vec![0u8; size as usize];
        reader.ReadBytes(&mut buf).ok()?;
        crate::media::shrink(&buf, 96)
    })();
    let mut c = COVERS.lock();
    if c.len() > 400 {
        c.clear();
    }
    c.insert(id.to_string(), art.clone());
    art
}

/// file:///C:/… avec les caractères spéciaux encodés (espaces, #, accents…)
fn file_uri(path: &str) -> String {
    let mut s = String::from("file:///");
    for b in path.replace('\\', "/").bytes() {
        if b.is_ascii_alphanumeric() || b"-._~/:".contains(&b) {
            s.push(b as char);
        } else {
            s.push_str(&format!("%{b:02X}"));
        }
    }
    s
}

/// Source d'un titre. Par URI (instantané) quand le chemin est en ASCII ; sinon par le fichier lui-même :
/// Windows ne retrouve pas un fichier dont l'URI contient des caractères non ASCII encodés (« ｜ », « é »…).
fn source(path: &str) -> windows::core::Result<MediaSource> {
    if path.is_ascii() {
        MediaSource::CreateFromUri(&Uri::CreateUri(&HSTRING::from(file_uri(path)))?)
    } else {
        MediaSource::CreateFromStorageFile(&file(path)?)
    }
}

fn player() -> windows::core::Result<MediaPlayer> {
    let mut g = PLAYER.lock();
    if let Some(p) = g.as_ref() {
        return Ok(p.clone());
    }
    let p = MediaPlayer::new()?;
    let _ = p.SetAudioCategory(MediaPlayerAudioCategory::Media);
    let _ = p.SetVolume(volume() as f64 / 100.0);
    p.CommandManager()?.SetIsEnabled(true)?;
    *g = Some(p.clone());
    Ok(p)
}

/// Ajoute la pochette au morceau (visible dans l'île et les contrôles de Windows).
fn decorate(item: &MediaPlaybackItem, path: &str) {
    let Some(t) = thumbnail(path, 300) else { return };
    let _ = (|| -> windows::core::Result<()> {
        let props = item.GetDisplayProperties()?;
        props.SetThumbnail(&RandomAccessStreamReference::CreateFromStream(&t)?)?;
        item.ApplyDisplayProperties(&props)
    })();
}

/// Joue `ids` à partir de `start` (aléatoire si `shuffle`).
pub fn play(ids: Vec<String>, start: usize, shuffle: bool) -> Result<(), String> {
    if ids.is_empty() {
        return Err("Aucune musique".into());
    }
    let lib: HashMap<String, Track> = library().into_iter().map(|t| (t.id.clone(), t)).collect();
    let ids: Vec<String> = ids.into_iter().filter(|i| lib.contains_key(i)).collect();
    if ids.is_empty() {
        return Err("Fichier introuvable (déplacé ou supprimé ?)".into());
    }
    let start = start.min(ids.len() - 1);
    (|| -> windows::core::Result<()> {
        let list = MediaPlaybackList::new()?;
        let items = list.Items()?;
        for id in &ids {
            let t = &lib[id];
            let src = source(id)?;
            let item = MediaPlaybackItem::Create(&src)?;
            let props = item.GetDisplayProperties()?;
            props.SetType(MediaPlaybackType::Music)?;
            let mp = props.MusicProperties()?;
            mp.SetTitle(&HSTRING::from(&t.title))?;
            mp.SetArtist(&HSTRING::from(&t.artist))?;
            mp.SetAlbumTitle(&HSTRING::from(&t.album))?;
            item.ApplyDisplayProperties(&props)?;
            items.Append(&item)?;
        }
        let first = items.GetAt(start as u32)?;
        decorate(&first, &ids[start]);
        list.SetShuffleEnabled(shuffle)?;
        list.SetAutoRepeatEnabled(false)?;
        list.SetStartingItem(&first)?;
        let paths = ids.clone();
        let done = Mutex::new(HashSet::from([start as u32]));
        list.CurrentItemChanged(&TypedEventHandler::new(move |l: Ref<MediaPlaybackList>, a: Ref<CurrentMediaPlaybackItemChangedEventArgs>| {
            let (Some(l), Some(a)) = (l.as_ref(), a.as_ref()) else { return Ok(()) };
            let Ok(item) = a.NewItem() else { return Ok(()) };
            let mut i = 0u32;
            if l.Items()?.IndexOf(&item, &mut i)? {
                if let Some(p) = paths.get(i as usize) {
                    if done.lock().insert(i) {
                        decorate(&item, p);
                    }
                    *NOW.lock() = Some(p.clone());
                    if let Some(app) = APP.lock().as_ref() {
                        let _ = app.emit("music-now", p);
                    }
                }
            }
            Ok(())
        }))?;
        let p = player()?;
        p.SetSource(&list)?;
        p.Play()?;
        Ok(())
    })()
    .map_err(|e| format!("Lecture impossible : {}", e.message()))?;
    *NOW.lock() = Some(ids[start].clone());
    Ok(())
}

/// Supprime un titre de Ma musique (fichier à la Corbeille). S'il joue, la lecture s'arrête d'abord.
pub fn delete(id: &str) -> Result<(), String> {
    let path = PathBuf::from(id);
    // uniquement un fichier audio d'un des dossiers de la bibliothèque
    let ok = folders().iter().any(|d| path.parent() == Some(d.as_path())) && !crate::grab::audio_files(&path).is_empty();
    if !ok {
        return Err("Titre introuvable".into());
    }
    if NOW.lock().as_deref() == Some(id) {
        if let Some(p) = PLAYER.lock().as_ref() {
            let _ = p.Pause();
            let _ = p.SetSource(None::<&windows::Media::Playback::IMediaPlaybackSource>);
        }
        *NOW.lock() = None;
    }
    crate::grab::to_recycle_bin(&[path.clone()])?;
    if let Some(t) = TAGS.lock().as_mut() {
        t.remove(id);
    }
    COVERS.lock().remove(id);
    if let Some(name) = path.file_name() {
        crate::grab::music_removed(name);
    }
    crate::log("lecteur : un titre supprimé de Ma musique (Corbeille)");
    Ok(())
}

pub fn open_folder() -> Result<(), String> {
    let dir = crate::grab::music_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    std::process::Command::new("explorer").arg(&dir).spawn().map(|_| ()).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    /// Chaque titre de la bibliothèque doit pouvoir s'ouvrir dans le lecteur.
    /// cargo test --release --bin dynamic-island library_opens -- --ignored --nocapture
    #[test]
    #[ignore]
    fn library_opens() {
        for p in library().iter().map(|t| t.id.clone()) {
            let r = (|| -> windows::core::Result<()> {
                let src = source(&p)?;
                src.OpenAsync()?.join()
            })();
            println!("{} | {:?}", p.rsplit(std::path::MAIN_SEPARATOR).next().unwrap_or(""), r.as_ref().map_err(|e| e.message()));
            assert!(r.is_ok(), "{p}");
        }
    }
}
