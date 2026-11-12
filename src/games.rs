//! Jeux : détecte le jeu en cours, retient le dernier joué et sait le relancer.
//!
//! Trois sources, de la plus sûre à la plus large :
//! 1. la liste de détection de Discord (≈ 25 000 jeux : nom officiel + logo), téléchargée
//!    une fois par semaine et gardée en cache compact dans %APPDATA%\DynamicIsland ;
//! 2. la liste des jeux reconnus par Windows (Game Bar : HKCU\System\GameConfigStore) ;
//! 3. les dossiers de jeux (Jeux, Games, CRACKED, SteamLibrary, EPIC, LOL…) : tout programme
//!    lancé depuis là avec une vraie fenêtre est considéré comme un jeu.

use crate::{audio, config, log, media};
use once_cell::sync::Lazy;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};
use tauri::{AppHandle, Emitter};
use windows::core::{HSTRING, PWSTR};
use windows::core::BOOL;
use windows::Win32::Foundation::{HWND, LPARAM, RECT};
use windows::Win32::System::Registry::*;

#[derive(Serialize, Clone, Default, PartialEq)]
pub struct Game {
    pub key: String,
    pub name: String,
    pub icon: Option<String>,
    pub cover: Option<String>,
    /// début de la partie (ms epoch)
    pub since: i64,
    /// dernière fois vu en jeu (ms epoch)
    pub last: i64,
}

#[derive(Serialize, Clone, Default)]
pub struct GameState {
    pub running: Option<Game>,
    pub last: Option<Game>,
}

#[derive(Clone, Debug)]
enum Launch {
    Uri(String),
    Exe(PathBuf, String),
}

static STATE: Lazy<Mutex<GameState>> = Lazy::new(Default::default);
static LAUNCH: Lazy<Mutex<HashMap<String, Launch>>> = Lazy::new(Default::default);
/// jeux déjà résolus (nom, icône…), par chemin d'exe en minuscules
static RESOLVED: Lazy<Mutex<HashMap<String, Option<(Game, Launch)>>>> = Lazy::new(Default::default);
static DISCORD: Lazy<Mutex<Option<DiscordIndex>>> = Lazy::new(Default::default);

pub fn state() -> GameState {
    STATE.lock().clone()
}

/// Lance (ou relance) un jeu connu.
pub fn launch(key: &str) -> Result<(), String> {
    let l = LAUNCH.lock().get(key).cloned().ok_or("Jeu introuvable")?;
    log(&format!("jeu : lancement {l:?}"));
    match l {
        Launch::Uri(u) => crate::shell_open(&u),
        Launch::Exe(exe, args) => {
            if !exe.exists() {
                return Err("Le jeu n'est plus installé à cet endroit".into());
            }
            let dir = exe.parent().map(|p| p.to_string_lossy().to_string()).unwrap_or_default();
            shell_exec(&exe.to_string_lossy(), &args, &dir)
        }
    }
}

fn shell_exec(file: &str, args: &str, dir: &str) -> Result<(), String> {
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    let r = unsafe {
        ShellExecuteW(None, &HSTRING::from("open"), &HSTRING::from(file), &HSTRING::from(args), &HSTRING::from(dir), SW_SHOWNORMAL)
    };
    if r.0 as isize > 32 { Ok(()) } else { Err("Impossible de lancer le jeu".into()) }
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

// ---------------------------------------------------------------------------
// Boucle de détection
// ---------------------------------------------------------------------------

pub fn start(app: AppHandle) {
    std::thread::spawn(discord_refresh);
    std::thread::spawn(move || {
        let mut known = Known::default();
        let mut loaded = Instant::now() - Duration::from_secs(3600);
        let mut paths: HashMap<u32, (String, Option<String>)> = HashMap::new();
        let mut cur_key: Option<String> = None;
        let mut since = 0i64;
        let mut sig = String::from("-");
        loop {
            if loaded.elapsed() > Duration::from_secs(60) {
                known = Known::load();
                loaded = Instant::now();
            }
            let found = find_running(&known, &mut paths);
            let now = now_ms();
            let snapshot = {
                let mut st = STATE.lock();
                match found {
                    Some((mut g, l, exe)) => {
                        if cur_key.as_deref() != Some(&g.key) {
                            since = now;
                            cur_key = Some(g.key.clone());
                            log(&format!("jeu détecté : {}", g.name));
                            remember(&exe);
                        }
                        g.since = since;
                        g.last = now;
                        LAUNCH.lock().insert(g.key.clone(), l);
                        st.last = Some(g.clone());
                        st.running = Some(g);
                    }
                    None => {
                        cur_key = None;
                        st.running = None;
                        if st.last.is_none() {
                            st.last = last_played(&known);
                        }
                    }
                }
                st.clone()
            };
            let s = format!(
                "{}|{}",
                snapshot.running.as_ref().map(|g| g.key.as_str()).unwrap_or(""),
                snapshot.last.as_ref().map(|g| g.key.as_str()).unwrap_or("")
            );
            if s != sig {
                sig = s;
                let _ = app.emit("game", snapshot);
            }
            std::thread::sleep(Duration::from_secs(3));
        }
    });
}

/// Ce que l'on sait des jeux installés (Game Bar + dossiers de jeux).
#[derive(Default)]
struct Known {
    /// chemin complet en minuscules → (chemin, dernier lancement ms)
    store: HashMap<String, (String, i64)>,
    /// noms de fichiers (minuscules) présents dans `store`
    files: HashSet<String>,
    /// dossiers de jeux (minuscules, avec « \ » final)
    roots: Vec<String>,
}

const BLOCKED_EXES: &[&str] = &[
    "aseprite.exe", "robloxstudiobeta.exe", "blender.exe", "unity.exe", "unrealeditor.exe", "code.exe",
    "dynamic-island.exe", "steam.exe", "steamwebhelper.exe", "epicgameslauncher.exe", "riotclientservices.exe",
    "riotclientux.exe", "riotclientcrashhandler.exe", "explorer.exe", "discord.exe",
    // client / lanceur Riot : seule la partie (League of Legends.exe) compte comme un jeu
    "leagueclient.exe", "leagueclientux.exe", "leagueclientuxrender.exe", "riotclientuxrender.exe", "leaguecrashhandler64.exe",
];

impl Known {
    fn load() -> Known {
        let mut k = Known::default();
        for (exe, last) in game_config_store() {
            let lower = exe.to_lowercase();
            let file = file_name(&lower);
            if BLOCKED_EXES.contains(&file.as_str()) {
                continue;
            }
            k.files.insert(file);
            k.store.insert(lower, (exe, last));
        }
        k.roots = game_roots(&k.store);
        k
    }
}

fn file_name(p: &str) -> String {
    p.rsplit(['\\', '/']).next().unwrap_or(p).to_string()
}

/// Dossiers de jeux : bibliothèques Steam + dossiers « Jeux / Games / CRACKED / EPIC… » à la racine
/// des disques + dossiers d'où Windows a déjà vu des jeux.
fn game_roots(store: &HashMap<String, (String, i64)>) -> Vec<String> {
    let re = regex::Regex::new(
        r"(?i)^(jeux|games?|my games|cracked|steamlibrary|steam|epic|epic ?games|xbox|xboxgames|cod|lol|riot|riot games|rockstar|rockstar games|ubisoft|gog|gog games|ea|ea games|battle\.net|blizzard|.*_steam)$",
    )
    .unwrap();
    let mut roots: Vec<String> = Vec::new();
    let mut push = |p: String| {
        let mut p = p.to_lowercase().replace('/', "\\");
        if !p.ends_with('\\') {
            p.push('\\');
        }
        if !roots.contains(&p) {
            roots.push(p);
        }
    };
    for lib in steam_libraries() {
        push(lib.join("steamapps").join("common").to_string_lossy().to_string());
    }
    for d in b'C'..=b'Z' {
        let drive = format!("{}:\\", d as char);
        let Ok(rd) = std::fs::read_dir(&drive) else { continue };
        for e in rd.flatten() {
            if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                let n = e.file_name().to_string_lossy().to_string();
                if re.is_match(&n) {
                    push(format!("{drive}{n}"));
                }
            }
        }
    }
    // dossiers de premier niveau d'où viennent les jeux connus de Windows (D:\CRACKED, C:\Riot Games…)
    let generic = ["program files", "program files (x86)", "users", "windows", "programdata", "xboxgames", "windowsapps"];
    for (lower, _) in store.values().map(|(e, l)| (e.to_lowercase(), l)) {
        let parts: Vec<&str> = lower.split('\\').collect();
        if parts.len() >= 3 && !generic.contains(&parts[1]) {
            push(format!("{}\\{}", parts[0], parts[1]));
        }
    }
    roots
}

/// Programmes annexes à ignorer dans les dossiers de jeux.
fn is_helper(file: &str) -> bool {
    const BAD: &[&str] = &[
        "crash", "report", "unins", "setup", "install", "redist", "dxsetup", "helper", "anticheat", "easyanticheat",
        "beservice", "battleye", "update", "patcher", "upc.exe", "uplay", "ubisoftconnect", "cefprocess", "webhelper",
        "overlay", "elevate", "service", "bootstrap", "prereq", "dotnet", "handler", "qtwebengine", "launcherpatcher",
        "vc_redist", "dxwebsetup", "notification", "updater",
    ];
    BAD.iter().any(|b| file.contains(b))
}

/// Un lanceur (client Riot, launcher d'éditeur…) n'est pas une partie.
fn is_launcher(lower_path: &str) -> bool {
    let file = lower_path.rsplit('\\').next().unwrap_or("");
    lower_path.contains(r"\riot client\") || file.contains("launcher") || file.starts_with("leagueclient")
}

/// pid → fenêtres visibles d'une taille raisonnable
fn windowed_pids() -> HashSet<u32> {
    use windows::Win32::UI::WindowsAndMessaging::{EnumWindows, GetWindowRect, GetWindowThreadProcessId, IsWindowVisible};
    unsafe extern "system" fn cb(h: HWND, l: LPARAM) -> BOOL {
        let set = &mut *(l.0 as *mut HashSet<u32>);
        if IsWindowVisible(h).as_bool() {
            let mut r = RECT::default();
            if GetWindowRect(h, &mut r).is_ok() && r.right - r.left >= 320 && r.bottom - r.top >= 200 {
                let mut pid = 0u32;
                GetWindowThreadProcessId(h, Some(&mut pid));
                set.insert(pid);
            }
        }
        BOOL(1)
    }
    let mut set: HashSet<u32> = HashSet::new();
    unsafe {
        let _ = EnumWindows(Some(cb), LPARAM(&mut set as *mut _ as isize));
    }
    set
}

fn processes() -> Vec<(u32, String)> {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Diagnostics::ToolHelp::*;
    let mut out = Vec::new();
    unsafe {
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else { return out };
        let mut e = PROCESSENTRY32W { dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        let mut ok = Process32FirstW(snap, &mut e).is_ok();
        while ok {
            let len = e.szExeFile.iter().position(|&c| c == 0).unwrap_or(e.szExeFile.len());
            out.push((e.th32ProcessID, String::from_utf16_lossy(&e.szExeFile[..len]).to_lowercase()));
            ok = Process32NextW(snap, &mut e).is_ok();
        }
        let _ = CloseHandle(snap);
    }
    out
}

/// Cherche le jeu en cours (les lanceurs, comme le client LoL, ne comptent pas).
fn find_running(known: &Known, paths: &mut HashMap<u32, (String, Option<String>)>) -> Option<(Game, Launch, String)> {
    let procs = processes();
    let alive: HashSet<u32> = procs.iter().map(|p| p.0).collect();
    paths.retain(|pid, _| alive.contains(pid));
    let mut windows: Option<HashSet<u32>> = None;
    let mut best: Option<(u8, String)> = None; // (score, chemin)
    let discord = DISCORD.lock();
    for (pid, file) in procs {
        if pid <= 4 || BLOCKED_EXES.contains(&file.as_str()) {
            continue;
        }
        let entry = paths.entry(pid).or_insert_with(|| (file.clone(), None));
        if entry.1.is_none() {
            entry.1 = Some(audio::exe_path(pid).unwrap_or_default());
        }
        let path = entry.1.clone().unwrap_or_default();
        if path.is_empty() {
            continue;
        }
        let lower = path.to_lowercase();
        if is_launcher(&lower) {
            continue;
        }
        let score = if let Some(d) = discord.as_ref().and_then(|d| d.find(&lower)) {
            if d.launcher {
                continue;
            }
            4
        } else if known.files.contains(&file) && known.store.contains_key(&lower) {
            3
        } else if !is_helper(&file)
            && file.ends_with(".exe")
            && known.roots.iter().any(|r| lower.starts_with(r))
            && windows.get_or_insert_with(windowed_pids).contains(&pid)
        {
            1
        } else {
            continue;
        };
        if best.as_ref().map(|b| score > b.0).unwrap_or(true) {
            best = Some((score, path));
        }
    }
    drop(discord);
    let (_, path) = best?;
    resolve(&path).map(|(g, l)| (g, l, path))
}

// ---------------------------------------------------------------------------
// Nom, icône et lancement d'un jeu à partir de son exécutable
// ---------------------------------------------------------------------------

fn resolve(path: &str) -> Option<(Game, Launch)> {
    let lower = path.to_lowercase();
    if let Some(r) = RESOLVED.lock().get(&lower) {
        return r.clone();
    }
    let r = resolve_uncached(path);
    RESOLVED.lock().insert(lower, r.clone());
    r
}

fn resolve_uncached(path: &str) -> Option<(Game, Launch)> {
    let lower = path.to_lowercase().replace('/', "\\");
    let exe = PathBuf::from(path);
    let file = file_name(&lower);
    let disc = DISCORD.lock().as_ref().and_then(|d| d.find(&lower).cloned());

    let mut name: Option<String> = disc.as_ref().map(|d| d.name.clone());
    let mut cover = None;
    let mut launch = Launch::Exe(exe.clone(), String::new());
    let mut steam_icon = None;
    let mut key = disc.as_ref().map(|d| format!("discord:{}", d.id)).unwrap_or_else(|| format!("exe:{lower}"));

    if lower.contains("\\riot games\\") || lower.contains("\\riot\\") {
        // les jeux Riot se lancent via le Riot Client
        let product = if lower.contains("valorant") { Some(("valorant", "VALORANT")) } else if lower.contains("league of legends") { Some(("league_of_legends", "League of Legends")) } else { None };
        if let (Some((prod, pname)), Some(client)) = (product, riot_client()) {
            launch = Launch::Exe(client, format!("--launch-product={prod} --launch-patchline=live"));
            name.get_or_insert_with(|| pname.to_string());
            if disc.is_none() {
                key = format!("riot:{prod}");
            }
        }
    } else if file == "javaw.exe" || file == "java.exe" {
        // Minecraft via Prism Launcher
        if lower.contains("prismlauncher") {
            let prism = PathBuf::from(std::env::var("LOCALAPPDATA").unwrap_or_default()).join(r"Programs\PrismLauncher\prismlauncher.exe");
            name = Some("Minecraft".into());
            key = "minecraft".into();
            launch = Launch::Exe(prism.clone(), String::new());
            steam_icon = audio::exe_icon(&prism.to_string_lossy(), 96);
        } else {
            return None;
        }
    } else if let Some(i) = lower.find("\\steamapps\\common\\") {
        let lib = &path[..i];
        let dir = lower[i + 18..].split('\\').next().unwrap_or("").to_string();
        if let Some((appid, sname)) = steam_manifest(Path::new(lib), &dir) {
            launch = Launch::Uri(format!("steam://rungameid/{appid}"));
            name.get_or_insert(sname);
            cover = steam_art(&appid, &["library_header.jpg"], 420);
            if let Some(cache) = steam_path().map(|s| s.join(r"appcache\librarycache").join(&appid)) {
                // petite icône Steam (fichier .jpg au nom en hash), si l'exe n'en a pas
                steam_icon = std::fs::read_dir(cache).ok().and_then(|rd| {
                    rd.flatten()
                        .find(|e| {
                            let n = e.file_name().to_string_lossy().to_string();
                            n.len() == 44 && n.ends_with(".jpg")
                        })
                        .and_then(|e| std::fs::read(e.path()).ok())
                        .and_then(|b| media::shrink(&b, 96))
                });
            }
        }
    } else if let Some((ename, uri)) = epic_game(&lower) {
        name.get_or_insert(ename);
        launch = Launch::Uri(uri);
    }

    let name = name
        .or_else(|| audio::version_string(path, "ProductName").map(|s| clean_name(&s)).filter(|s| good_name(s)))
        .or_else(|| audio::version_string(path, "FileDescription").map(|s| clean_name(&s)).filter(|s| good_name(s)))
        .unwrap_or_else(|| folder_name(path));

    let icon = disc
        .as_ref()
        .and_then(discord_icon)
        .or_else(|| audio::exe_icon(path, 96))
        .or(steam_icon);

    Some((Game { key, name, icon, cover, since: 0, last: 0 }, launch))
}

fn clean_name(s: &str) -> String {
    let s = s.replace("(TM)", "").replace('™', "").replace('®', "").replace("(R)", "").replace("(C)", "");
    let s = s.trim_end_matches(" Client").trim_end_matches(" Launcher");
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn good_name(s: &str) -> bool {
    let l = s.to_lowercase();
    l.len() >= 2
        && !["game", "unity", "client", "launcher", "application", "app"].contains(&l.as_str())
        && !["unreal", "bootstrap", "shipping", "win64", ".exe", "java(", "java platform", "epic games", "electron"]
            .iter()
            .any(|j| l.contains(j))
}

/// Nom de dossier du jeu : le premier dossier après « Jeux / Games / CRACKED… ».
fn folder_name(path: &str) -> String {
    let containers = [
        "jeux", "games", "game", "cracked", "steamlibrary", "steamapps", "common", "epic", "epic games", "xbox", "xboxgames",
        "cod", "lol", "riot", "riot games", "rockstar", "ubisoft", "program files", "program files (x86)", "gog games", "ea games",
    ];
    let parts: Vec<&str> = path.split(['\\', '/']).collect();
    let dirs = &parts[1..parts.len().saturating_sub(1)];
    let mut pick = None;
    for (i, d) in dirs.iter().enumerate() {
        if containers.contains(&d.to_lowercase().as_str()) {
            pick = dirs.get(i + 1).copied();
        }
    }
    pick.or_else(|| dirs.first().copied())
        .map(|s| s.to_string())
        .unwrap_or_else(|| file_name(path).trim_end_matches(".exe").to_string())
}

// ---------------------------------------------------------------------------
// Dernier jeu joué
// ---------------------------------------------------------------------------

fn last_file() -> PathBuf {
    config::dir().join("last_game.json")
}

#[derive(Serialize, Deserialize)]
struct LastSaved {
    exe: String,
    at: i64,
}

/// Mémorise l'exe du jeu en cours (appelé quand un jeu démarre).
fn remember(exe: &str) {
    let s = LastSaved { exe: exe.to_string(), at: now_ms() };
    if let Ok(j) = serde_json::to_string(&s) {
        let _ = std::fs::write(last_file(), j);
    }
}

/// Le dernier jeu joué : le plus récent entre ce qu'on a retenu et ce que Windows a noté.
fn last_played(known: &Known) -> Option<Game> {
    let mut cands: Vec<(i64, String)> = known.store.values().map(|(e, t)| (*t, e.clone())).collect();
    if let Some(s) = std::fs::read_to_string(last_file()).ok().and_then(|s| serde_json::from_str::<LastSaved>(&s).ok()) {
        cands.push((s.at, s.exe));
    }
    cands.sort_by(|a, b| b.0.cmp(&a.0));
    for (t, exe) in cands.into_iter().take(12) {
        if is_launcher(&exe.to_lowercase()) || !Path::new(&exe).exists() {
            continue;
        }
        if let Some((mut g, l)) = resolve(&exe) {
            g.last = t;
            LAUNCH.lock().insert(g.key.clone(), l);
            return Some(g);
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Windows Game Bar (GameConfigStore)
// ---------------------------------------------------------------------------

fn game_config_store() -> Vec<(String, i64)> {
    let mut out = Vec::new();
    unsafe {
        let mut key = HKEY::default();
        if RegOpenKeyExW(HKEY_CURRENT_USER, &HSTRING::from(r"System\GameConfigStore\Children"), None, KEY_READ, &mut key).is_err() {
            return out;
        }
        let mut i = 0u32;
        loop {
            let mut buf = [0u16; 256];
            let mut len = buf.len() as u32;
            if RegEnumKeyExW(key, i, Some(PWSTR(buf.as_mut_ptr())), &mut len, None, None, None, None).is_err() {
                break;
            }
            i += 1;
            let child = String::from_utf16_lossy(&buf[..len as usize]);
            let Some(exe) = reg_str(key, &child, "MatchedExeFullPath") else { continue };
            let ft = reg_u64(key, &child, "LastAccessed").unwrap_or(0) as i64;
            let ms = if ft > 0 { ft / 10_000 - 11_644_473_600_000 } else { 0 };
            out.push((exe, ms));
        }
        let _ = RegCloseKey(key);
    }
    out
}

unsafe fn reg_str(root: HKEY, sub: &str, val: &str) -> Option<String> {
    let mut buf = vec![0u16; 1024];
    let mut len = (buf.len() * 2) as u32;
    RegGetValueW(root, &HSTRING::from(sub), &HSTRING::from(val), RRF_RT_REG_SZ, None, Some(buf.as_mut_ptr() as _), Some(&mut len))
        .ok()
        .ok()?;
    let n = (len as usize / 2).saturating_sub(1);
    Some(String::from_utf16_lossy(&buf[..n]))
}

unsafe fn reg_u64(root: HKEY, sub: &str, val: &str) -> Option<u64> {
    let mut v = 0u64;
    let mut len = 8u32;
    RegGetValueW(root, &HSTRING::from(sub), &HSTRING::from(val), RRF_RT_QWORD | RRF_RT_DWORD, None, Some(&mut v as *mut _ as _), Some(&mut len))
        .ok()
        .ok()?;
    Some(v)
}

// ---------------------------------------------------------------------------
// Steam / Epic / Riot
// ---------------------------------------------------------------------------

fn steam_path() -> Option<PathBuf> {
    unsafe { reg_str(HKEY_CURRENT_USER, r"Software\Valve\Steam", "SteamPath") }.map(|s| PathBuf::from(s.replace('/', "\\")))
}

fn steam_libraries() -> Vec<PathBuf> {
    let Some(steam) = steam_path() else { return vec![] };
    let mut libs = vec![steam.clone()];
    if let Ok(vdf) = std::fs::read_to_string(steam.join(r"steamapps\libraryfolders.vdf")) {
        let re = regex::Regex::new(r#""path"\s+"([^"]+)""#).unwrap();
        for c in re.captures_iter(&vdf) {
            let p = PathBuf::from(c[1].replace("\\\\", "\\"));
            if !libs.iter().any(|l| l.to_string_lossy().eq_ignore_ascii_case(&p.to_string_lossy())) {
                libs.push(p);
            }
        }
    }
    libs
}

/// (appid, nom) du jeu Steam installé dans `lib\steamapps\common\<dir>`
fn steam_manifest(lib: &Path, dir: &str) -> Option<(String, String)> {
    let re = regex::Regex::new(r#""(appid|name|installdir)"\s+"([^"]*)""#).unwrap();
    for e in std::fs::read_dir(lib.join("steamapps")).ok()?.flatten() {
        let n = e.file_name().to_string_lossy().to_string();
        if !n.starts_with("appmanifest_") {
            continue;
        }
        let Ok(txt) = std::fs::read_to_string(e.path()) else { continue };
        let mut f: HashMap<String, String> = HashMap::new();
        for c in re.captures_iter(&txt) {
            f.entry(c[1].to_string()).or_insert_with(|| c[2].to_string());
        }
        if f.get("installdir").map(|d| d.eq_ignore_ascii_case(dir)).unwrap_or(false) {
            return Some((f.get("appid")?.clone(), f.get("name")?.clone()));
        }
    }
    None
}

fn steam_art(appid: &str, files: &[&str], max: u32) -> Option<String> {
    let cache = steam_path()?.join(r"appcache\librarycache");
    for f in files {
        for p in [cache.join(appid).join(f), cache.join(format!("{appid}_{f}").replace("library_", ""))] {
            if let Ok(b) = std::fs::read(&p) {
                return media::shrink(&b, max);
            }
        }
    }
    None
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct EpicItem {
    display_name: String,
    install_location: String,
    catalog_namespace: String,
    catalog_item_id: String,
    app_name: String,
}

fn epic_game(lower: &str) -> Option<(String, String)> {
    let dir = Path::new(r"C:\ProgramData\Epic\EpicGamesLauncher\Data\Manifests");
    for e in std::fs::read_dir(dir).ok()?.flatten() {
        let Ok(txt) = std::fs::read_to_string(e.path()) else { continue };
        let Ok(it) = serde_json::from_str::<EpicItem>(&txt) else { continue };
        let loc = it.install_location.to_lowercase().replace('/', "\\");
        if !loc.is_empty() && lower.starts_with(&loc) {
            let uri = format!(
                "com.epicgames.launcher://apps/{}%3A{}%3A{}?action=launch&silent=true",
                it.catalog_namespace, it.catalog_item_id, it.app_name
            );
            return Some((it.display_name, uri));
        }
    }
    None
}

fn riot_client() -> Option<PathBuf> {
    let p = PathBuf::from(std::env::var("ProgramData").unwrap_or_else(|_| r"C:\ProgramData".into())).join(r"Riot Games\RiotClientInstalls.json");
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(p).ok()?).ok()?;
    let c = v.get("rc_default").or_else(|| v.get("rc_live"))?.as_str()?;
    let c = PathBuf::from(c.replace('/', "\\"));
    c.exists().then_some(c)
}

// ---------------------------------------------------------------------------
// Liste de détection de Discord
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct DiscordGame {
    id: String,
    name: String,
    icon: String,
    launcher: bool,
}

struct DiscordIndex {
    /// nom de fichier → [(suffixe de chemin, jeu)]
    by_file: HashMap<String, Vec<(String, u32)>>,
    games: Vec<DiscordGame>,
}

impl DiscordIndex {
    /// `lower` : chemin complet en minuscules. Discord donne la fin du chemin (« ship/hades2.exe »).
    fn find(&self, lower: &str) -> Option<&DiscordGame> {
        let norm = lower.replace('\\', "/");
        let file = file_name(&norm);
        let list = self.by_file.get(&file)?;
        list.iter()
            .filter(|(suffix, _)| norm == *suffix || norm.ends_with(&format!("/{suffix}")))
            .max_by_key(|(suffix, _)| suffix.len())
            .map(|(_, i)| &self.games[*i as usize])
    }
}

fn discord_cache() -> PathBuf {
    config::dir().join("discord_games.tsv")
}

/// Charge le cache local, puis le met à jour depuis Discord s'il a plus de 7 jours.
fn discord_refresh() {
    let path = discord_cache();
    if let Some(idx) = load_tsv(&path) {
        *DISCORD.lock() = Some(idx);
        RESOLVED.lock().clear();
    }
    let fresh = std::fs::metadata(&path)
        .and_then(|m| m.modified())
        .map(|t| SystemTime::now().duration_since(t).unwrap_or_default() < Duration::from_secs(7 * 86400))
        .unwrap_or(false);
    if fresh {
        return;
    }
    match download_discord(&path) {
        Ok(n) => {
            log(&format!("jeux : liste Discord mise à jour ({n} jeux)"));
            if let Some(idx) = load_tsv(&path) {
                *DISCORD.lock() = Some(idx);
                RESOLVED.lock().clear();
            }
        }
        Err(e) => log(&format!("jeux : liste Discord indisponible : {e}")),
    }
}

#[derive(Deserialize)]
struct DApp {
    id: String,
    name: String,
    #[serde(default)]
    icon_hash: Option<String>,
    #[serde(default)]
    executables: Vec<DExe>,
}
#[derive(Deserialize)]
struct DExe {
    name: String,
    os: String,
    #[serde(default)]
    is_launcher: bool,
}

const GENERIC: &[&str] = &["game.exe", "client.exe", "launcher.exe", "start.exe", "main.exe", "app.exe", "play.exe", "run.exe", "nw.exe", "electron.exe", "java.exe", "javaw.exe", "python.exe", "pythonw.exe", "unity.exe", "setup.exe", "update.exe"];

fn download_discord(path: &Path) -> Result<usize, String> {
    let resp = crate::http()
        .get("https://discord.com/api/v9/applications/detectable")
        .timeout(Duration::from_secs(90))
        .call()
        .map_err(|e| e.to_string())?;
    let apps: Vec<DApp> = serde_json::from_reader(std::io::BufReader::new(resp.into_reader())).map_err(|e| e.to_string())?;
    let mut out = String::with_capacity(2 << 20);
    let mut n = 0;
    for a in &apps {
        let name = a.name.replace(['\t', '\n', '\r'], " ");
        let mut any = false;
        for e in a.executables.iter().filter(|e| e.os == "win32") {
            let exe = e.name.trim_start_matches('>').to_lowercase().replace('\\', "/");
            if exe.is_empty() || (!exe.contains('/') && GENERIC.contains(&exe.as_str())) {
                continue;
            }
            out.push_str(&format!("{exe}\t{}\t{name}\t{}\t{}\n", a.id, a.icon_hash.as_deref().unwrap_or(""), e.is_launcher as u8));
            any = true;
        }
        n += any as usize;
    }
    std::fs::write(path, out).map_err(|e| e.to_string())?;
    Ok(n)
}

fn load_tsv(path: &Path) -> Option<DiscordIndex> {
    let txt = std::fs::read_to_string(path).ok()?;
    let mut idx = DiscordIndex { by_file: HashMap::new(), games: Vec::new() };
    let mut ids: HashMap<&str, u32> = HashMap::new();
    for line in txt.lines() {
        let f: Vec<&str> = line.split('\t').collect();
        if f.len() < 5 {
            continue;
        }
        let gi = *ids.entry(f[1]).or_insert_with(|| {
            idx.games.push(DiscordGame { id: f[1].into(), name: f[2].into(), icon: f[3].into(), launcher: false });
            (idx.games.len() - 1) as u32
        });
        // un même jeu peut avoir un exe « lanceur » et un exe de jeu : on garde l'info par exe
        let launcher = f[4] == "1";
        let gi = if launcher {
            idx.games.push(DiscordGame { id: f[1].into(), name: f[2].into(), icon: f[3].into(), launcher: true });
            (idx.games.len() - 1) as u32
        } else {
            gi
        };
        idx.by_file.entry(file_name(f[0])).or_default().push((f[0].to_string(), gi));
    }
    idx.by_file.shrink_to_fit();
    Some(idx)
}

/// Logo officiel (CDN Discord), mis en cache sur le disque.
fn discord_icon(g: &DiscordGame) -> Option<String> {
    if g.icon.is_empty() {
        return None;
    }
    let dir = config::dir().join("game_icons");
    let file = dir.join(format!("{}.png", g.id));
    let bytes = match std::fs::read(&file) {
        Ok(b) => b,
        Err(_) => {
            let url = format!("https://cdn.discordapp.com/app-icons/{}/{}.png?size=128", g.id, g.icon);
            let resp = crate::http().get(&url).timeout(Duration::from_secs(10)).call().ok()?;
            let mut b = Vec::new();
            let mut r = std::io::Read::take(resp.into_reader(), 2 << 20);
            std::io::Read::read_to_end(&mut r, &mut b).ok()?;
            let _ = std::fs::create_dir_all(&dir);
            let _ = std::fs::write(&file, &b);
            b
        }
    };
    media::shrink(&bytes, 96)
}

#[cfg(test)]
mod tests {
    #[test]
    fn detect_games() {
        super::discord_refresh();
        let k = super::Known::load();
        println!("store {} roots {:?}", k.store.len(), k.roots);
        let mut paths = Default::default();
        let r = super::find_running(&k, &mut paths);
        println!("running: {:?}", r.as_ref().map(|(g, l, p)| (g.name.clone(), g.key.clone(), g.icon.is_some(), l.clone(), p.clone())));
        let last = super::last_played(&k);
        println!("last: {:?}", last.map(|g| (g.name, g.key, g.icon.is_some(), g.cover.is_some())));
        for p in [
            r"D:\SteamLibrary\steamapps\common\Cyberpunk 2077\bin\x64\Cyberpunk2077.exe",
            r"D:\CRACKED\Hades II\Ship\Hades2.exe",
            r"D:\Jeux\Genshin Impact game\GenshinImpact.exe",
            r"D:\CRACKED\MiSide\Game\MiSide v16702566\MiSideFull.exe",
            r"D:\Games\WILD HEARTS\00_game\target_origin\ex\WILD HEARTS.exe",
            r"D:\LOL\Riot Games\League of Legends\LeagueClientUx.exe",
            r"D:\Jeux\FallGuys\FallGuys_client_game.exe",
        ] {
            let g = super::resolve(p);
            println!("{p} => {:?}", g.map(|(g, l)| (g.name, g.key, g.icon.is_some(), g.cover.is_some(), l)));
        }
    }
}
