//! Télécharger une vidéo, une musique ou une transcription depuis un lien (YouTube, TikTok, X, Reddit,
//! Twitch…), avec yt-dlp + ffmpeg (+ Deno pour YouTube). Les fichiers vont dans Téléchargements\Île.
//!
//! Outils : yt-dlp est gardé dans %LOCALAPPDATA%\DynamicIsland\tools (copie à nous, mise à jour
//! automatiquement : les sites changent souvent). ffmpeg et Deno déjà installés sur le PC sont réutilisés ;
//! sinon on les télécharge au même endroit, depuis leurs pages officielles (GitHub), au premier usage.
//! Contenus protégés par DRM (Netflix, Disney+…) : impossibles, on le dit tout de suite.
//! Liens Spotify / Deezer : on lit la liste des titres (pages publiques), on cherche chaque titre sur
//! YouTube et on le télécharge en MP3, avec la pochette et les bonnes infos.
//! L'historique des téléchargements est gardé dans %APPDATA%\DynamicIsland\grab-history.json.

use once_cell::sync::Lazy;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime};
use tauri::{AppHandle, Emitter};

const NO_WINDOW: u32 = 0x0800_0000;
const MAX_RUNNING: usize = 2;
/// durée maximale d'un GIF (secondes)
const GIF_MAX: f64 = 30.0;

const URL_YTDLP: &str = "https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp.exe";
const URL_FFMPEG: &str = "https://github.com/yt-dlp/FFmpeg-Builds/releases/download/latest/ffmpeg-master-latest-win64-gpl.zip";
const URL_DENO: &str = "https://github.com/denoland/deno/releases/latest/download/deno-x86_64-pc-windows-msvc.zip";

#[derive(Serialize, Deserialize, Clone, Default)]
#[serde(default)]
pub struct Job {
    pub id: u32,
    pub url: String,
    pub title: String,
    pub author: String,
    pub thumb: Option<String>,
    /// "video" | "audio" | "text" | "gif"
    pub kind: String,
    /// libellé du choix (« 1080p », « MP3 »…)
    pub label: String,
    /// "queued" | "running" | "post" | "done" | "error" | "stopped"
    pub status: String,
    /// 0..1 (None = inconnu, ex. live)
    pub pct: Option<f64>,
    pub bytes: u64,
    pub speed: u64,
    pub eta: Option<u64>,
    /// playlist : élément en cours / nombre total
    pub item: Option<(u32, u32)>,
    pub live: bool,
    pub path: Option<String>,
    pub files: u32,
    /// playlist / mix : tous les fichiers obtenus, dans l'ordre (path = le dernier)
    pub items: Vec<String>,
    pub error: Option<String>,
    /// « Spotify » / « Deezer » : titres retrouvés sur YouTube
    pub music: Option<String>,
    /// titre en cours (liste de musiques)
    pub cur: Option<String>,
    /// ajouté aux Titres likés de Spotify (« Titre — Artiste » ou « 12 titres »)
    pub liked: Option<String>,
    /// MP3 mis dans « Ma musique » (lecteur de l'île)
    pub in_music: bool,
    /// fin du téléchargement (ms depuis 1970)
    pub at: u64,
    /// envoi vers le tel avec LocalSend (pas gardé dans l'historique)
    #[serde(skip_deserializing)]
    pub phone: Option<Phone>,
    #[serde(skip)]
    opts: Opts,
    #[serde(skip)]
    pid: Option<u32>,
}

#[derive(Serialize, Clone, Default)]
pub struct Phone {
    /// "find" (recherche du tel) | "wait" (à accepter sur le tel) | "send" | "done" | "error"
    pub state: String,
    pub pct: Option<f64>,
    /// nom du tel
    pub dev: String,
    pub msg: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Default)]
#[serde(default)]
pub struct MTrack {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub isrc: Option<String>,
    pub duration: Option<f64>,
    /// URL de la pochette
    pub cover: Option<String>,
    /// identifiant Spotify, si connu
    pub spotify: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Default)]
#[serde(default)]
struct Opts {
    format: Option<u32>,
    start: Option<f64>,
    end: Option<f64>,
    playlist: bool,
    sub_lang: Option<String>,
    sub_auto: bool,
    expect: u64,
    vid: String,
    /// musiques (Spotify/Deezer) ou le titre d'un MP3 YouTube (pour les Titres likés)
    tracks: Vec<MTrack>,
    music: bool,
    /// playlist : seulement les N premiers éléments (mix YouTube, quasi infini)
    limit: Option<u32>,
}

#[derive(Serialize, Deserialize)]
struct Saved {
    job: Job,
    #[serde(default)]
    opts: Opts,
}

#[derive(Default)]
struct St {
    jobs: Vec<Job>,
    seq: u32,
    installing: Option<String>,
    install_err: Option<String>,
    updated: bool,
}

static ST: Lazy<Mutex<St>> = Lazy::new(|| {
    let jobs = load_history();
    let seq = jobs.iter().map(|j| j.id).max().unwrap_or(0);
    Mutex::new(St { jobs, seq, ..Default::default() })
});

/* ---------------- historique ---------------- */

fn hist_path() -> PathBuf {
    crate::config::dir().join("grab-history.json")
}

/// Téléchargements finis des sessions précédentes (ceux dont le fichier a disparu sont oubliés).
fn load_history() -> Vec<Job> {
    let Ok(raw) = std::fs::read_to_string(hist_path()) else { return vec![] };
    let list: Vec<Saved> = serde_json::from_str(&raw).unwrap_or_default();
    list.into_iter()
        .filter(|x| x.job.status != "done" || x.job.path.as_ref().map(|p| Path::new(p).exists()).unwrap_or(false))
        .map(|x| {
            let mut j = x.job;
            j.opts = x.opts;
            j.pid = None;
            // mix téléchargé avant que l'île garde la liste : on la retrouve dans son dossier (« 003 - … »)
            if j.items.is_empty() && j.files > 1 && j.kind == "audio" {
                if let Some(dir) = j.path.as_deref().map(Path::new).filter(|p| p.is_file()).and_then(Path::parent) {
                    let re = regex::Regex::new(r"^\d{3} - ").unwrap();
                    j.items = audio_files(dir).into_iter().filter(|f| f.file_name().map(|n| re.is_match(&n.to_string_lossy())).unwrap_or(false)).map(|f| f.to_string_lossy().to_string()).collect();
                }
            }
            j
        })
        .collect()
}

fn save_history() {
    let list: Vec<Saved> = {
        let st = ST.lock();
        let done: Vec<&Job> = st.jobs.iter().filter(|j| matches!(j.status.as_str(), "done" | "error")).collect();
        done[done.len().saturating_sub(100)..].iter().map(|j| Saved { job: (*j).clone(), opts: j.opts.clone() }).collect()
    };
    if let Ok(txt) = serde_json::to_string(&list) {
        let p = hist_path();
        if let Some(d) = p.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let tmp = p.with_extension("tmp");
        if std::fs::write(&tmp, txt).is_ok() {
            let _ = std::fs::rename(&tmp, &p);
        }
    }
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}
static APP: Lazy<Mutex<Option<AppHandle>>> = Lazy::new(|| Mutex::new(None));

/* ---------------- outils ---------------- */

fn tools_dir() -> PathBuf {
    crate::config::dir().join("tools")
}

pub fn out_dir() -> PathBuf {
    let dl = crate::downloads::known_downloads().unwrap_or_else(|| PathBuf::from(std::env::var("USERPROFILE").unwrap_or_default()).join("Downloads"));
    dl.join("Île")
}

fn in_path(name: &str) -> Vec<PathBuf> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path).map(|d| d.join(name)).filter(|p| p.is_file()).collect()
}

fn ytdlp() -> Option<PathBuf> {
    let p = tools_dir().join("yt-dlp.exe");
    p.is_file().then_some(p)
}

/// Dossier contenant ffmpeg.exe ET ffprobe.exe.
fn ffmpeg_dir() -> Option<PathBuf> {
    let home = PathBuf::from(std::env::var("USERPROFILE").unwrap_or_default());
    let mut cands: Vec<PathBuf> = vec![tools_dir()];
    cands.extend(in_path("ffmpeg.exe").into_iter().filter_map(|p| p.parent().map(Path::to_path_buf)));
    cands.extend([PathBuf::from(r"C:\ffmpeg\bin"), home.join(r"scoop\shims"), PathBuf::from(r"C:\ProgramData\chocolatey\bin")]);
    cands.into_iter().find(|d| d.join("ffmpeg.exe").is_file() && d.join("ffprobe.exe").is_file())
}

fn deno() -> Option<PathBuf> {
    let home = PathBuf::from(std::env::var("USERPROFILE").unwrap_or_default());
    let mut cands = vec![tools_dir().join("deno.exe")];
    cands.extend(in_path("deno.exe"));
    cands.push(home.join(r".deno\bin\deno.exe"));
    cands.into_iter().find(|p| p.is_file())
}

pub fn status() -> Value {
    let st = ST.lock();
    json!({
        "ytdlp": ytdlp().is_some(),
        "ffmpeg": ffmpeg_dir().is_some(),
        "deno": deno().is_some(),
        "ready": ytdlp().is_some() && ffmpeg_dir().is_some() && deno().is_some(),
        "installing": st.installing,
        "error": st.install_err,
        "dir": out_dir().to_string_lossy(),
    })
}

fn emit_setup() {
    if let Some(app) = APP.lock().clone() {
        let _ = app.emit("grab-setup", status());
    }
}

fn set_installing(s: Option<String>) {
    ST.lock().installing = s;
    emit_setup();
}

/// Télécharge ce qui manque (en arrière-plan). L'avancement arrive par l'évènement « grab-setup ».
pub fn install(app: &AppHandle) {
    *APP.lock() = Some(app.clone());
    {
        let mut st = ST.lock();
        if st.installing.is_some() {
            return;
        }
        st.installing = Some("Préparation…".into());
        st.install_err = None;
    }
    std::thread::spawn(|| {
        let r = install_all();
        if let Err(e) = &r {
            crate::log(&format!("téléchargeur : installation impossible : {e}"));
        }
        let mut st = ST.lock();
        st.installing = None;
        st.install_err = r.err();
        drop(st);
        emit_setup();
    });
}

fn install_all() -> Result<(), String> {
    let dir = tools_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    if ytdlp().is_none() {
        fetch_file(URL_YTDLP, &dir.join("yt-dlp.exe"), "yt-dlp")?;
        ST.lock().updated = true;
        crate::log("téléchargeur : yt-dlp installé");
    }
    if ffmpeg_dir().is_none() {
        let zip = dir.join("ffmpeg.zip");
        fetch_file(URL_FFMPEG, &zip, "ffmpeg")?;
        set_installing(Some("ffmpeg : extraction…".into()));
        extract(&zip, &dir, &["ffmpeg.exe", "ffprobe.exe"])?;
        crate::log("téléchargeur : ffmpeg installé");
    }
    if deno().is_none() {
        let zip = dir.join("deno.zip");
        fetch_file(URL_DENO, &zip, "Deno")?;
        set_installing(Some("Deno : extraction…".into()));
        extract(&zip, &dir, &["deno.exe"])?;
        crate::log("téléchargeur : Deno installé");
    }
    Ok(())
}

fn fetch_file(url: &str, dest: &Path, name: &str) -> Result<(), String> {
    set_installing(Some(format!("{name} : connexion…")));
    let resp = crate::http().get(url).timeout(Duration::from_secs(900)).call().map_err(|e| format!("{name} : {e}"))?;
    let total: u64 = resp.header("Content-Length").and_then(|s| s.parse().ok()).unwrap_or(0);
    let tmp = dest.with_extension("download");
    let mut f = std::fs::File::create(&tmp).map_err(|e| e.to_string())?;
    let mut r = resp.into_reader();
    let mut buf = vec![0u8; 256 * 1024];
    let mut got = 0u64;
    let mut last = Instant::now();
    loop {
        let n = r.read(&mut buf).map_err(|e| format!("{name} : {e}"))?;
        if n == 0 {
            break;
        }
        f.write_all(&buf[..n]).map_err(|e| e.to_string())?;
        got += n as u64;
        if last.elapsed() > Duration::from_millis(400) {
            last = Instant::now();
            let p = if total > 0 { format!("{} %", got * 100 / total) } else { format!("{} Mo", got >> 20) };
            set_installing(Some(format!("{name} : {p}")));
        }
    }
    drop(f);
    if got < 1 << 20 || (total > 0 && got != total) {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("{name} : téléchargement incomplet"));
    }
    let _ = std::fs::remove_file(dest);
    std::fs::rename(&tmp, dest).map_err(|e| e.to_string())
}

/// Extrait quelques fichiers d'un zip (tar.exe de Windows lit les zip), à plat dans `dir`.
fn extract(zip: &Path, dir: &Path, names: &[&str]) -> Result<(), String> {
    let tmp = dir.join("extract");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).map_err(|e| e.to_string())?;
    let tar = PathBuf::from(std::env::var("SystemRoot").unwrap_or(r"C:\Windows".into())).join(r"System32\tar.exe");
    let out = Command::new(tar).arg("-xf").arg(zip).arg("-C").arg(&tmp).creation_flags(NO_WINDOW).output().map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(format!("extraction impossible : {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    for n in names {
        let found = walk(&tmp).into_iter().find(|p| p.file_name().map(|f| f.to_string_lossy().eq_ignore_ascii_case(n)).unwrap_or(false));
        let Some(src) = found else { return Err(format!("{n} introuvable dans l'archive")) };
        let _ = std::fs::remove_file(dir.join(n));
        std::fs::rename(&src, dir.join(n)).map_err(|e| e.to_string())?;
    }
    let _ = std::fs::remove_dir_all(&tmp);
    let _ = std::fs::remove_file(zip);
    Ok(())
}

fn walk(d: &Path) -> Vec<PathBuf> {
    let mut v = vec![];
    if let Ok(rd) = std::fs::read_dir(d) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                v.extend(walk(&p));
            } else {
                v.push(p);
            }
        }
    }
    v
}

/// yt-dlp se met à jour lui-même, au plus une fois par jour (les sites changent souvent).
fn maybe_update() {
    let Some(exe) = ytdlp() else { return };
    {
        let mut st = ST.lock();
        if st.updated {
            return;
        }
        st.updated = true;
    }
    let stamp = tools_dir().join("yt-dlp.updated");
    let fresh = std::fs::metadata(&stamp).and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).map(|e| e < Duration::from_secs(20 * 3600)).unwrap_or(false);
    if fresh {
        return;
    }
    std::thread::spawn(move || {
        let out = Command::new(&exe).args(["-U", "--no-colors"]).creation_flags(NO_WINDOW).output();
        if let Ok(o) = out {
            let s = String::from_utf8_lossy(&o.stdout);
            let line = s.lines().filter(|l| !l.trim().is_empty()).last().unwrap_or("").trim().to_string();
            crate::log(&format!("téléchargeur : mise à jour yt-dlp : {line}"));
            let _ = std::fs::write(&stamp, line);
        }
    });
}

fn base_cmd() -> Result<Command, String> {
    let exe = ytdlp().ok_or("yt-dlp n'est pas installé")?;
    let mut c = Command::new(exe);
    c.creation_flags(NO_WINDOW).env("PYTHONIOENCODING", "utf-8").env("PYTHONUTF8", "1");
    c.args(["--no-colors", "--encoding", "utf-8"]);
    if let Some(d) = ffmpeg_dir() {
        c.arg("--ffmpeg-location").arg(d);
    }
    if let Some(d) = deno() {
        c.arg("--js-runtimes").arg(format!("deno:{}", d.display()));
    }
    Ok(c)
}

/* ---------------- analyse d'un lien ---------------- */

/// Sites protégés par DRM : yt-dlp ne peut rien, on évite d'attendre pour rien.
fn drm_site(url: &str) -> Option<&'static str> {
    let u = url.to_lowercase();
    let host = u.split("://").nth(1).unwrap_or(&u).split(['/', '?', '#']).next().unwrap_or("");
    let list: [(&str, &str); 12] = [
        ("netflix.", "Netflix"), ("disneyplus.", "Disney+"), ("primevideo.", "Prime Video"),
        ("music.apple.", "Apple Music"), ("tv.apple.", "Apple TV+"), ("tidal.", "Tidal"),
        ("canalplus.", "Canal+"), ("max.com", "Max"), ("dazn.", "DAZN"), ("crunchyroll.", "Crunchyroll"),
        ("molotov.", "Molotov"), ("paramountplus.", "Paramount+"),
    ];
    list.iter().find(|(d, _)| host.contains(d)).map(|(_, n)| *n)
}

pub fn probe(url: &str) -> Result<Value, String> {
    let url = url.trim();
    if !(url.starts_with("http://") || url.starts_with("https://")) || url.contains(char::is_whitespace) {
        return Err("Ce n'est pas un lien".into());
    }
    if let Some(src) = music_site(url) {
        maybe_update();
        return music_probe(src, url);
    }
    if let Some(n) = drm_site(url) {
        return Err(format!("{n} protège ses contenus (DRM) : impossible de les télécharger."));
    }
    maybe_update();
    let t0 = Instant::now();
    let mix = youtube_mix(url);
    let mut c = base_cmd()?;
    c.args(["-J", "--flat-playlist", "--no-playlist", "--no-warnings", "--socket-timeout", "15"]);
    if mix.is_some() {
        // lien vers le mix seul : il est quasi infini, on n'en lit que le début
        c.args(["--playlist-end", "100"]);
    }
    let out = c.arg(url).stdin(Stdio::null()).output().map_err(|e| e.to_string())?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).to_string();
        crate::log(&format!("téléchargeur : analyse impossible ({}) : {}", host_of(url), first_error(&err)));
        return Err(friendly(&err));
    }
    let v: Value = serde_json::from_slice(&out.stdout).map_err(|_| "Réponse illisible de yt-dlp".to_string())?;
    let mut r = summarize(&v, url);
    if let Some((vid, list)) = mix {
        // mix YouTube (playlist générée par YouTube) : on peut prendre ce titre seul ou les N premiers du mix
        let playlist = !r["playlist"].is_null();
        let vid = vid.or_else(|| (!playlist).then(|| r["id"].as_str().unwrap_or("").to_string())).filter(|x| !x.is_empty());
        let mix_url = match &vid {
            Some(v) => format!("https://www.youtube.com/watch?v={v}&list={list}"),
            None => format!("https://www.youtube.com/playlist?list={list}"),
        };
        let title = if playlist { r["title"].as_str().unwrap_or("Mix").to_string() } else { format!("Mix - {}", r["title"].as_str().unwrap_or("")) };
        r["mix"] = json!({ "url": mix_url, "title": title });
    }
    crate::log(&format!("téléchargeur : analyse {} en {} ms{}", host_of(url), t0.elapsed().as_millis(), if r["mix"].is_null() { "" } else { " (mix)" }));
    Ok(r)
}

/// Liste « déjà téléchargés » pour yt-dlp (--download-archive) : les vidéos YouTube dont le MP3 est déjà
/// dans Ma musique (reconnues au « [id] » à la fin du nom de fichier). Fichier temporaire, propre à ce téléchargement.
fn known_archive(job: u32) -> Option<PathBuf> {
    let re = regex::Regex::new(r"\[([A-Za-z0-9_-]{11})\]\.[A-Za-z0-9]+$").unwrap();
    let mut ids: Vec<String> = [music_dir(), spotify_dir()]
        .iter()
        .flat_map(|d| audio_files(d))
        .filter_map(|p| p.file_name().and_then(|n| re.captures(&n.to_string_lossy()).map(|c| c[1].to_string())))
        .collect();
    ids.sort();
    ids.dedup();
    if ids.is_empty() {
        return None;
    }
    let path = tools_dir().join(format!("deja-{job}.txt"));
    let body: String = ids.iter().map(|i| format!("youtube {i}\n")).collect();
    std::fs::write(&path, body).ok()?;
    Some(path)
}

/// Mix YouTube (liste « RD… » créée par YouTube) : renvoie (id de la vidéo si présent, id du mix).
fn youtube_mix(url: &str) -> Option<(Option<String>, String)> {
    let host = host_of(url).to_lowercase();
    if !(host.ends_with("youtube.com") || host.ends_with("youtu.be")) {
        return None;
    }
    let query = url.split_once('?')?.1.split('#').next().unwrap_or("");
    let param = |k: &str| query.split('&').find_map(|p| p.split_once('=').filter(|(a, _)| *a == k).map(|(_, b)| b.to_string()));
    let list = param("list").filter(|l| l.starts_with("RD") && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'))?;
    // youtu.be/ID?list=RD… : l'id est dans le chemin
    let vid = param("v").or_else(|| host.ends_with("youtu.be").then(|| url.split("://").nth(1).unwrap_or("").split(['/', '?']).nth(1).unwrap_or("").to_string()));
    let vid = vid.filter(|v| !v.is_empty() && v.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
    Some((vid, list))
}

fn host_of(url: &str) -> String {
    url.split("://").nth(1).unwrap_or(url).split(['/', '?', '#']).next().unwrap_or("").to_string()
}

fn s(v: &Value, k: &str) -> String {
    v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string()
}
fn f(v: &Value, k: &str) -> Option<f64> {
    v.get(k).and_then(|x| x.as_f64())
}

fn fsize(fm: &Value, dur: f64) -> u64 {
    if let Some(n) = f(fm, "filesize").or_else(|| f(fm, "filesize_approx")) {
        return n as u64;
    }
    // débit (kbit/s) × durée
    f(fm, "tbr").map(|t| (t * 125.0 * dur) as u64).unwrap_or(0)
}

fn summarize(v: &Value, url: &str) -> Value {
    // la meilleure miniature en JPEG/PNG (la liste est rangée de la moins bonne à la meilleure)
    let thumb_url = v.get("thumbnails").and_then(|a| a.as_array()).and_then(|a| {
        a.iter().rev().filter_map(|x| x.get("url").and_then(|u| u.as_str())).find(|u| {
            let path = u.split('?').next().unwrap_or(u).to_lowercase();
            path.ends_with(".jpg") || path.ends_with(".jpeg") || path.ends_with(".png")
        })
    }).map(str::to_string).unwrap_or_else(|| s(v, "thumbnail"));
    let thumb = fetch_thumb(&thumb_url);
    let author = [s(v, "uploader"), s(v, "channel"), s(v, "creator"), s(v, "playlist_uploader")].into_iter().find(|x| !x.is_empty()).unwrap_or_default();
    let page = Some(s(v, "webpage_url")).filter(|x| !x.is_empty()).unwrap_or(url.to_string());
    if s(v, "_type") == "playlist" {
        let n = v.get("entries").and_then(|e| e.as_array()).map(|a| a.len()).unwrap_or(0) as u64;
        let n = f(v, "playlist_count").map(|x| x as u64).unwrap_or(n);
        return json!({ "url": page, "title": s(v, "title"), "author": author, "thumb": thumb, "playlist": n, "site": s(v, "extractor_key"),
            "videos": [], "audio": true, "subs": null, "duration": null, "live": false, "id": s(v, "id") });
    }
    let dur = f(v, "duration").unwrap_or(0.0);
    let live = s(v, "live_status") == "is_live" || v.get("is_live").and_then(|x| x.as_bool()) == Some(true);
    let formats = v.get("formats").and_then(|a| a.as_array()).cloned().unwrap_or_default();
    let is_video = |fm: &Value| {
        let vc = s(fm, "vcodec");
        // X (GIF) : un seul mp4 sans codec ni taille annoncés
        let video_ext = matches!(s(fm, "ext").as_str(), "mp4" | "webm" | "mov" | "mkv" | "m4v" | "gif");
        vc != "none" && (f(fm, "height").unwrap_or(0.0) > 0.0 || (!vc.is_empty() && s(fm, "acodec") == "none") || (vc.is_empty() && video_ext))
    };
    let audio_best = formats.iter().filter(|fm| s(fm, "vcodec") == "none" && s(fm, "acodec") != "none")
        .map(|fm| fsize(fm, dur)).max().unwrap_or(0);
    // hauteurs disponibles, avec une taille estimée (vidéo h264 si possible + meilleur son)
    let mut heights: Vec<u32> = formats.iter().filter(|fm| is_video(fm)).filter_map(|fm| f(fm, "height").map(|h| h as u32)).filter(|h| *h >= 144).collect();
    heights.sort_unstable();
    heights.dedup();
    let mut videos = vec![];
    for (i, h) in heights.iter().rev().enumerate() {
        // on propose la meilleure, puis les paliers courants
        if i > 0 && ![2160, 1440, 1080, 720, 480, 360].contains(h) {
            continue;
        }
        let at_h: Vec<&Value> = formats.iter().filter(|fm| is_video(fm) && f(fm, "height").map(|x| x as u32) == Some(*h)).collect();
        let pick = at_h.iter().filter(|fm| s(fm, "vcodec").starts_with("avc")).max_by_key(|fm| fsize(fm, dur))
            .or_else(|| at_h.iter().max_by_key(|fm| fsize(fm, dur)));
        let size = pick.map(|fm| fsize(fm, dur) + if s(fm, "acodec") == "none" || s(fm, "acodec").is_empty() { audio_best } else { 0 }).unwrap_or(0);
        videos.push(json!({ "h": h, "size": if live { 0 } else { size } }));
        if videos.len() >= 5 {
            break;
        }
    }
    // pas de hauteur connue (Reddit, X… parfois) : une seule option « meilleure »
    let has_video = formats.iter().any(is_video) || (formats.is_empty() && s(v, "vcodec") != "none" && s(v, "ext") != "mp3");
    if videos.is_empty() && has_video {
        let size = formats.iter().filter(|fm| is_video(fm)).map(|fm| fsize(fm, dur)).max().unwrap_or(0);
        videos.push(json!({ "h": 0, "size": size }));
    }
    // GIF de X : pas de durée (les vidéos en ont une), pas de son
    let ek = s(v, "extractor_key").to_lowercase();
    let is_gif = has_video && ek.contains("twitter") && dur <= 0.0;
    // transcription : sous-titres écrits d'abord, sinon automatiques ; langue de la vidéo, sinon fr, sinon en
    let lang = s(v, "language");
    let subs = pick_subs(v.get("subtitles"), &lang, false).or_else(|| pick_subs(v.get("automatic_captions"), &lang, true));
    json!({
        "url": page, "title": s(v, "title"), "author": author, "thumb": thumb, "duration": if dur > 0.0 { Some(dur) } else { None },
        "live": live, "playlist": null, "site": s(v, "extractor_key"), "id": s(v, "id"),
        "videos": videos, "audio": !is_gif && (!formats.is_empty() || has_video || s(v, "acodec") != "none"), "is_gif": is_gif,
        "audio_size": if live { 0 } else if audio_best > 0 { audio_best } else { 0 },
        "subs": subs.map(|(l, auto)| json!({ "lang": l, "auto": auto })),
        "gif": has_video && !live,
        "track": s(v, "track"), "artist": first_artist(v), "album": s(v, "album"),
    })
}

fn first_artist(v: &Value) -> String {
    v.get("artists").and_then(|a| a.as_array()).map(|a| a.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join(", "))
        .filter(|x| !x.is_empty()).unwrap_or_else(|| s(v, "artist"))
}

fn pick_subs(subs: Option<&Value>, lang: &str, auto: bool) -> Option<(String, bool)> {
    let m = subs?.as_object()?;
    if m.is_empty() {
        return None;
    }
    let base = |k: &str| k.split(['-', '_']).next().unwrap_or(k).to_lowercase();
    let mut prefs: Vec<String> = vec![];
    if !lang.is_empty() {
        // automatiques : « xx-orig » = la vraie langue parlée (pas une traduction)
        if auto {
            prefs.push(format!("{}-orig", base(lang)));
        }
        prefs.push(lang.to_string());
        prefs.push(base(lang));
    }
    if !auto || lang.is_empty() {
        prefs.extend(["fr".into(), "en".into()]);
    }
    for p in &prefs {
        if m.contains_key(p) {
            return Some((p.clone(), auto));
        }
    }
    if auto {
        // la piste « -orig » s'il y en a une
        if let Some(k) = m.keys().find(|k| k.ends_with("-orig")) {
            return Some((k.clone(), true));
        }
        return None;
    }
    for p in ["fr", "en"] {
        if let Some(k) = m.keys().find(|k| base(k) == p) {
            return Some((k.clone(), false));
        }
    }
    m.keys().next().map(|k| (k.clone(), false))
}

fn fetch_thumb(url: &str) -> Option<String> {
    if !url.starts_with("http") {
        return None;
    }
    let r = crate::http().get(url).timeout(Duration::from_secs(6)).call().ok()?;
    let mut buf = vec![];
    r.into_reader().take(4 << 20).read_to_end(&mut buf).ok()?;
    crate::media::shrink(&buf, 320)
}

/* ---------------- erreurs ---------------- */

fn first_error(err: &str) -> String {
    let line = err.lines().find(|l| l.starts_with("ERROR:")).or_else(|| err.lines().rev().find(|l| !l.trim().is_empty())).unwrap_or("").trim();
    let line = line.trim_start_matches("ERROR:").trim();
    let line = if let Some(i) = line.find("]: ").filter(|_| line.starts_with('[')) { &line[i + 3..] } else { line };
    line.chars().take(220).collect()
}

fn friendly(err: &str) -> String {
    let e = err.to_lowercase();
    let has = |k: &str| e.contains(k);
    if has("drm") {
        "Contenu protégé (DRM) : impossible à télécharger.".into()
    } else if has("unsupported url") {
        "Ce site n'est pas pris en charge (ou le lien ne mène pas à une vidéo).".into()
    } else if has("confirm your age") || has("age-restricted") || has("age restricted") {
        "Vidéo limitée par l'âge : il faut être connecté au site.".into()
    } else if has("private video") || has("login required") || has("log in") || has("sign in") || has("login_required") || has("requires authentication") || has("cookies") {
        "Contenu privé ou réservé aux comptes connectés.".into()
    } else if has("members-only") || has("join this channel") {
        "Vidéo réservée aux membres de la chaîne.".into()
    } else if has("video unavailable") || has("has been removed") || has("not available") && !has("format") {
        "Vidéo indisponible (supprimée, privée ou bloquée dans ton pays).".into()
    } else if has("http error 429") || has("too many requests") {
        "Le site limite les téléchargements : réessaie dans quelques minutes.".into()
    } else if has("http error 403") {
        "Le site a refusé l'accès (403). Réessaie plus tard : yt-dlp se met à jour chaque jour.".into()
    } else if has("getaddrinfo") || has("unable to download webpage") && (has("timed out") || has("connection")) || has("network is unreachable") {
        "Pas de connexion au site.".into()
    } else if has("requested format is not available") {
        "Ce format n'est pas disponible pour cette vidéo.".into()
    } else if has("no space left") {
        "Plus de place sur le disque.".into()
    } else {
        let l = first_error(err);
        if l.is_empty() { "Échec du téléchargement".into() } else { l }
    }
}

/* ---------------- file d'attente ---------------- */

#[allow(clippy::too_many_arguments)]
pub fn start(app: &AppHandle, url: String, kind: String, format: Option<u32>, start: Option<f64>, end: Option<f64>, playlist: bool, info: Value) -> Result<u32, String> {
    *APP.lock() = Some(app.clone());
    if ytdlp().is_none() {
        return Err("Le téléchargeur n'est pas encore installé".into());
    }
    if !matches!(kind.as_str(), "video" | "audio" | "text" | "gif") {
        return Err("Format inconnu".into());
    }
    let music = info.get("music").and_then(|x| x.as_str()).map(str::to_string);
    let tracks: Vec<MTrack> = if music.is_some() {
        info.get("tracks").cloned().and_then(|t| serde_json::from_value(t).ok()).unwrap_or_default()
    } else if kind == "audio" && !playlist {
        // le titre d'un MP3 YouTube, pour pouvoir l'ajouter aux Titres likés
        let g = |k: &str| info.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
        let title = Some(g("track")).filter(|x| !x.is_empty()).unwrap_or_else(|| g("title"));
        let artist = Some(g("artist")).filter(|x| !x.is_empty()).unwrap_or_else(|| g("author"));
        vec![MTrack { title, artist, album: g("album"), duration: info.get("duration").and_then(|x| x.as_f64()), ..Default::default() }]
    } else {
        vec![]
    };
    if music.is_some() && tracks.is_empty() {
        return Err("Aucun titre dans ce lien".into());
    }
    if kind == "gif" {
        let dur = info.get("duration").and_then(|x| x.as_f64()).unwrap_or(0.0);
        let len = end.unwrap_or(if dur > 0.0 { dur } else { 0.0 }) - start.unwrap_or(0.0);
        if len > GIF_MAX + 0.5 {
            return Err(format!("Un GIF dure {} s maximum : choisis un extrait", GIF_MAX as u32));
        }
    }
    let subs = info.get("subs").filter(|x| !x.is_null());
    if kind == "text" && subs.is_none() {
        return Err("Pas de sous-titres ni de transcription pour cette vidéo".into());
    }
    let live = info.get("live").and_then(|x| x.as_bool()).unwrap_or(false);
    let expect = if kind == "audio" {
        info.get("audio_size").and_then(|x| x.as_u64()).unwrap_or(0)
    } else {
        info.get("videos").and_then(|a| a.as_array()).and_then(|a| a.iter().find(|x| format.map(|h| x["h"].as_u64() == Some(h as u64)).unwrap_or(true))).and_then(|x| x["size"].as_u64()).unwrap_or(0)
    };
    // un extrait : taille au prorata
    let dur = info.get("duration").and_then(|x| x.as_f64()).unwrap_or(0.0);
    let expect = match (start, end) {
        (s0, e0) if dur > 0.0 && (s0.is_some() || e0.is_some()) => {
            let len = e0.unwrap_or(dur).min(dur) - s0.unwrap_or(0.0).max(0.0);
            (expect as f64 * (len / dur).clamp(0.0, 1.0)) as u64
        }
        _ => expect,
    };
    let label = match kind.as_str() {
        "audio" if tracks.len() > 1 && music.is_some() => format!("MP3 · {} titres", tracks.len()),
        "audio" => "MP3".to_string(),
        "gif" => "GIF".to_string(),
        "text" => "Transcription".to_string(),
        _ => match format {
            Some(h) if h > 0 => format!("{h}p"),
            _ => "Vidéo".to_string(),
        },
    };
    let label = if start.is_some() || end.is_some() { format!("{label} · extrait") } else { label };
    let label = match info["limit"].as_u64() {
        Some(n) if playlist => format!("{label} · {n} titres du mix"),
        _ => label,
    };
    let mut st = ST.lock();
    st.seq += 1;
    let id = st.seq;
    st.jobs.push(Job {
        id,
        url,
        title: info["title"].as_str().unwrap_or("").to_string(),
        author: info["author"].as_str().unwrap_or("").to_string(),
        thumb: info["thumb"].as_str().map(str::to_string),
        kind,
        label,
        status: "queued".into(),
        live,
        item: if playlist { info["playlist"].as_u64().map(|n| (0, n as u32)) } else if tracks.len() > 1 { Some((0, tracks.len() as u32)) } else { None },
        music: music.clone(),
        opts: Opts {
            format,
            start,
            end,
            playlist: playlist || (music.is_some() && tracks.len() > 1),
            music: music.is_some(),
            tracks,
            sub_lang: subs.and_then(|s| s["lang"].as_str()).map(str::to_string),
            sub_auto: subs.and_then(|s| s["auto"].as_bool()).unwrap_or(false),
            expect,
            vid: info["id"].as_str().unwrap_or("").to_string(),
            limit: if playlist { info["limit"].as_u64().map(|n| n.clamp(1, 500) as u32) } else { None },
        },
        ..Default::default()
    });
    // on garde les 100 derniers
    let n = st.jobs.len();
    if n > 100 {
        let mut drop_n = n - 100;
        st.jobs.retain(|j| {
            if drop_n > 0 && matches!(j.status.as_str(), "done" | "error" | "stopped") {
                drop_n -= 1;
                false
            } else {
                true
            }
        });
    }
    drop(st);
    emit_jobs();
    pump();
    Ok(id)
}

pub fn jobs() -> Vec<Job> {
    ST.lock().jobs.clone()
}

fn emit_jobs() {
    if let Some(app) = APP.lock().clone() {
        let _ = app.emit("grab", jobs());
    }
}

fn with_job(id: u32, f: impl FnOnce(&mut Job)) {
    if let Some(j) = ST.lock().jobs.iter_mut().find(|j| j.id == id) {
        f(j);
    }
}

/// Lance les téléchargements en attente (2 à la fois).
fn pump() {
    let next = {
        let mut st = ST.lock();
        let running = st.jobs.iter().filter(|j| matches!(j.status.as_str(), "running" | "post")).count();
        if running >= MAX_RUNNING {
            return;
        }
        let Some(j) = st.jobs.iter_mut().find(|j| j.status == "queued") else { return };
        j.status = "running".into();
        j.clone()
    };
    std::thread::spawn(move || {
        let id = next.id;
        let r = run(next);
        with_job(id, |j| {
            j.pid = None;
            j.speed = 0;
            j.eta = None;
            match r {
                Ok(()) => {
                    if j.status != "stopped" {
                        j.status = "done".into();
                        j.pct = Some(1.0);
                    }
                }
                Err(e) => {
                    if j.status != "stopped" {
                        j.status = "error".into();
                        j.error = Some(e);
                    }
                }
            }
        });
        with_job(id, |j| {
            j.at = now_ms();
            j.cur = None;
        });
        save_history();
        let done = jobs().into_iter().find(|j| j.id == id);
        if let (Some(j), Some(app)) = (done, APP.lock().clone()) {
            crate::log(&format!("téléchargeur : {} {} ({})", j.status, j.label, host_of(&j.url)));
            let _ = app.emit("grab-done", &j);
        }
        emit_jobs();
        pump();
    });
    // un deuxième peut partir aussi
    pump_later();
}

fn pump_later() {
    std::thread::spawn(|| {
        std::thread::sleep(Duration::from_millis(50));
        pump();
    });
}

fn run(job: Job) -> Result<(), String> {
    if job.opts.music {
        return run_music(job);
    }
    let out = out_dir();
    std::fs::create_dir_all(&out).map_err(|e| format!("Dossier {} : {e}", out.display()))?;
    let o = &job.opts;
    let mut c = base_cmd()?;
    c.arg("-P").arg(&out);
    let tpl = if o.playlist { "%(playlist_title).80B/%(playlist_index)03d - %(title).120B [%(id)s].%(ext)s" } else { "%(title).150B [%(id)s].%(ext)s" };
    c.args(["-o", tpl, "--newline", "--progress", "--no-mtime", "--socket-timeout", "20", "--retries", "5", "--fragment-retries", "10"]);
    c.args(["--progress-template", "download:@@P %(progress.downloaded_bytes)s|%(progress.total_bytes)s|%(progress.total_bytes_estimate)s|%(progress.speed)s|%(progress.eta)s|%(info.playlist_index)s|%(info.n_entries)s"]);
    c.args(["--print", "after_move:@@F %(filepath)s"]);
    c.arg(if o.playlist { "--yes-playlist" } else { "--no-playlist" });
    if let Some(n) = o.limit {
        c.args(["--playlist-end", &n.to_string()]);
    }
    match job.kind.as_str() {
        "audio" => {
            c.args(["-f", "ba/b", "-x", "--audio-format", "mp3", "--audio-quality", "0", "--embed-metadata", "--embed-thumbnail", "--convert-thumbnails", "jpg"]);
        }
        "gif" => {
            // une vidéo sans le son, pas trop grande : elle devient un GIF ensuite
            c.args(["-f", "bv*/b", "-S", "res:720,fps"]);
        }
        "text" => {
            c.args(["--skip-download", "--sub-format", "vtt/srt/best", "--convert-subs", "srt", "--sub-langs", o.sub_lang.as_deref().unwrap_or("fr,en")]);
            c.arg(if o.sub_auto { "--write-auto-subs" } else { "--write-subs" });
        }
        _ => {
            let sort = match o.format {
                Some(h) if h > 0 => format!("res:{h},fps,vcodec:h264,acodec:aac"),
                _ => "res,fps,vcodec:h264,acodec:aac".to_string(),
            };
            c.args(["-f", "bv*+ba/b", "-S", &sort, "--merge-output-format", "mp4", "--embed-metadata"]);
        }
    }
    if job.kind != "text" && (o.start.is_some() || o.end.is_some()) {
        let a = o.start.unwrap_or(0.0).max(0.0);
        let b = o.end.map(|e| format!("{e:.2}")).unwrap_or("inf".into());
        c.args(["--download-sections", &format!("*{a:.2}-{b}"), "--force-keyframes-at-cuts"]);
    }
    if job.live {
        // un live s'enregistre jusqu'à « Arrêter » : fichier lisible même interrompu
        c.args(["--no-part", "--hls-use-mpegts"]);
    }
    // MP3 d'une playlist / d'un mix : les titres déjà dans Ma musique ne sont pas retéléchargés
    let archive = if o.playlist && job.kind == "audio" { known_archive(job.id) } else { None };
    if let Some(a) = &archive {
        c.arg("--download-archive").arg(a).arg("--verbose");
    }
    c.arg("--").arg(&job.url);
    c.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let started = SystemTime::now();
    let mut child = c.spawn().map_err(|e| e.to_string())?;
    let pid = child.id();
    with_job(job.id, |j| j.pid = Some(pid));
    emit_jobs();

    // stderr dans un fil à part (erreurs)
    let stderr = child.stderr.take().unwrap();
    let err_buf = std::sync::Arc::new(Mutex::new(String::new()));
    let eb = err_buf.clone();
    // titres sautés car déjà dans Ma musique (yt-dlp ne le dit qu'en --verbose, sur stderr)
    let skipped_n = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
    let sk = skipped_n.clone();
    let verbose = archive.is_some();
    let err_thread = std::thread::spawn(move || {
        // en octets : yt-dlp écrit stderr en cp1252 (« Île » n'y est pas de l'UTF-8). Avec .lines(), la lecture
        // s'arrêtait à la première ligne invalide, puis yt-dlp plantait en écrivant dans le tuyau fermé
        let mut rd = BufReader::new(stderr);
        let mut raw = vec![];
        loop {
            raw.clear();
            match rd.read_until(b'\n', &mut raw) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
            let line = String::from_utf8_lossy(&raw);
            let line = line.trim_end();
            if verbose {
                if line.contains("has already been recorded in the archive") {
                    sk.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
                // le bavardage de --verbose fausserait friendly() : on ne garde que les erreurs
                if !(line.starts_with("ERROR:") || line.starts_with("WARNING:")) {
                    continue;
                }
            }
            let mut b = eb.lock();
            if b.len() < 16_000 {
                b.push_str(&line);
                b.push('\n');
            }
        }
    });

    let stdout = child.stdout.take().unwrap();
    let mut files: Vec<String> = vec![];
    // octets des fichiers déjà finis (vidéo puis son, éléments d'une playlist)
    let mut finished_bytes = 0u64;
    let mut cur_total = 0u64;
    let mut cur_done = 0u64;
    let mut last_emit = Instant::now();
    let mut reader = BufReader::new(stdout);
    let mut raw = vec![];
    loop {
        raw.clear();
        match reader.read_until(b'\n', &mut raw) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        let line = String::from_utf8_lossy(&raw);
        let line = line.trim_end();
        if let Some(rest) = line.strip_prefix("@@P ") {
            let p: Vec<&str> = rest.split('|').collect();
            let num = |i: usize| p.get(i).and_then(|x| x.trim().parse::<f64>().ok());
            let done = num(0).unwrap_or(0.0) as u64;
            let total = num(1).or(num(2)).unwrap_or(0.0) as u64;
            // nouveau fichier (le compteur repart de zéro)
            if done + (1 << 16) < cur_done {
                finished_bytes += cur_total.max(cur_done);
            }
            cur_done = done;
            cur_total = total;
            let idx = num(5).map(|x| x as u32);
            // mix limité aux N premiers : la progression se compte sur N
            let n = num(6).map(|x| x as u32).map(|n| o.limit.map_or(n, |l| n.min(l))).or(o.limit);
            let expect = job.opts.expect;
            let all = finished_bytes + done;
            let pct = if o.playlist {
                match (idx, n) {
                    (Some(i), Some(n)) if n > 0 && total > 0 => Some((i.saturating_sub(1) as f64 + done as f64 / total as f64) / n as f64),
                    _ => None,
                }
            } else if expect > 0 && !job.live {
                Some((all as f64 / (expect.max(finished_bytes + total)) as f64).min(0.99))
            } else if total > 0 && !job.live {
                Some(done as f64 / total as f64)
            } else {
                None
            };
            let speed = num(3).unwrap_or(0.0) as u64;
            let eta = num(4).map(|x| x as u64);
            with_job(job.id, |j| {
                if j.status == "stopped" {
                    return;
                }
                j.status = "running".into();
                j.bytes = all;
                j.speed = speed;
                j.eta = eta;
                if pct.is_some() {
                    j.pct = pct.map(|x| x.clamp(0.0, 1.0));
                }
                if let (Some(i), Some(n)) = (idx, n) {
                    j.item = Some((i, n));
                }
            });
        } else if let Some(fp) = line.strip_prefix("@@F ") {
            files.push(fp.trim().to_string());
            with_job(job.id, |j| {
                j.files = files.len() as u32;
                j.path = Some(fp.trim().to_string());
                if o.playlist {
                    j.items = files.clone();
                }
            });
        } else if line.starts_with("[Merger]") || line.starts_with("[ExtractAudio]") || line.starts_with("[EmbedThumbnail]") || line.starts_with("[VideoRemuxer]") || line.starts_with("[FixupM3u8]") || line.starts_with("[SubtitlesConvertor]") || line.starts_with("[Metadata]") {
            with_job(job.id, |j| {
                if j.status != "stopped" {
                    j.status = "post".into();
                    j.speed = 0;
                    j.eta = None;
                }
            });
        } else if let Some(rest) = line.strip_prefix("[download] Downloading item ") {
            // « 3 of 12 »
            let mut it = rest.split(" of ");
            if let (Some(a), Some(b)) = (it.next().and_then(|x| x.trim().parse().ok()), it.next().and_then(|x| x.trim().parse().ok())) {
                with_job(job.id, |j| j.item = Some((a, b)));
            }
        }
        if last_emit.elapsed() > Duration::from_millis(350) {
            last_emit = Instant::now();
            emit_jobs();
        }
    }
    let status = child.wait().map_err(|e| e.to_string())?;
    let _ = err_thread.join();
    let err = err_buf.lock().clone();
    let skipped = skipped_n.load(std::sync::atomic::Ordering::Relaxed);
    if let Some(a) = &archive {
        let _ = std::fs::remove_file(a);
    }
    if skipped > 0 {
        crate::log(&format!("téléchargeur : {skipped} titre(s) déjà dans Ma musique, sautés"));
    }
    let dup_note = match skipped {
        0 => None,
        1 => Some("1 titre déjà dans Ma musique, sauté".to_string()),
        n => Some(format!("{n} titres déjà dans Ma musique, sautés")),
    };
    let stopped = jobs().iter().any(|j| j.id == job.id && j.status == "stopped");
    if stopped {
        if job.live {
            // live arrêté : l'enregistrement jusque-là est gardé
            let tag = format!("[{}]", job.opts.vid);
            let rec = walk(&out).into_iter()
                .filter(|p| !job.opts.vid.is_empty() && p.to_string_lossy().contains(&tag))
                .filter(|p| p.metadata().and_then(|m| m.modified()).map(|t| t >= started).unwrap_or(false))
                .max_by_key(|p| p.metadata().map(|m| m.len()).unwrap_or(0));
            if let Some(r) = rec {
                with_job(job.id, |j| {
                    j.status = "done".into();
                    j.path = Some(r.to_string_lossy().to_string());
                    j.files = 1;
                });
            }
            return Ok(());
        }
        cleanup_partial(&out, &job);
        return Ok(());
    }
    if job.kind == "text" {
        let txt = subs_to_text(&out, started, &job.opts.vid)?;
        with_job(job.id, |j| {
            j.path = Some(txt.to_string_lossy().to_string());
            j.files = 1;
        });
        return Ok(());
    }
    if files.is_empty() && skipped > 0 && status.success() {
        return Err(if skipped == 1 { "Ce titre est déjà dans Ma musique".to_string() } else { format!("Les {skipped} titres sont déjà dans Ma musique") });
    }
    if !status.success() && files.is_empty() {
        crate::log(&format!("téléchargeur : échec ({}) : {}", host_of(&job.url), first_error(&err)));
        return Err(friendly(&err));
    }
    if files.is_empty() {
        return Err(friendly(&err));
    }
    if status.success() {
        if let Some(n) = &dup_note {
            with_job(job.id, |j| j.error = Some(n.clone()));
        }
    }
    if let Some(p) = files.last() {
        let size = std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
        with_job(job.id, |j| {
            if !o.playlist {
                j.bytes = size;
            }
        });
    }
    if job.kind == "gif" {
        let src = PathBuf::from(files.last().unwrap());
        with_job(job.id, |j| {
            j.status = "post".into();
            j.speed = 0;
            j.eta = None;
        });
        emit_jobs();
        let gif = to_gif(&src)?;
        let size = std::fs::metadata(&gif).map(|m| m.len()).unwrap_or(0);
        with_job(job.id, |j| {
            j.path = Some(gif.to_string_lossy().to_string());
            j.bytes = size;
        });
        return Ok(());
    }
    if !status.success() {
        // playlist : certains éléments ont échoué
        with_job(job.id, |j| j.error = Some(format!("Certains éléments ont échoué : {}", friendly(&err))));
    }
    Ok(())
}

/// Sous-titres .srt → texte simple (.txt) : sans minutage, sans les répétitions des sous-titres automatiques.
fn subs_to_text(dir: &Path, since: SystemTime, vid: &str) -> Result<PathBuf, String> {
    let srt = std::fs::read_dir(dir).map_err(|e| e.to_string())?.flatten().map(|e| e.path())
        .filter(|p| p.extension().map(|e| e.eq_ignore_ascii_case("srt") || e.eq_ignore_ascii_case("vtt")).unwrap_or(false))
        .filter(|p| vid.is_empty() || p.to_string_lossy().contains(&format!("[{vid}]")))
        .filter(|p| p.metadata().and_then(|m| m.modified()).map(|t| t >= since - Duration::from_secs(2)).unwrap_or(false))
        .max_by_key(|p| p.metadata().and_then(|m| m.modified()).unwrap_or(SystemTime::UNIX_EPOCH))
        .ok_or("Pas de sous-titres récupérés pour cette vidéo")?;
    let raw = std::fs::read_to_string(&srt).map_err(|e| e.to_string())?;
    let tag = regex::Regex::new(r"<[^>]+>|\{\\[^}]*\}").unwrap();
    let mut lines: Vec<String> = vec![];
    for l in raw.lines() {
        let l = l.trim();
        if l.is_empty() || l.contains("-->") || l.chars().all(|c| c.is_ascii_digit()) || l == "WEBVTT" {
            continue;
        }
        let t = tag.replace_all(l, "").replace("&nbsp;", " ").replace("&amp;", "&").trim().to_string();
        if t.is_empty() || lines.last() == Some(&t) {
            continue;
        }
        // auto : la ligne précédente est souvent répétée en début de la suivante
        lines.push(t);
    }
    // phrases regroupées en paragraphes d'environ 600 caractères
    let mut out = String::new();
    let mut para = 0usize;
    for l in lines {
        if !out.is_empty() {
            if para > 600 && (out.ends_with('.') || out.ends_with('?') || out.ends_with('!')) {
                out.push_str("\n\n");
                para = 0;
            } else {
                out.push(' ');
            }
        }
        para += l.len();
        out.push_str(&l);
    }
    let stem = srt.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or("transcription".into());
    // « Titre [id].fr » → « Titre [id] (fr).txt »
    let (name, lang) = match stem.rsplit_once('.') {
        Some((a, b)) if b.len() <= 12 => (a.to_string(), Some(b.to_string())),
        _ => (stem.clone(), None),
    };
    let txt = dir.join(match lang {
        Some(l) => format!("{name} ({l}).txt"),
        None => format!("{name}.txt"),
    });
    std::fs::write(&txt, out.trim()).map_err(|e| e.to_string())?;
    let _ = std::fs::remove_file(&srt);
    Ok(txt)
}

fn cleanup_partial(dir: &Path, job: &Job) {
    if job.live || job.opts.vid.is_empty() {
        return;
    }
    let tag = format!("[{}]", job.opts.vid);
    for p in walk(dir) {
        let n = p.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        if n.contains(&tag) && (n.contains(".part") || n.ends_with(".ytdl") || n.contains(".temp.") || n.contains(".f") && n.ends_with(".part")) {
            let _ = std::fs::remove_file(&p);
        }
    }
}

pub fn stop(id: u32) {
    let mut pid = None;
    with_job(id, |j| {
        if matches!(j.status.as_str(), "queued" | "running" | "post") {
            pid = j.pid;
            j.status = "stopped".into();
            j.speed = 0;
            j.eta = None;
        }
    });
    if let Some(pid) = pid {
        let _ = Command::new("taskkill").args(["/T", "/F", "/PID", &pid.to_string()]).creation_flags(NO_WINDOW).output();
    }
    emit_jobs();
    pump();
}

/// Retire de la liste les téléchargements finis (les fichiers restent).
pub fn clear() {
    ST.lock().jobs.retain(|j| matches!(j.status.as_str(), "queued" | "running" | "post"));
    save_history();
    emit_jobs();
}

pub fn retry(app: &AppHandle, id: u32) {
    *APP.lock() = Some(app.clone());
    with_job(id, |j| {
        if matches!(j.status.as_str(), "error" | "stopped") {
            j.status = "queued".into();
            j.error = None;
            j.pct = None;
            j.bytes = 0;
            j.path = None;
            j.files = 0;
            j.items.clear();
            j.liked = None;
            j.in_music = false;
        }
    });
    emit_jobs();
    pump();
}

/// Montre le fichier dans l'explorateur (seulement un fichier de Téléchargements\Île).
pub fn reveal(id: u32) -> Result<(), String> {
    let job = jobs().into_iter().find(|j| j.id == id).ok_or("Introuvable")?;
    let out = out_dir();
    let target = job.path.map(PathBuf::from).filter(|p| p.exists() && p.starts_with(&out));
    let mut c = Command::new("explorer");
    match target {
        Some(p) if job.opts.playlist => c.arg(p.parent().unwrap_or(&out)),
        Some(p) => c.raw_arg(format!("/select,\"{}\"", p.display())),
        None => c.arg(&out),
    };
    c.spawn().map(|_| ()).map_err(|e| e.to_string())
}

pub fn open_dir() -> Result<(), String> {
    let out = out_dir();
    std::fs::create_dir_all(&out).map_err(|e| e.to_string())?;
    Command::new("explorer").arg(&out).spawn().map(|_| ()).map_err(|e| e.to_string())
}

/// Ouvre le fichier avec l'application par défaut.
pub fn open(id: u32) -> Result<(), String> {
    let job = jobs().into_iter().find(|j| j.id == id).ok_or("Introuvable")?;
    let p = job.path.map(PathBuf::from).filter(|p| p.is_file() && p.starts_with(out_dir())).ok_or("Fichier introuvable")?;
    Command::new("explorer").arg(&p).spawn().map(|_| ()).map_err(|e| e.to_string())
}

pub fn set_app(app: &AppHandle) {
    *APP.lock() = Some(app.clone());
    import_once();
}

/* ---------------- fichiers déjà présents (avant l'historique) ---------------- */

static IMPORTING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Une seule fois : ce qui est déjà dans Téléchargements\Île (téléchargé avant qu'on garde l'historique)
/// entre dans la liste. Ensuite « Effacer » reste respecté.
fn import_once() {
    if IMPORTING.swap(true, std::sync::atomic::Ordering::SeqCst) {
        return;
    }
    let marker = hist_path().with_file_name("grab-imported");
    if marker.exists() {
        return;
    }
    std::thread::spawn(move || {
        let n = import_folder();
        if let Some(d) = marker.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let _ = std::fs::write(&marker, n.to_string());
        crate::log(&format!("téléchargeur : {n} fichier(s) déjà présents ajoutés à la liste"));
        if n > 0 {
            save_history();
            emit_jobs();
        }
    });
}

const MEDIA_EXT: [&str; 11] = ["mp3", "m4a", "opus", "ogg", "wav", "mp4", "mkv", "webm", "mov", "gif", "txt"];

fn import_folder() -> usize {
    let out = out_dir();
    let known: Vec<PathBuf> = jobs().iter().filter_map(|j| j.path.as_ref().map(PathBuf::from)).collect();
    // fichiers à la racine : un par ligne ; sous-dossier (playlist, album) : une ligne par dossier
    let mut groups: Vec<(Option<PathBuf>, Vec<PathBuf>)> = vec![];
    for p in walk(&out) {
        let name = p.file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default();
        let ext = p.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
        if !MEDIA_EXT.contains(&ext.as_str()) || name.starts_with(".ile-") || name.contains(".part") || name.contains(".temp.") {
            continue;
        }
        if known.iter().any(|k| k == &p) {
            continue;
        }
        let sub = p.parent().filter(|d| *d != out.as_path()).and_then(|d| {
            // premier niveau sous Téléchargements\Île
            let rel = d.strip_prefix(&out).ok()?;
            Some(out.join(rel.components().next()?))
        });
        match sub {
            Some(d) => {
                if known.iter().any(|k| k.starts_with(&d)) {
                    continue;
                }
                match groups.iter_mut().find(|(g, _)| g.as_ref() == Some(&d)) {
                    Some((_, v)) => v.push(p),
                    None => groups.push((Some(d), vec![p])),
                }
            }
            None => groups.push((None, vec![p])),
        }
    }
    let mtime = |p: &Path| p.metadata().and_then(|m| m.modified()).ok()
        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok()).map(|d| d.as_millis() as u64).unwrap_or(0);
    let mut new_jobs: Vec<Job> = groups.into_iter().filter_map(|(dir, mut files)| {
        files.sort_by_key(|p| mtime(p));
        let last = files.last()?.clone();
        let ext = last.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
        let kind = match ext.as_str() {
            "mp3" | "m4a" | "opus" | "ogg" | "wav" => "audio",
            "gif" => "gif",
            "txt" => "text",
            _ => "video",
        };
        let stem = last.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        let id_re = regex::Regex::new(r"\s*\[([A-Za-z0-9_-]{6,})\]\s*(\([a-z-]+\))?$").unwrap();
        let vid = id_re.captures(&stem).map(|c| c[1].to_string()).unwrap_or_default();
        let clean = id_re.replace(&stem, "").trim().to_string();
        let tags = probe_tags(&last);
        let title = match &dir {
            Some(d) => d.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default(),
            None => tags.0.clone().filter(|t| !t.is_empty()).unwrap_or(clean.clone()),
        };
        let artist = tags.1.clone().unwrap_or_default();
        let thumb = if kind == "text" { None } else { file_thumb(&last, kind) };
        let bytes: u64 = files.iter().map(|p| p.metadata().map(|m| m.len()).unwrap_or(0)).sum();
        let label = match kind {
            "audio" => "MP3".to_string(),
            "gif" => "GIF".to_string(),
            "text" => "Transcription".to_string(),
            _ => tags.2.filter(|h| *h > 0).map(|h| if h >= 2160 { "4K".to_string() } else { format!("{h}p") }).unwrap_or("Vidéo".into()),
        };
        let url = if vid.len() == 11 { format!("https://www.youtube.com/watch?v={vid}") } else { String::new() };
        let tracks = if kind == "audio" && dir.is_none() {
            vec![MTrack { title: tags.0.filter(|t| !t.is_empty()).unwrap_or(clean), artist: artist.clone(), album: tags.3.unwrap_or_default(), ..Default::default() }]
        } else {
            vec![]
        };
        Some(Job {
            url, title, author: artist, thumb, kind: kind.into(), label, status: "done".into(), pct: Some(1.0), bytes,
            path: Some(last.to_string_lossy().to_string()), files: files.len() as u32, at: mtime(&last),
            opts: Opts { playlist: dir.is_some(), vid, tracks, ..Default::default() },
            ..Default::default()
        })
    }).collect();
    let n = new_jobs.len();
    if n == 0 {
        return 0;
    }
    new_jobs.sort_by_key(|j| j.at);
    let mut st = ST.lock();
    for j in new_jobs.iter_mut() {
        st.seq += 1;
        j.id = st.seq;
    }
    // plus anciens que ce qui est déjà dans la liste : on les met devant
    new_jobs.append(&mut st.jobs);
    st.jobs = new_jobs;
    n
}

/// (titre, artiste, hauteur vidéo, album) lus par ffprobe.
fn probe_tags(p: &Path) -> (Option<String>, Option<String>, Option<u32>, Option<String>) {
    let Some(probe) = ffmpeg_dir().map(|d| d.join("ffprobe.exe")) else { return (None, None, None, None) };
    let out = Command::new(probe).creation_flags(NO_WINDOW)
        .args(["-v", "error", "-select_streams", "v:0", "-show_entries", "format_tags=title,artist,album:stream=height", "-of", "json"])
        .arg(p).stdin(Stdio::null()).output();
    let Ok(out) = out else { return (None, None, None, None) };
    let v: Value = serde_json::from_slice(&out.stdout).unwrap_or(Value::Null);
    let t = &v["format"]["tags"];
    let tag = |k: &str| t.as_object().and_then(|o| o.iter().find(|(x, _)| x.eq_ignore_ascii_case(k))).and_then(|(_, x)| x.as_str()).map(str::to_string);
    let h = v["streams"].as_array().and_then(|a| a.first()).and_then(|s| s["height"].as_u64()).map(|h| h as u32);
    (tag("title"), tag("artist"), h, tag("album"))
}

/// Miniature d'un fichier : pochette intégrée (MP3) ou une image de la vidéo.
fn file_thumb(p: &Path, kind: &str) -> Option<String> {
    let ff = ffmpeg_dir()?.join("ffmpeg.exe");
    let mut c = Command::new(ff);
    c.creation_flags(NO_WINDOW).args(["-v", "error"]);
    if kind == "video" {
        c.args(["-ss", "1"]);
    }
    c.arg("-i").arg(p).args(["-an", "-frames:v", "1", "-vf", "scale='min(480,iw)':-2", "-f", "image2pipe", "-vcodec", "mjpeg", "pipe:1"]);
    let out = c.stdin(Stdio::null()).output().ok()?;
    if out.stdout.len() < 500 {
        return None;
    }
    crate::media::shrink(&out.stdout, 320)
}

/* ---------------- Spotify / Deezer → YouTube (MP3) ---------------- */

const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0 Safari/537.36";
const MAX_TRACKS: usize = 500;

fn music_site(url: &str) -> Option<&'static str> {
    let h = host_of(&url.to_lowercase());
    if h.ends_with("spotify.com") || h.ends_with("spotify.link") {
        Some("Spotify")
    } else if h.ends_with("deezer.com") || h.ends_with("deezer.page.link") || h.ends_with("dzr.page.link") {
        Some("Deezer")
    } else {
        None
    }
}

fn get_text(url: &str) -> Result<(String, String), String> {
    let r = crate::http().get(url).set("User-Agent", UA).set("Accept-Language", "fr-FR,fr;q=0.9,en;q=0.8").timeout(Duration::from_secs(15)).call().map_err(|e| match e {
        ureq::Error::Status(c, _) => format!("HTTP {c}"),
        e => e.to_string(),
    })?;
    let fin = r.get_url().to_string();
    let mut body = String::new();
    r.into_reader().take(8 << 20).read_to_string(&mut body).map_err(|e| e.to_string())?;
    Ok((fin, body))
}

fn get_json(url: &str) -> Result<Value, String> {
    let (_, body) = get_text(url)?;
    serde_json::from_str(&body).map_err(|_| "Réponse illisible".to_string())
}

/// Lien court (spotify.link, link.deezer.com…) → lien complet (redirection, sinon cherché dans la page).
fn resolve(url: &str, re: &regex::Regex) -> Result<(String, String), String> {
    let grab = |s: &str| re.captures(s).map(|c| (c[1].to_string(), c[2].to_string()));
    if let Some(r) = grab(url) {
        return Ok(r);
    }
    let (fin, body) = get_text(url)?;
    grab(&fin).or_else(|| grab(&body)).ok_or_else(|| "Lien non reconnu (titre, album ou playlist attendu)".to_string())
}

fn music_probe(src: &str, url: &str) -> Result<Value, String> {
    let t0 = Instant::now();
    let r = if src == "Spotify" { spotify_probe(url) } else { deezer_probe(url) };
    match &r {
        Ok(v) => crate::log(&format!("téléchargeur : {src} {} titres en {} ms", v["count"], t0.elapsed().as_millis())),
        Err(e) => crate::log(&format!("téléchargeur : {src} illisible : {e}")),
    }
    r
}

fn music_summary(src: &str, kind: &str, id: &str, url: String, title: String, author: String, cover: Option<String>, mut tracks: Vec<MTrack>) -> Result<Value, String> {
    tracks.retain(|t| !t.title.trim().is_empty());
    tracks.truncate(MAX_TRACKS);
    if tracks.is_empty() {
        return Err("Aucun titre trouvé dans ce lien".into());
    }
    let thumb = cover.as_deref().and_then(fetch_thumb);
    let dur: f64 = tracks.iter().filter_map(|t| t.duration).sum();
    Ok(json!({
        "url": url, "title": title, "author": author, "thumb": thumb, "music": src, "mkind": kind,
        "count": tracks.len(), "tracks": tracks, "duration": if dur > 0.0 { Some(dur) } else { None },
        "playlist": null, "videos": [], "audio": true, "subs": null, "live": false, "id": id, "site": src,
    }))
}

fn best_image(v: &Value) -> Option<String> {
    v.as_array()?.iter()
        .max_by_key(|i| i["maxWidth"].as_u64().or(i["width"].as_u64()).unwrap_or(0))
        .and_then(|i| i["url"].as_str()).map(str::to_string)
}

fn spotify_probe(url: &str) -> Result<Value, String> {
    let re = regex::Regex::new(r"https?://open\.spotify\.com/(?:intl-[a-zA-Z-]+/)?(?:embed/)?(track|album|playlist)/([A-Za-z0-9]{22})").unwrap();
    let (kind, id) = resolve(url, &re)?;
    // la page « embed » est publique : nom, artistes, durées et identifiants des titres
    let (_, html) = get_text(&format!("https://open.spotify.com/embed/{kind}/{id}"))?;
    let data = html.split("<script id=\"__NEXT_DATA__\" type=\"application/json\">").nth(1).and_then(|x| x.split("</script>").next())
        .ok_or("Page Spotify illisible")?;
    let v: Value = serde_json::from_str(data).map_err(|_| "Page Spotify illisible".to_string())?;
    let e = &v["props"]["pageProps"]["state"]["data"]["entity"];
    if e.is_null() {
        return Err("Contenu Spotify introuvable (privé ou supprimé ?)".into());
    }
    let name = e["name"].as_str().or(e["title"].as_str()).unwrap_or("").to_string();
    let cover = best_image(&e["visualIdentity"]["image"]).or_else(|| best_image(&e["coverArt"]["sources"]));
    let arts = |a: &Value| a.as_array().map(|a| a.iter().filter_map(|x| x["name"].as_str()).collect::<Vec<_>>().join(", ")).unwrap_or_default();
    let sid = |uri: &str| uri.rsplit(':').next().map(str::to_string);
    let mut author = e["subtitle"].as_str().map(str::to_string).unwrap_or_else(|| arts(&e["artists"]));
    let mut tracks: Vec<MTrack> = if kind == "track" {
        vec![MTrack {
            title: name.clone(), artist: author.clone(), duration: e["duration"].as_f64().map(|d| d / 1000.0),
            cover: cover.clone(), spotify: Some(id.clone()), ..Default::default()
        }]
    } else {
        e["trackList"].as_array().map(|l| l.iter().map(|t| MTrack {
            title: t["title"].as_str().unwrap_or("").to_string(),
            artist: t["subtitle"].as_str().unwrap_or("").replace('\u{a0}', " "),
            album: if kind == "album" { name.clone() } else { String::new() },
            duration: t["duration"].as_f64().map(|d| d / 1000.0),
            cover: if kind == "album" { cover.clone() } else { None },
            spotify: t["uri"].as_str().and_then(sid),
            ..Default::default()
        }).collect()).unwrap_or_default()
    };
    // grande playlist : la page n'en montre qu'une partie ; l'API (si Spotify est connecté) donne tout
    if kind == "playlist" && tracks.len() >= 50 && crate::spotify::is_connected() {
        if let Ok(all) = spotify_playlist_api(&id) {
            if all.len() > tracks.len() {
                tracks = all;
            }
        }
    }
    if kind == "playlist" {
        author = if author.is_empty() { "Playlist".into() } else { author };
    }
    let page = format!("https://open.spotify.com/{kind}/{id}");
    music_summary("Spotify", &kind, &id, page, name, author, cover, tracks)
}

fn spotify_playlist_api(id: &str) -> Result<Vec<MTrack>, String> {
    let mut out = vec![];
    let mut next = Some(format!("/playlists/{id}/tracks?limit=100&additional_types=track"));
    while let Some(p) = next.take() {
        let v = crate::spotify::api_get(&p)?;
        for it in v["items"].as_array().cloned().unwrap_or_default() {
            let t = &it["track"];
            if t.is_null() || t["id"].is_null() {
                continue;
            }
            out.push(MTrack {
                title: t["name"].as_str().unwrap_or("").to_string(),
                artist: t["artists"].as_array().map(|a| a.iter().filter_map(|x| x["name"].as_str()).collect::<Vec<_>>().join(", ")).unwrap_or_default(),
                album: t["album"]["name"].as_str().unwrap_or("").to_string(),
                isrc: t["external_ids"]["isrc"].as_str().map(str::to_string),
                duration: t["duration_ms"].as_f64().map(|d| d / 1000.0),
                cover: t["album"]["images"].as_array().and_then(|a| a.first()).and_then(|i| i["url"].as_str()).map(str::to_string),
                spotify: t["id"].as_str().map(str::to_string),
            });
        }
        if out.len() >= MAX_TRACKS {
            break;
        }
        next = v["next"].as_str().map(str::to_string);
    }
    Ok(out)
}

fn deezer_track(t: &Value, album: Option<(&str, &str)>) -> MTrack {
    MTrack {
        title: t["title"].as_str().unwrap_or("").to_string(),
        artist: t["artist"]["name"].as_str().unwrap_or("").to_string(),
        album: album.map(|a| a.0).or(t["album"]["title"].as_str()).unwrap_or("").to_string(),
        isrc: t["isrc"].as_str().map(str::to_string),
        duration: t["duration"].as_f64(),
        cover: album.map(|a| a.1).or(t["album"]["cover_xl"].as_str()).filter(|x| !x.is_empty()).map(str::to_string),
        spotify: None,
    }
}

fn deezer_list(first: &str) -> Result<Vec<Value>, String> {
    let mut out = vec![];
    let mut next = Some(first.to_string());
    while let Some(u) = next.take() {
        let v = get_json(&u)?;
        out.extend(v["data"].as_array().cloned().unwrap_or_default());
        if out.len() >= MAX_TRACKS {
            break;
        }
        next = v["next"].as_str().map(str::to_string);
    }
    Ok(out)
}

fn deezer_probe(url: &str) -> Result<Value, String> {
    let re = regex::Regex::new(r"https?://(?:www\.)?deezer\.com/(?:[a-z]{2}(?:-[a-z]{2})?/)?(track|album|playlist)/(\d+)").unwrap();
    let (kind, id) = resolve(url, &re)?;
    let api = "https://api.deezer.com";
    let v = get_json(&format!("{api}/{kind}/{id}"))?;
    if let Some(e) = v.get("error") {
        crate::log(&format!("téléchargeur : Deezer : {}", e["message"].as_str().unwrap_or("?")));
        return Err("Contenu Deezer introuvable (privé ou supprimé ?)".into());
    }
    let page = format!("https://www.deezer.com/{kind}/{id}");
    match kind.as_str() {
        "track" => {
            let t = deezer_track(&v, None);
            music_summary("Deezer", &kind, &id, page, t.title.clone(), t.artist.clone(), t.cover.clone(), vec![t])
        }
        "album" => {
            let title = v["title"].as_str().unwrap_or("").to_string();
            let cover = v["cover_xl"].as_str().unwrap_or("").to_string();
            let list = deezer_list(&format!("{api}/album/{id}/tracks?limit=100"))?;
            let tracks = list.iter().map(|t| deezer_track(t, Some((&title, &cover)))).collect();
            music_summary("Deezer", &kind, &id, page, title.clone(), v["artist"]["name"].as_str().unwrap_or("").to_string(), Some(cover).filter(|c| !c.is_empty()), tracks)
        }
        _ => {
            let list = deezer_list(&format!("{api}/playlist/{id}/tracks?limit=100"))?;
            let tracks = list.iter().map(|t| deezer_track(t, None)).collect();
            music_summary("Deezer", &kind, &id, page, v["title"].as_str().unwrap_or("").to_string(),
                v["creator"]["name"].as_str().unwrap_or("Playlist").to_string(), v["picture_xl"].as_str().map(str::to_string), tracks)
        }
    }
}

/// Nom de fichier Windows valide.
fn clean_name(s: &str, max: usize) -> String {
    let t: String = s.chars().map(|c| if "<>:\"/\\|?*".contains(c) || c.is_control() { ' ' } else { c }).collect();
    let t = t.split_whitespace().collect::<Vec<_>>().join(" ");
    let t: String = t.chars().take(max).collect();
    let t = t.trim_end_matches(['.', ' ']).to_string();
    if t.is_empty() { "Sans titre".into() } else { t }
}

/// « Get Lucky (feat. Pharrell Williams) - Remastered 2013 » → « Get Lucky »
fn base_title(s: &str) -> String {
    static RE: Lazy<regex::Regex> = Lazy::new(|| regex::Regex::new(r"(?i)\s*[(\[][^)\]]*\b(feat|ft|with|avec|remaster(ed)?|version|edit|mono|stereo|bonus)\b[^)\]]*[)\]]|\s+-\s+[^-]*\b(remaster(ed)?|version|edit|mono|stereo)\b.*$").unwrap());
    let t = RE.replace_all(s, "").trim().to_string();
    if t.is_empty() { s.to_string() } else { t }
}

fn words(s: &str) -> Vec<String> {
    s.to_lowercase().chars().map(|c| if c.is_alphanumeric() { c } else { ' ' }).collect::<String>()
        .split_whitespace().map(str::to_string).collect()
}

const STOP: [&str; 9] = ["the", "a", "feat", "ft", "with", "and", "et", "x", "de"];
const BAD: [&str; 22] = ["live", "cover", "remix", "nightcore", "sped", "slowed", "reverb", "8d", "karaoke", "instrumental", "acoustic", "acapella",
    "reaction", "tutorial", "lesson", "extended", "loop", "hour", "hours", "heure", "piano", "tiktok"];

/// La meilleure vidéo YouTube pour ce titre (durée proche, chaîne « - Topic » ou de l'artiste, pas de remix/live…).
fn yt_find(t: &MTrack) -> Result<String, String> {
    let first_artist = t.artist.split(',').next().unwrap_or("").trim().to_string();
    let q = if first_artist.is_empty() { t.title.clone() } else { format!("{first_artist} - {}", t.title) };
    let out = base_cmd()?
        .args(["--flat-playlist", "-J", "--no-warnings", "--socket-timeout", "15"])
        .arg(format!("ytsearch8:{q}"))
        .stdin(Stdio::null())
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(friendly(&String::from_utf8_lossy(&out.stderr)));
    }
    let v: Value = serde_json::from_slice(&out.stdout).map_err(|_| "Recherche YouTube illisible".to_string())?;
    let src_t: Vec<String> = words(&base_title(&t.title)).into_iter().filter(|w| !STOP.contains(&w.as_str())).collect();
    let src_all: Vec<String> = words(&format!("{} {}", t.title, t.artist));
    let art_w: Vec<String> = words(&first_artist);
    let mut best: Option<(f64, String)> = None;
    for e in v["entries"].as_array().cloned().unwrap_or_default() {
        let Some(u) = e["url"].as_str().or(e["webpage_url"].as_str()).map(str::to_string) else { continue };
        let title = e["title"].as_str().unwrap_or("");
        let chan = e["channel"].as_str().or(e["uploader"].as_str()).unwrap_or("").to_lowercase();
        let tw = words(title);
        let cw = words(&chan);
        let hit = src_t.iter().filter(|w| tw.contains(w)).count() as f64 / src_t.len().max(1) as f64;
        let mut score = hit * 3.0;
        if !art_w.is_empty() && art_w.iter().all(|w| tw.contains(w) || cw.contains(w)) {
            score += 1.5;
        }
        if chan.ends_with(" - topic") {
            score += 1.5;
        }
        let tl = title.to_lowercase();
        if tl.contains("official audio") || tl.contains("audio officiel") || tl.contains("(audio)") {
            score += 0.5;
        }
        for b in BAD {
            if tw.iter().any(|w| w == b) && !src_all.iter().any(|w| w == b) {
                score -= 2.5;
            }
        }
        match (t.duration, e["duration"].as_f64()) {
            (Some(d), Some(ed)) => {
                let diff = (d - ed).abs();
                score += if diff <= 3.0 { 2.0 } else if diff <= 8.0 { 1.0 } else if diff <= 20.0 { 0.0 } else { -(diff / 30.0).min(4.0) };
            }
            (Some(_), None) => score -= 0.5,
            _ => {}
        }
        score += (e["view_count"].as_f64().unwrap_or(0.0) + 1.0).ln() / 40.0;
        if best.as_ref().map(|(s, _)| score > *s).unwrap_or(true) {
            best = Some((score, u));
        }
    }
    match best {
        Some((s, u)) if s >= 2.5 => Ok(u),
        _ => Err("Introuvable sur YouTube".into()),
    }
}

fn is_stopped(id: u32) -> bool {
    jobs().iter().any(|j| j.id == id && j.status == "stopped")
}

fn run_music(job: Job) -> Result<(), String> {
    let out = out_dir();
    let n = job.opts.tracks.len();
    let dir = if n > 1 { out.join(clean_name(&job.title, 80)) } else { out.clone() };
    std::fs::create_dir_all(&dir).map_err(|e| format!("Dossier {} : {e}", dir.display()))?;
    let mut ok = 0u32;
    let mut missing: Vec<String> = vec![];
    let mut last_err = String::new();
    let mut total_bytes = 0u64;
    for (i, t) in job.opts.tracks.iter().enumerate() {
        if is_stopped(job.id) {
            break;
        }
        let name = if t.artist.is_empty() { t.title.clone() } else { format!("{} - {}", t.artist.split(',').next().unwrap_or("").trim(), t.title) };
        let dest = dir.join(format!("{}.mp3", clean_name(&name, 150)));
        with_job(job.id, |j| {
            if j.status == "stopped" {
                return;
            }
            j.status = "running".into();
            j.cur = Some(t.title.clone());
            j.pct = Some(i as f64 / n as f64);
            if n > 1 {
                j.item = Some((i as u32 + 1, n as u32));
            }
        });
        emit_jobs();
        if !dest.is_file() {
            if let Err(e) = music_one(&job, t, &dest, i, n, total_bytes) {
                if is_stopped(job.id) {
                    break;
                }
                crate::log(&format!("téléchargeur : titre {}/{n} raté : {e}", i + 1));
                missing.push(t.title.clone());
                last_err = e;
                continue;
            }
        }
        ok += 1;
        total_bytes += std::fs::metadata(&dest).map(|m| m.len()).unwrap_or(0);
        with_job(job.id, |j| {
            j.files = ok;
            j.bytes = total_bytes;
            j.path = Some(dest.to_string_lossy().to_string());
        });
    }
    // fichiers temporaires d'un titre interrompu
    let tag = format!(".ile-{}-", job.id);
    for p in walk(&dir) {
        if p.file_name().map(|f| f.to_string_lossy().starts_with(&tag)).unwrap_or(false) {
            let _ = std::fs::remove_file(p);
        }
    }
    if is_stopped(job.id) {
        return Ok(());
    }
    if ok == 0 {
        return Err(if n == 1 { last_err } else { "Aucun titre trouvé sur YouTube".into() });
    }
    if !missing.is_empty() {
        let e = if missing.len() == 1 { format!("Introuvable : {}", missing[0]) } else { format!("{} titres introuvables", missing.len()) };
        with_job(job.id, |j| j.error = Some(e));
    }
    Ok(())
}

fn music_one(job: &Job, t: &MTrack, dest: &Path, i: usize, n: usize, before: u64) -> Result<(), String> {
    let url = yt_find(t)?;
    let dir = dest.parent().unwrap_or(Path::new("."));
    let stem = format!(".ile-{}-{}", job.id, i);
    let mut c = base_cmd()?;
    c.arg("-P").arg(dir);
    c.args(["-o", &format!("{stem}.%(ext)s"), "-f", "ba/b", "-x", "--audio-format", "mp3", "--audio-quality", "0", "--no-playlist",
        "--newline", "--progress", "--no-mtime", "--socket-timeout", "20", "--retries", "5", "--embed-thumbnail", "--convert-thumbnails", "jpg"]);
    c.args(["--progress-template", "download:@@P %(progress.downloaded_bytes)s|%(progress.total_bytes)s|%(progress.total_bytes_estimate)s|%(progress.speed)s|%(progress.eta)s"]);
    c.args(["--print", "after_move:@@F %(filepath)s"]);
    c.arg("--").arg(&url);
    c.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = c.spawn().map_err(|e| e.to_string())?;
    with_job(job.id, |j| j.pid = Some(child.id()));
    let stderr = child.stderr.take().unwrap();
    let err_thread = std::thread::spawn(move || {
        // tout lire (en octets) : s'arrêter avant la fin fermerait le tuyau et ferait planter yt-dlp
        let mut b = vec![];
        let _ = BufReader::new(stderr).read_to_end(&mut b);
        b.truncate(16_000);
        String::from_utf8_lossy(&b).into_owned()
    });
    let mut file: Option<String> = None;
    let mut last_emit = Instant::now();
    let mut rd = BufReader::new(child.stdout.take().unwrap());
    let mut raw = vec![];
    loop {
        raw.clear();
        match rd.read_until(b'\n', &mut raw) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        let line = String::from_utf8_lossy(&raw).trim_end().to_string();
        if let Some(rest) = line.strip_prefix("@@P ") {
            let p: Vec<&str> = rest.split('|').collect();
            let num = |k: usize| p.get(k).and_then(|x| x.trim().parse::<f64>().ok());
            let done = num(0).unwrap_or(0.0);
            let total = num(1).or(num(2)).unwrap_or(0.0);
            let frac = if total > 0.0 { (done / total).min(1.0) } else { 0.0 };
            with_job(job.id, |j| {
                if j.status == "stopped" {
                    return;
                }
                j.pct = Some((i as f64 + frac * 0.9) / n as f64);
                j.bytes = before + done as u64;
                j.speed = num(3).unwrap_or(0.0) as u64;
                j.eta = if n == 1 { num(4).map(|x| x as u64) } else { None };
            });
            if last_emit.elapsed() > Duration::from_millis(350) {
                last_emit = Instant::now();
                emit_jobs();
            }
        } else if let Some(fp) = line.strip_prefix("@@F ") {
            file = Some(fp.trim().to_string());
        }
    }
    let status = child.wait().map_err(|e| e.to_string())?;
    let err = err_thread.join().unwrap_or_default();
    with_job(job.id, |j| {
        j.pid = None;
        j.speed = 0;
    });
    let file = file.map(PathBuf::from).filter(|p| p.is_file());
    let Some(file) = file else {
        return Err(if status.success() { "Fichier introuvable".into() } else { friendly(&err) });
    };
    tag_mp3(&file, t, dest);
    Ok(())
}

/// Pochette de secours (et album) : recherche Deezer, publique.
fn deezer_cover(t: &MTrack) -> Option<(String, String)> {
    let a = t.artist.split(',').next().unwrap_or("").trim();
    let q = format!("{a} {}", base_title(&t.title));
    let v = get_json(&format!("https://api.deezer.com/search?limit=8&q={}", crate::spotify::enc(&q))).ok()?;
    let tw = words(&base_title(&t.title));
    let aw = words(a);
    let src = words(&t.title);
    v["data"].as_array()?.iter().find(|x| {
        let w = words(x["title"].as_str().unwrap_or(""));
        let xa = words(x["artist"]["name"].as_str().unwrap_or(""));
        tw.iter().filter(|k| w.contains(k)).count() * 2 >= tw.len().max(1)
            && aw.iter().all(|k| xa.contains(k))
            && !BAD.iter().any(|b| w.iter().any(|k| k == b) && !src.iter().any(|k| k == b))
    }).and_then(|x| Some((x["album"]["cover_xl"].as_str()?.to_string(), x["album"]["title"].as_str().unwrap_or("").to_string())))
}

/// Remplace les infos YouTube par les vraies (titre, artiste, album, pochette carrée), puis renomme.
fn tag_mp3(src: &Path, t: &MTrack, dest: &Path) {
    let ff = ffmpeg_dir().map(|d| d.join("ffmpeg.exe"));
    let mut album = t.album.clone();
    let mut cover_url = t.cover.clone();
    if cover_url.is_none() {
        if let Some((c, a)) = deezer_cover(t) {
            cover_url = Some(c);
            if album.is_empty() {
                album = a;
            }
        }
    }
    let cover_path = src.with_extension("cover.jpg");
    let has_cover = cover_url.as_deref().and_then(|u| {
        let r = crate::http().get(u).timeout(Duration::from_secs(10)).call().ok()?;
        let mut b = vec![];
        r.into_reader().take(8 << 20).read_to_end(&mut b).ok()?;
        (b.len() > 1000 && b.starts_with(&[0xFF, 0xD8])).then_some(b)
    }).map(|b| std::fs::write(&cover_path, b).is_ok()).unwrap_or(false);
    let tmp = src.with_extension("tag.mp3");
    let ok = ff.map(|ff| {
        let mut c = Command::new(ff);
        c.creation_flags(NO_WINDOW).args(["-y", "-v", "error", "-i"]).arg(src);
        if has_cover {
            c.arg("-i").arg(&cover_path).args(["-map", "0:a", "-map", "1:v", "-disposition:v", "attached_pic", "-metadata:s:v", "title=Album cover", "-metadata:s:v", "comment=Cover (front)"]);
        } else {
            c.args(["-map", "0"]);
        }
        c.args(["-c", "copy", "-map_metadata", "-1", "-id3v2_version", "3"]);
        c.arg("-metadata").arg(format!("title={}", t.title));
        c.arg("-metadata").arg(format!("artist={}", t.artist));
        if !album.is_empty() {
            c.arg("-metadata").arg(format!("album={album}"));
        }
        c.arg(&tmp);
        c.stdin(Stdio::null()).output().map(|o| o.status.success()).unwrap_or(false)
    }).unwrap_or(false);
    let _ = std::fs::remove_file(&cover_path);
    if ok && tmp.is_file() {
        let _ = std::fs::remove_file(src);
        let _ = std::fs::rename(&tmp, dest);
    } else {
        let _ = std::fs::remove_file(&tmp);
        let _ = std::fs::rename(src, dest);
    }
}

/* ---------------- GIF ---------------- */

/// Vidéo → GIF (480 px de large max, 15 images/s, palette calculée pour de belles couleurs).
fn to_gif(src: &Path) -> Result<PathBuf, String> {
    if src.extension().map(|e| e.eq_ignore_ascii_case("gif")).unwrap_or(false) {
        return Ok(src.to_path_buf());
    }
    let ff = ffmpeg_dir().ok_or("ffmpeg manquant")?.join("ffmpeg.exe");
    let gif = src.with_extension("gif");
    let out = Command::new(ff)
        .creation_flags(NO_WINDOW)
        .args(["-y", "-v", "error", "-i"])
        .arg(src)
        .args(["-an", "-vf", "fps=15,scale='min(480,iw)':-2:flags=lanczos,split[s0][s1];[s0]palettegen=stats_mode=diff[p];[s1][p]paletteuse=dither=bayer:bayer_scale=4:diff_mode=rectangle", "-loop", "0"])
        .arg(&gif)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() || !gif.is_file() {
        let _ = std::fs::remove_file(&gif);
        return Err(format!("Conversion en GIF impossible : {}", String::from_utf8_lossy(&out.stderr).lines().next().unwrap_or("").chars().take(160).collect::<String>()));
    }
    let _ = std::fs::remove_file(src);
    Ok(gif)
}

/* ---------------- Spotify : fichiers locaux ---------------- */

/// Dossier « Musique » de Windows.
fn music_root() -> PathBuf {
    use windows::Win32::System::Com::CoTaskMemFree;
    use windows::Win32::UI::Shell::{FOLDERID_Music, SHGetKnownFolderPath, KF_FLAG_DEFAULT};
    let music = unsafe {
        SHGetKnownFolderPath(&FOLDERID_Music, KF_FLAG_DEFAULT, None).ok().and_then(|p| {
            let s = p.to_string().ok();
            CoTaskMemFree(Some(p.0 as _));
            s.map(PathBuf::from)
        })
    };
    music.unwrap_or_else(|| PathBuf::from(std::env::var("USERPROFILE").unwrap_or_default()).join("Music"))
}

/// Dossier « Musique\Spotify (Île) » : Spotify lit les MP3 qui s'y trouvent (fichiers locaux).
pub fn spotify_dir() -> PathBuf {
    music_root().join("Spotify (Île)")
}

/// Dossier « Musique\Ma musique » : les MP3 ajoutés au lecteur de l'île.
pub fn music_dir() -> PathBuf {
    music_root().join("Ma musique")
}

/// Envoie des fichiers / dossiers à la Corbeille (récupérables), sans fenêtre de confirmation.
/// Quelques essais : un fichier encore ouvert par un lecteur se libère en général en une fraction de seconde.
pub fn to_recycle_bin(paths: &[PathBuf]) -> Result<(), String> {
    use windows::Win32::UI::Shell::{SHFileOperationW, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT, FO_DELETE, SHFILEOPSTRUCTW};
    let paths: Vec<&PathBuf> = paths.iter().filter(|p| p.exists()).collect();
    if paths.is_empty() {
        return Ok(());
    }
    // liste de chemins séparés par \0, terminée par \0\0
    let mut from: Vec<u16> = vec![];
    for p in &paths {
        from.extend(p.as_os_str().to_string_lossy().encode_utf16());
        from.push(0);
    }
    from.push(0);
    let mut last = 0;
    for attempt in 0..4 {
        if attempt > 0 {
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
        let mut op = SHFILEOPSTRUCTW {
            wFunc: FO_DELETE,
            pFrom: windows::core::PCWSTR(from.as_ptr()),
            fFlags: (FOF_ALLOWUNDO.0 | FOF_NOCONFIRMATION.0 | FOF_SILENT.0 | FOF_NOERRORUI.0) as u16,
            ..Default::default()
        };
        last = unsafe { SHFileOperationW(&mut op) };
        if last == 0 && !op.fAnyOperationsAborted.as_bool() && paths.iter().all(|p| !p.exists()) {
            return Ok(());
        }
    }
    Err(format!("Suppression impossible (fichier utilisé ? code {last})"))
}

/// Supprime un téléchargement : ses fichiers vont à la Corbeille et il quitte la liste.
pub fn remove(id: u32) -> Result<(), String> {
    let job = jobs().into_iter().find(|j| j.id == id).ok_or("Introuvable")?;
    if matches!(job.status.as_str(), "queued" | "running" | "post") {
        return Err("Téléchargement en cours : annule-le d'abord".into());
    }
    let out = out_dir();
    // uniquement ce que l'île a téléchargé, dans son dossier (tous les titres d'un mix)
    let paths: Vec<PathBuf> = if job.items.is_empty() { job.path.iter().map(PathBuf::from).collect() } else { job.items.iter().map(PathBuf::from).collect() };
    let paths: Vec<PathBuf> = paths.into_iter().filter(|p| p.starts_with(&out) && p != &out && p.exists()).collect();
    if !paths.is_empty() {
        to_recycle_bin(&paths)?;
    }
    ST.lock().jobs.retain(|j| j.id != id);
    save_history();
    emit_jobs();
    crate::log("téléchargeur : un téléchargement supprimé (Corbeille)");
    Ok(())
}

/* ---------------- envoi vers le tel (LocalSend) ---------------- */

/// numéro de l'envoi en cours par téléchargement : annuler (ou relancer) change le numéro,
/// l'ancien envoi le voit et s'arrête
static PHONE_GEN: Lazy<Mutex<std::collections::HashMap<u32, u64>>> = Lazy::new(Default::default);

/// Ce qu'on envoie au tel : tous les fichiers d'un mix / d'une playlist, sinon le fichier obtenu
/// (ou les fichiers de son dossier).
fn phone_files(job: &Job) -> Vec<PathBuf> {
    if !job.items.is_empty() {
        return job.items.iter().map(PathBuf::from).filter(|p| p.is_file()).collect();
    }
    match job.path.as_deref().map(Path::new) {
        Some(p) if p.is_file() => vec![p.to_path_buf()],
        Some(p) if p.is_dir() => {
            let mut v: Vec<PathBuf> = std::fs::read_dir(p).into_iter().flatten().flatten().map(|e| e.path()).filter(|f| f.is_file()).collect();
            v.sort();
            v
        }
        _ => vec![],
    }
}

/// Envoie un téléchargement sur le tel. Sans tel choisi (et aucun en mémoire) : Err("choose"),
/// l'interface propose alors la liste des tels trouvés.
pub fn phone_send(id: u32, dev: Option<crate::localsend::Device>) -> Result<(), String> {
    let job = jobs().into_iter().find(|j| j.id == id).ok_or("Introuvable")?;
    if job.status != "done" {
        return Err("Téléchargement pas terminé".into());
    }
    let files = phone_files(&job);
    if files.is_empty() {
        return Err("Fichier introuvable (déplacé ou supprimé ?)".into());
    }
    let dev = match dev {
        Some(d) => {
            crate::localsend::remember(&d);
            d
        }
        None => crate::localsend::remembered().ok_or("choose")?,
    };
    let gen = {
        let mut g = PHONE_GEN.lock();
        let e = g.entry(id).or_insert(0);
        *e += 1;
        *e
    };
    let alive = move || PHONE_GEN.lock().get(&id) == Some(&gen);
    let set = move |state: &str, pct: Option<f64>, dev: &str, msg: Option<String>| {
        if !alive() {
            return;
        }
        with_job(id, |j| j.phone = Some(Phone { state: state.into(), pct, dev: dev.into(), msg }));
        emit_jobs();
    };
    set("find", None, &dev.alias, None);
    std::thread::spawn(move || {
        let Some(found) = crate::localsend::locate(&dev) else {
            set("error", None, &dev.alias, Some(format!("{} introuvable : ouvre LocalSend dessus (même Wi-Fi)", dev.alias)));
            return;
        };
        if found.ip != dev.ip || found.https != dev.https || found.alias != dev.alias {
            crate::localsend::remember(&found);
        }
        let name = found.alias.clone();
        let mut last = Instant::now() - Duration::from_secs(1);
        let r = crate::localsend::send(&found, &files, |s| {
            if !alive() {
                return false;
            }
            match s {
                crate::localsend::Step::Waiting => set("wait", None, &name, None),
                crate::localsend::Step::Sending(a, t) => {
                    if last.elapsed() >= Duration::from_millis(150) {
                        last = Instant::now();
                        set("send", Some(if t > 0 { a as f64 / t as f64 } else { 0.0 }), &name, None);
                    }
                }
            }
            true
        });
        match r {
            Ok(n) => {
                crate::log(&format!("LocalSend : {n} fichier(s) envoyé(s) sur le tel"));
                let msg = if files.len() > 1 { Some(format!("{n} fichiers")) } else { None };
                set("done", None, &name, msg);
            }
            Err(e) => {
                if alive() {
                    crate::log(&format!("LocalSend : envoi non abouti ({e})"));
                }
                set("error", None, &name, Some(e));
            }
        }
    });
    Ok(())
}

/// Annule l'envoi vers le tel (ou efface son message).
pub fn phone_cancel(id: u32) {
    *PHONE_GEN.lock().entry(id).or_insert(0) += 1;
    with_job(id, |j| j.phone = None);
    emit_jobs();
}

/// Un titre a quitté « Ma musique » : les téléchargements correspondants ne sont plus marqués.
pub fn music_removed(name: &std::ffi::OsStr) {
    let mut changed = false;
    for j in ST.lock().jobs.iter_mut() {
        if j.in_music && j.path.iter().chain(j.items.iter()).any(|p| Path::new(p).file_name().map(|n| n == name).unwrap_or(false)) {
            j.in_music = false;
            changed = true;
        }
    }
    if changed {
        save_history();
        emit_jobs();
    }
}

/// Met des fichiers dans un dossier (lien physique : aucune place en plus ; copie si autre disque).
/// Renvoie (ajoutés, déjà présents).
fn link_into(files: &[PathBuf], dir: &Path) -> Result<(u32, u32), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("Dossier impossible à créer : {e}"))?;
    let (mut added, mut already) = (0u32, 0u32);
    for f in files {
        let Some(name) = f.file_name() else { continue };
        let dest = dir.join(name);
        if dest.exists() {
            already += 1;
            continue;
        }
        if std::fs::hard_link(f, &dest).is_err() {
            std::fs::copy(f, &dest).map_err(|e| format!("Copie impossible : {e}"))?;
        }
        added += 1;
    }
    Ok((added, already))
}

/// Ajoute le(s) MP3 d'un téléchargement à « Ma musique ». Renvoie les chemins (= id des titres dans le lecteur).
pub fn music_add(id: u32) -> Result<Value, String> {
    let job = jobs().into_iter().find(|j| j.id == id).ok_or("Introuvable")?;
    if job.kind != "audio" {
        return Err("Seulement pour les MP3".into());
    }
    let files = job_files(&job);
    if files.is_empty() {
        return Err("Fichier introuvable (déplacé ou supprimé ?)".into());
    }
    let dir = music_dir();
    let (added, already) = link_into(&files, &dir)?;
    with_job(id, |j| j.in_music = true);
    save_history();
    emit_jobs();
    crate::log(&format!("téléchargeur : {added} titre(s) ajouté(s) à Ma musique ({already} déjà présent(s))"));
    let ids: Vec<String> = files.iter().filter_map(|f| f.file_name()).map(|n| dir.join(n).to_string_lossy().to_string()).collect();
    Ok(json!({ "added": added, "already": already, "total": files.len(), "ids": ids }))
}

/// Les MP3 d'un téléchargement : tous les titres d'un mix / d'une playlist, sinon le fichier (ou dossier) obtenu.
fn job_files(job: &Job) -> Vec<PathBuf> {
    if !job.items.is_empty() {
        return job.items.iter().map(PathBuf::from).filter(|p| p.is_file()).collect();
    }
    job.path.as_deref().map(|p| audio_files(Path::new(p))).unwrap_or_default()
}

pub(crate) fn audio_files(p: &Path) -> Vec<PathBuf> {
    let is_audio = |f: &Path| f.extension().and_then(|e| e.to_str()).map(|e| matches!(e.to_lowercase().as_str(), "mp3" | "m4a" | "flac" | "wav" | "ogg")).unwrap_or(false);
    if p.is_dir() {
        let mut v: Vec<PathBuf> = std::fs::read_dir(p).into_iter().flatten().flatten().map(|e| e.path()).filter(|f| f.is_file() && is_audio(f)).collect();
        v.sort();
        v
    } else if p.is_file() && is_audio(p) {
        vec![p.to_path_buf()]
    } else {
        vec![]
    }
}

/// Met le(s) MP3 d'un téléchargement dans le dossier lu par Spotify (lien physique : aucune place en plus,
/// copie si le dossier est sur un autre disque). Spotify ne permet pas de « liker » un fichier local à distance :
/// on le rend juste visible dans Spotify (Bibliothèque → Fichiers locaux).
pub fn spotify_like(id: u32) -> Result<Value, String> {
    let job = jobs().into_iter().find(|j| j.id == id).ok_or("Introuvable")?;
    if job.kind != "audio" {
        return Err("Seulement pour les MP3".into());
    }
    let files = job_files(&job);
    if files.is_empty() {
        return Err("Fichier introuvable (déplacé ou supprimé ?)".into());
    }
    let dir = spotify_dir();
    let created = !dir.exists();
    let (added, already) = link_into(&files, &dir).map_err(|e| format!("Spotify : {e}"))?;
    let n = files.len();
    let liked = if n == 1 { "Dans Spotify (fichiers locaux)".to_string() } else { format!("{n} titres dans Spotify (fichiers locaux)") };
    with_job(id, |j| j.liked = Some(liked.clone()));
    save_history();
    emit_jobs();
    let spotifast = spotifast_link(&dir);
    // Les explications ne s'affichent qu'une fois (ou si le dossier vient d'être recréé, ou que Spotifast vient d'être relié).
    let marker = crate::config::dir().join(if spotifast.is_some() { "spotifast-local-help" } else { "spotify-local-help" });
    let sf_status = spotifast.as_ref().and_then(|v| v["status"].as_str()).unwrap_or("");
    let first = created || !marker.exists() || sf_status == "linked";
    if sf_status != "running" {
        let _ = std::fs::write(&marker, "1");
    }
    crate::log(&format!("téléchargeur : {added} titre(s) mis dans le dossier Spotify ({already} déjà présent(s))"));
    Ok(json!({ "liked": liked, "added": added, "already": already, "total": n, "dir": dir.to_string_lossy(), "first": first, "spotifast": spotifast }))
}

/// Spotifast (client Spotify) : relie son réglage « local_files_folder » au dossier de l'île.
/// Renvoie None si Spotifast n'est pas installé, sinon {status, soloist}.
/// status : "linked" (vient d'être relié), "ok" (déjà relié), "running" (à fermer pour relier), "other" (autre dossier choisi : on n'y touche pas).
fn spotifast_link(dir: &Path) -> Option<Value> {
    let appdata = std::env::var_os("APPDATA")?;
    let path = PathBuf::from(appdata).join("paolino").join("spotifast").join("config").join("settings.json");
    let text = std::fs::read_to_string(&path).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    let soloist = v["soloist_enabled"].as_bool().unwrap_or(false);
    let want = dir.to_string_lossy().to_string();
    let cur = v["local_files_folder"].as_str().unwrap_or("").to_string();
    let status = if cur.eq_ignore_ascii_case(&want) {
        "ok"
    } else if !cur.is_empty() {
        "other"
    } else if crate::privacy::running_exes().contains("spotifast.exe") {
        "running" // Spotifast réécrit ses réglages en quittant : on attend qu'il soit fermé
    } else {
        // on ne change que cette ligne, le reste du fichier reste tel quel
        let re = regex::Regex::new(r#""local_files_folder"\s*:\s*(null|"")"#).unwrap();
        let val = serde_json::to_string(&want).unwrap_or_default();
        let out = re.replace(&text, format!("\"local_files_folder\": {val}").as_str()).to_string();
        let tmp = path.with_extension("json.ile-tmp");
        if out == text || serde_json::from_str::<Value>(&out).is_err() || std::fs::write(&tmp, &out).is_err() || std::fs::rename(&tmp, &path).is_err() {
            let _ = std::fs::remove_file(&tmp);
            "running"
        } else {
            crate::log("téléchargeur : dossier des fichiers locaux relié dans Spotifast");
            "linked"
        }
    };
    Some(json!({ "status": status, "soloist": soloist }))
}

/// Ouvre le dossier Spotify dans l'Explorateur.
pub fn spotify_dir_open() -> Result<(), String> {
    let dir = spotify_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    std::process::Command::new("explorer").arg(&dir).spawn().map(|_| ()).map_err(|e| e.to_string())
}

#[cfg(test)]
mod mix_tests {
    /// le vrai chemin de téléchargement d'un mix (4 titres), doublons de Ma musique compris
    #[test]
    #[ignore]
    fn mix_download() {
        let job = super::Job {
            id: 9999,
            url: "https://www.youtube.com/watch?v=vk3faosjB8M&list=RDMMC55CgIJgwKg".into(),
            kind: "audio".into(),
            opts: super::Opts { playlist: true, limit: Some(4), ..Default::default() },
            ..Default::default()
        };
        super::ST.lock().jobs.push(job.clone());
        let r = super::run(job);
        let j = super::ST.lock().jobs.iter().find(|j| j.id == 9999).cloned().unwrap();
        println!("résultat {:?} · fichiers {} · note {:?} · chemin {:?}", r, j.files, j.error, j.path);
        assert!(r.is_ok());
    }

    /// les mix déjà téléchargés retrouvent tous leurs titres
    #[test]
    #[ignore]
    fn mix_items() {
        for j in super::load_history().iter().filter(|j| j.items.len() > 1) {
            println!("{} : {} titres · Ma musique en recevrait {}", j.title, j.items.len(), super::job_files(j).len());
        }
    }
}

#[cfg(test)]
mod music_tests {
    #[test]
    #[ignore]
    fn probes() {
        for u in [
            "https://open.spotify.com/intl-fr/track/0dEIca2nhcxDUV8C5QkPYb?si=abc",
            "https://open.spotify.com/album/4m2880jivSbbyEGAKfITCa",
            "https://open.spotify.com/playlist/37i9dQZF1DXcBWIGoYBM5M",
            "https://www.deezer.com/fr/track/3135556",
            "https://www.deezer.com/album/6575789",
        ] {
            match super::probe(u) {
                Ok(v) => println!("{} · {} · {} titres · {:?} · thumb {}", v["title"], v["author"], v["count"], v["tracks"][0], v["thumb"].as_str().map(|s| s.len()).unwrap_or(0)),
                Err(e) => println!("{u} : ERREUR {e}"),
            }
        }
    }

    #[test]
    #[ignore]
    fn find_and_tag() {
        let t = super::MTrack { title: "Harder, Better, Faster, Stronger".into(), artist: "Daft Punk".into(), duration: Some(224.0), ..Default::default() };
        println!("{:?}", super::yt_find(&t));
        let t = super::MTrack { title: "Get Lucky (feat. Pharrell Williams and Nile Rodgers)".into(), artist: "Daft Punk, Pharrell Williams, Nile Rodgers".into(), duration: Some(369.0), ..Default::default() };
        println!("{:?}", super::yt_find(&t));
        println!("{:?}", super::deezer_cover(&t));
        println!("{}", super::base_title("Get Lucky (feat. Pharrell Williams and Nile Rodgers) - Radio Edit"));
    }

    fn job(url: &str, kind: &str, start: Option<f64>, end: Option<f64>) {
        let info = super::probe(url).expect("probe");
        let mut st = super::ST.lock();
        st.seq += 1;
        let id = st.seq;
        let tracks: Vec<super::MTrack> = serde_json::from_value(info["tracks"].clone()).unwrap_or_default();
        let job = super::Job {
            id, url: url.into(), kind: kind.into(), status: "running".into(), title: info["title"].as_str().unwrap_or("").into(),
            opts: super::Opts { start, end, music: !tracks.is_empty(), playlist: tracks.len() > 1, tracks, vid: info["id"].as_str().unwrap_or("").into(), ..Default::default() },
            ..Default::default()
        };
        st.jobs.push(job.clone());
        drop(st);
        let t = std::time::Instant::now();
        let r = super::run(job);
        let j = super::jobs().into_iter().find(|j| j.id == id).unwrap();
        let size = j.path.as_ref().and_then(|p| std::fs::metadata(p).ok()).map(|m| m.len()).unwrap_or(0);
        println!("{kind} -> {:?} en {} ms : {:?} ({} octets, {} fichiers, err {:?})", r, t.elapsed().as_millis(), j.path, size, j.files, j.error);
    }

    #[test]
    #[ignore]
    fn x_gif() {
        let u = std::env::var("GRAB_URL").unwrap();
        let v = super::probe(&u).unwrap();
        println!("is_gif {} audio {} videos {}", v["is_gif"], v["audio"], v["videos"]);
        job(&u, "gif", None, None);
    }

    #[test]
    #[ignore]
    fn runs() {
        job("https://www.deezer.com/fr/track/3135556", "audio", None, None);
        job("https://www.youtube.com/watch?v=jNQXAC9IVRw", "gif", Some(2.0), Some(6.0));
    }
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore]
    fn probe_url() {
        let url = std::env::var("GRAB_URL").unwrap_or("https://www.youtube.com/watch?v=jNQXAC9IVRw".into());
        let t = std::time::Instant::now();
        match super::probe(&url) {
            Ok(mut v) => {
                v["thumb"] = serde_json::json!(v["thumb"].as_str().map(|s| s.len()));
                println!("{} ms\n{}", t.elapsed().as_millis(), serde_json::to_string_pretty(&v).unwrap());
            }
            Err(e) => println!("ERREUR : {e}"),
        }
    }

    #[test]
    #[ignore]
    fn tools() {
        println!("{}", super::status());
    }
}

#[cfg(test)]
mod setup_tests {
    #[test]
    #[ignore]
    fn install_missing() {
        println!("{:?}", super::install_all());
        println!("{}", super::status());
    }
}

#[cfg(test)]
mod run_tests {
    use super::*;
    fn go(url: &str, kind: &str, format: Option<u32>, start: Option<f64>, end: Option<f64>) {
        let info = probe(url).expect("probe");
        let mut st = ST.lock();
        st.seq += 1;
        let id = st.seq;
        let subs = info.get("subs").filter(|x| !x.is_null());
        let job = Job {
            id, url: url.into(), kind: kind.into(), status: "running".into(), live: false,
            opts: Opts { format, start, end, playlist: false, sub_lang: subs.and_then(|s| s["lang"].as_str()).map(str::to_string),
                sub_auto: subs.and_then(|s| s["auto"].as_bool()).unwrap_or(false), expect: 0, vid: info["id"].as_str().unwrap_or("").into(), ..Default::default() },
            ..Default::default()
        };
        st.jobs.push(job.clone());
        drop(st);
        let t = Instant::now();
        let r = run(job);
        let j = jobs().into_iter().find(|j| j.id == id).unwrap();
        let size = j.path.as_ref().and_then(|p| std::fs::metadata(p).ok()).map(|m| m.len()).unwrap_or(0);
        println!("{kind} {:?} -> {:?} en {} ms : {:?} ({} octets)", format, r, t.elapsed().as_millis(), j.path, size);
    }
    #[test]
    #[ignore]
    fn downloads() {
        go("https://www.youtube.com/watch?v=jNQXAC9IVRw", "video", None, None, None);
        go("https://www.youtube.com/watch?v=jNQXAC9IVRw", "audio", None, None, None);
        go("https://www.youtube.com/watch?v=jNQXAC9IVRw", "text", None, None, None);
        go("https://www.youtube.com/watch?v=LXb3EKWsInQ", "video", Some(720), Some(10.0), Some(16.0));
    }
}

#[cfg(test)]
mod import_tests {
    #[test]
    #[ignore]
    fn import() {
        let n = super::import_folder();
        println!("{n} importés");
        for j in super::jobs() {
            println!("{} | {} | {} | {} | {} o | thumb {} | tracks {:?} | {}", j.id, j.kind, j.label, j.title, j.bytes, j.thumb.as_ref().map(|t| t.len()).unwrap_or(0),
                j.opts.tracks.iter().map(|t| format!("{} / {}", t.title, t.artist)).collect::<Vec<_>>(), j.url);
        }
    }
}
