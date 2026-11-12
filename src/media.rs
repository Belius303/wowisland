//! Musique / médias via Windows GSMTC (Spotify, Spotifast, navigateurs, Apple Music, Deezer, VLC…).
//! Toutes les sessions sont suivies en parallèle (ex. un live dans le navigateur + Spotify).

use base64::{engine::general_purpose::STANDARD as B64, Engine};
use serde::Serialize;
use std::collections::HashMap;
use std::time::Duration;
use tauri::{AppHandle, Emitter};
use windows::Media::Control::{
    GlobalSystemMediaTransportControlsSession as Session,
    GlobalSystemMediaTransportControlsSessionManager as Manager,
    GlobalSystemMediaTransportControlsSessionPlaybackStatus as Status,
};
use windows::Storage::Streams::{DataReader, IRandomAccessStreamReference};

#[derive(Serialize, Clone, PartialEq, Default)]
pub struct MediaState {
    /// identifiant de la session (AppUserModelId), sert à piloter la bonne source
    pub id: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub app: String,
    pub playing: bool,
    /// secondes
    pub position: f64,
    pub duration: f64,
    /// identifiant de la pochette (0 = aucune). L'image elle-même n'est envoyée qu'une fois,
    /// via la commande `media_art`, pour ne pas renvoyer des centaines de Ko toutes les 500 ms.
    pub art_key: u32,
    /// session « courante » selon Windows
    pub current: bool,
    /// lecture aléatoire / répétition (0 non, 1 morceau, 2 liste) : None si l'appli ne les expose pas
    pub shuffle: Option<bool>,
    pub repeat: Option<u8>,
}

struct ArtCache {
    track: String,
    key: u32,
    /// empreinte de l'image brute affichée
    hash: u64,
    /// relectures restantes après un changement de morceau
    retries: u8,
}

static ARTS: once_cell::sync::Lazy<parking_lot::Mutex<HashMap<String, (u32, String)>>> = once_cell::sync::Lazy::new(Default::default);
static ART_SEQ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);

/// Pochette actuelle d'une session (data URL réduite), si sa clé correspond.
pub fn art(id: &str) -> Option<(u32, String)> {
    ARTS.lock().get(id).cloned()
}

/// Signature sans la position (qui change à chaque tick) : on n'émet que sur un vrai changement.
fn signature(list: &[MediaState]) -> Vec<(String, String, String, bool, u64, u32, bool, Option<bool>, Option<u8>)> {
    list.iter().map(|m| (m.id.clone(), m.title.clone(), m.artist.clone(), m.playing, m.duration as u64, m.art_key, m.current, m.shuffle, m.repeat)).collect()
}

pub fn start(app: AppHandle) {
    std::thread::Builder::new()
        .name("media".into())
        .spawn(move || {
            let mut last = vec![];
            let mut arts: HashMap<String, ArtCache> = HashMap::new();
            let mut manager: Option<Manager> = None;
            let mut tick: u32 = 0;
            loop {
                if manager.is_none() {
                    manager = Manager::RequestAsync().and_then(|op| op.join()).ok();
                }
                let list = manager.as_ref().map(|m| read_all(m, &mut arts)).unwrap_or_default();
                tick += 1;
                // Renvoi à chaque changement, et toutes les ~5 s pour resynchroniser les positions
                // (l'UI extrapole la position entre deux envois).
                let sig = signature(&list);
                if sig != last || tick % 10 == 0 {
                    let _ = app.emit("media", &list);
                    last = sig;
                }
                std::thread::sleep(Duration::from_millis(500));
            }
        })
        .ok();
}

fn read_all(m: &Manager, arts: &mut HashMap<String, ArtCache>) -> Vec<MediaState> {
    let current_id = m.GetCurrentSession().and_then(|s| s.SourceAppUserModelId()).map(|s| s.to_string()).unwrap_or_default();
    let Ok(sessions) = m.GetSessions() else { return vec![] };
    let mut out = vec![];
    for i in 0..sessions.Size().unwrap_or(0) {
        let Ok(s) = sessions.GetAt(i) else { continue };
        if let Ok(Some(mut st)) = read_session(&s, arts) {
            st.current = st.id == current_id;
            out.push(st);
        }
    }
    arts.retain(|k, _| out.iter().any(|m| &m.id == k));
    ARTS.lock().retain(|k, _| arts.contains_key(k));
    out
}

fn read_session(session: &Session, arts: &mut HashMap<String, ArtCache>) -> windows::core::Result<Option<MediaState>> {
    let id = session.SourceAppUserModelId()?.to_string();
    let props = session.TryGetMediaPropertiesAsync()?.join()?;
    let title = props.Title()?.to_string();
    if title.is_empty() {
        return Ok(None);
    }
    let artist = props.Artist()?.to_string();
    let album = props.AlbumTitle().map(|s| s.to_string()).unwrap_or_default();
    let info = session.GetPlaybackInfo()?;
    let playing = info.PlaybackStatus()? == Status::Playing;
    let ctl = info.Controls().ok();
    let shuffle = ctl.as_ref().filter(|c| c.IsShuffleEnabled().unwrap_or(false)).map(|_| info.IsShuffleActive().and_then(|v| v.Value()).unwrap_or(false));
    let repeat = ctl.as_ref().filter(|c| c.IsRepeatEnabled().unwrap_or(false)).map(|_| info.AutoRepeatMode().and_then(|v| v.Value()).map(|r| r.0 as u8).unwrap_or(0));
    let tl = session.GetTimelineProperties()?;
    let mut position = tl.Position()?.Duration as f64 / 1e7;
    let duration = (tl.EndTime()?.Duration - tl.StartTime()?.Duration).max(0) as f64 / 1e7;
    // La position n'est mise à jour par l'appli que ponctuellement : on extrapole.
    if playing {
        if let Ok(updated) = tl.LastUpdatedTime() {
            let now = chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0) / 100 + 116_444_736_000_000_000;
            let delta = (now - updated.UniversalTime) as f64 / 1e7;
            if delta > 0.0 && delta < 3600.0 {
                position += delta;
            }
        }
    }
    if duration > 0.0 {
        position = position.clamp(0.0, duration);
    }

    let track = format!("{title}\u{1}{artist}\u{1}{album}");
    let entry = arts.entry(id.clone()).or_insert(ArtCache { track: String::new(), key: 0, hash: 0, retries: 0 });
    // Au changement de morceau, Spotify & co publient le nouveau titre AVANT la nouvelle pochette :
    // lue tout de suite, on récupère encore celle du morceau précédent (pochette « en retard » d'un titre).
    // On relit donc la pochette pendant ~5 s et on la remplace dès que l'image change vraiment.
    if entry.track != track {
        entry.track = track;
        entry.retries = 10;
    }
    if entry.retries > 0 || entry.key == 0 {
        entry.retries = entry.retries.saturating_sub(1);
        if let Some(raw) = props.Thumbnail().ok().and_then(|t| read_thumbnail_bytes(&t).ok()) {
            let hash = {
                use std::hash::{Hash, Hasher};
                let mut h = std::collections::hash_map::DefaultHasher::new();
                raw.hash(&mut h);
                h.finish()
            };
            if hash != entry.hash || entry.key == 0 {
                let key = ART_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                ARTS.lock().insert(id.clone(), (key, encode_thumbnail(&raw, 200)));
                entry.key = key;
                entry.hash = hash;
            }
        }
    }

    let app = friendly_app(&id);
    Ok(Some(MediaState { id, title, artist, album, app, playing, position, duration, art_key: entry.key, current: false, shuffle, repeat }))
}

pub fn read_thumbnail_sized(r: &IRandomAccessStreamReference, max: u32) -> windows::core::Result<String> {
    read_thumbnail_bytes(r).map(|b| encode_thumbnail(&b, max))
}

fn read_thumbnail_bytes(r: &IRandomAccessStreamReference) -> windows::core::Result<Vec<u8>> {
    let stream = r.OpenReadAsync()?.join()?;
    let size = stream.Size()? as u32;
    let reader = DataReader::CreateDataReader(&stream)?;
    reader.LoadAsync(size)?.join()?;
    let mut buf = vec![0u8; size as usize];
    reader.ReadBytes(&mut buf)?;
    if buf.is_empty() {
        return Err(windows::core::Error::from(windows::Win32::Foundation::E_FAIL));
    }
    Ok(buf)
}

fn encode_thumbnail(buf: &[u8], max: u32) -> String {
    // Certaines applis (ex. Spotifast) déclarent un type invalide : on se fie au contenu réel du fichier.
    let ctype = sniff_image(buf).unwrap_or_else(|| "image/png".into());
    shrink(buf, max).unwrap_or_else(|| format!("data:{ctype};base64,{}", B64.encode(buf)))
}

/// Réduit une image à `max` px de côté (JPEG si opaque, PNG sinon) : beaucoup moins de mémoire
/// dans la WebView (une pochette 1000×1000 décodée pèse 4 Mo par <img>).
pub fn shrink(buf: &[u8], max: u32) -> Option<String> {
    let img = image::load_from_memory(buf).ok()?;
    let img = if img.width() > max || img.height() > max { img.thumbnail(max, max) } else { img };
    let mut out = std::io::Cursor::new(Vec::new());
    if img.color().has_alpha() {
        img.write_to(&mut out, image::ImageFormat::Png).ok()?;
        Some(format!("data:image/png;base64,{}", B64.encode(out.into_inner())))
    } else {
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 88).encode_image(&img.to_rgb8()).ok()?;
        Some(format!("data:image/jpeg;base64,{}", B64.encode(out.into_inner())))
    }
}

fn sniff_image(b: &[u8]) -> Option<String> {
    let t = if b.starts_with(&[0xFF, 0xD8, 0xFF]) {
        "image/jpeg"
    } else if b.starts_with(&[0x89, b'P', b'N', b'G']) {
        "image/png"
    } else if b.starts_with(b"GIF8") {
        "image/gif"
    } else if b.len() > 12 && &b[0..4] == b"RIFF" && &b[8..12] == b"WEBP" {
        "image/webp"
    } else if b.starts_with(b"BM") {
        "image/bmp"
    } else {
        return None;
    };
    Some(t.into())
}

pub fn friendly_app(id: &str) -> String {
    let l = id.to_lowercase();
    let known = [
        ("spotifast", "Spotifast"),
        ("spotify", "Spotify"),
        ("chrome", "Chrome"),
        ("msedge", "Edge"),
        ("firefox", "Firefox"),
        ("operagx", "Opera GX"),
        ("opera", "Opera"),
        ("brave", "Brave"),
        ("applemusic", "Apple Music"),
        ("deezer", "Deezer"),
        ("vlc", "VLC"),
        ("zunemusic", "Lecteur multimédia"),
        ("tidal", "TIDAL"),
        ("discord", "Discord"),
        ("twitch", "Twitch"),
    ];
    if l.contains("dynamic-island") || l.contains("dynamic_island") || l.contains("dynamicisland") {
        return "Ma musique".into();
    }
    for (k, v) in known {
        if l.contains(k) {
            return v.to_string();
        }
    }
    let name = id.trim_end_matches(".exe").rsplit(['!', '\\']).next().unwrap_or(id);
    // Identifiants opaques (ex. "F0DC299D809B9700" pour certains navigateurs) : on n'affiche rien.
    if name.len() >= 10 && name.chars().all(|c| c.is_ascii_hexdigit()) {
        return String::new();
    }
    name.to_string()
}

fn find_session(id: Option<&str>) -> Result<Session, String> {
    let m = Manager::RequestAsync().and_then(|o| o.join()).map_err(|e| e.to_string())?;
    if let Some(id) = id.filter(|s| !s.is_empty()) {
        let sessions = m.GetSessions().map_err(|e| e.to_string())?;
        for i in 0..sessions.Size().unwrap_or(0) {
            if let Ok(s) = sessions.GetAt(i) {
                if s.SourceAppUserModelId().map(|x| x.to_string() == id).unwrap_or(false) {
                    return Ok(s);
                }
            }
        }
    }
    m.GetCurrentSession().map_err(|_| "Aucun média en cours".to_string())
}

pub fn control(id: Option<&str>, action: &str) -> Result<(), String> {
    let s = find_session(id)?;
    (|| -> windows::core::Result<()> {
        match action {
            "play_pause" => { s.TryTogglePlayPauseAsync()?.join()?; }
            "next" => { s.TrySkipNextAsync()?.join()?; }
            "prev" => { s.TrySkipPreviousAsync()?.join()?; }
            "shuffle" => {
                let on = s.GetPlaybackInfo()?.IsShuffleActive().and_then(|v| v.Value()).unwrap_or(false);
                s.TryChangeShuffleActiveAsync(!on)?.join()?;
            }
            "repeat" => {
                use windows::Media::MediaPlaybackAutoRepeatMode as M;
                let cur = s.GetPlaybackInfo()?.AutoRepeatMode().and_then(|v| v.Value()).unwrap_or(M::None);
                // non → toute la liste → ce morceau → non
                let next = if cur == M::None { M::List } else if cur == M::List { M::Track } else { M::None };
                s.TryChangeAutoRepeatModeAsync(next)?.join()?;
            }
            _ => {}
        }
        Ok(())
    })()
    .map_err(|e| e.to_string())
}

pub fn seek(id: Option<&str>, seconds: f64) -> Result<(), String> {
    let s = find_session(id)?;
    s.TryChangePlaybackPositionAsync((seconds * 1e7) as i64).and_then(|o| o.join()).map(|_| ()).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    #[test]
    fn dump_sessions() {
        let m = super::Manager::RequestAsync().unwrap().join().unwrap();
        let s = m.GetSessions().unwrap();
        for i in 0..s.Size().unwrap() {
            let x = s.GetAt(i).unwrap();
            let id = x.SourceAppUserModelId().unwrap().to_string();
            let p = x.TryGetMediaPropertiesAsync().unwrap().join().unwrap();
            let th = p.Thumbnail();
            let art = match &th {
                Ok(t) => match super::read_thumbnail_sized(t, 200) { Ok(s) => format!("ok {} chars {}", s.len(), &s[..40]), Err(e) => format!("read err {e}") },
                Err(e) => format!("no thumb {e}"),
            };
            println!("SESSION {id} | {} - {} | art: {art}", p.Title().unwrap(), p.Artist().unwrap());
        }
    }
}
