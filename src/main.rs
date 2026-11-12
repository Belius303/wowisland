#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod alerts;
mod claude;
mod grab;
mod audio;
mod clipboard;
mod config;
mod discord;
mod focus;
mod localsend;
mod lyrics;
mod ocr;
mod osd;
mod winnotif;
mod downloads;
mod games;
mod gmail;
mod iphone;
mod lol;
mod media;
mod notifications;
mod otp;
mod overlay;
mod player;
mod privacy;
mod screenshot;
mod spotify;

use serde::{Deserialize, Serialize};
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, WindowEvent};

// ---------------- utilitaires ----------------

pub fn shell_open(target: &str) -> Result<(), String> {
    use windows::core::HSTRING;
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    let r = unsafe { ShellExecuteW(None, &HSTRING::from("open"), &HSTRING::from(target), None, None, SW_SHOWNORMAL) };
    if r.0 as isize > 32 { Ok(()) } else { Err(format!("Impossible d'ouvrir {target}")) }
}

/// Client HTTPS partagé (TLS natif de Windows).
pub fn http() -> ureq::Agent {
    let mut b = ureq::AgentBuilder::new().user_agent("WowIsland/0.1").timeout_connect(std::time::Duration::from_secs(6));
    if let Ok(tls) = native_tls::TlsConnector::new() {
        b = b.tls_connector(std::sync::Arc::new(tls));
    }
    b.build()
}

/// Journal léger dans %APPDATA%\DynamicIsland\log.txt (tronqué au-delà de 512 Ko).
pub fn log(msg: &str) {
    use std::io::Write;
    let path = config::dir().join("log.txt");
    if std::fs::metadata(&path).map(|m| m.len() > 512 * 1024).unwrap_or(false) {
        let _ = std::fs::remove_file(&path);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(f, "{} {msg}", chrono::Local::now().format("%Y-%m-%d %H:%M:%S"));
    }
}

/// Relance l'appli : on lance une nouvelle instance (qui attend la fin de celle-ci) puis on quitte.
fn restart(app: &AppHandle) {
    use std::os::windows::process::CommandExt;
    if let Ok(exe) = std::env::current_exe() {
        if std::process::Command::new(exe).arg("--wait").creation_flags(0x0000_0008 /* DETACHED_PROCESS */).spawn().is_ok() {
            app.exit(0);
            return;
        }
    }
    log("relance impossible");
}

/// Arguments WebView2 communs (doivent être identiques pour toutes les fenêtres) : moins de processus
/// et de services Edge inutiles pour une petite UI locale → moins de RAM.
pub const BROWSER_ARGS: &str = "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection,Translate,msEdgeTranslate,AutofillServerCommunication,msEdgeShopping,SpareRendererForSitePerProcess --disable-background-networking --disable-component-update --disable-sync --disable-default-apps --disable-extensions --no-pings --renderer-process-limit=1 --js-flags=--max-old-space-size=96";

/// Les réglages sont créés à la demande et détruits à la fermeture (pas de WebView qui dort en RAM).
fn show_settings(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("settings") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
        return;
    }
    let r = tauri::WebviewWindowBuilder::new(app, "settings", tauri::WebviewUrl::App("settings.html".into()))
        .title("WowIsland — Réglages")
        .inner_size(520.0, 760.0)
        .min_inner_size(440.0, 560.0)
        .center()
        .theme(Some(tauri::Theme::Dark))
        .additional_browser_args(BROWSER_ARGS)
        .build();
    if let Err(e) = r {
        log(&format!("réglages: {e}"));
    }
}

// ---------------- commandes appelées par l'UI ----------------

#[tauri::command]
fn set_island_rects(rects: Vec<overlay::Rect>) {
    overlay::set_rects(rects);
}

#[tauri::command]
async fn media_control(id: Option<String>, action: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || media::control(id.as_deref(), &action)).await.map_err(|e| e.to_string())?
}

#[tauri::command]
async fn media_seek(id: Option<String>, seconds: f64) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || media::seek(id.as_deref(), seconds)).await.map_err(|e| e.to_string())?
}

#[tauri::command]
async fn get_mixer() -> Result<audio::Mixer, String> {
    tauri::async_runtime::spawn_blocking(audio::snapshot).await.map_err(|e| e.to_string())?
}

#[tauri::command]
async fn set_device_volume(id: String, volume: Option<f32>, muted: Option<bool>) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || audio::set_device(&id, volume, muted)).await.map_err(|e| e.to_string())?
}

#[tauri::command]
async fn set_app_volume(key: String, volume: Option<f32>, muted: Option<bool>) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || audio::set_app(&key, volume, muted)).await.map_err(|e| e.to_string())?
}

#[tauri::command]
async fn iphone_diag() -> iphone::Diag {
    tauri::async_runtime::spawn_blocking(iphone::diag).await.unwrap_or_default()
}

#[tauri::command]
fn repair_iphone_service() -> Result<(), String> {
    iphone::repair_service()
}

#[tauri::command]
fn open_phone_link() -> Result<(), String> {
    iphone::open_phone_link()
}

#[tauri::command]
async fn spotify_state() -> spotify::State {
    tauri::async_runtime::spawn_blocking(spotify::state).await.unwrap_or_default()
}

#[tauri::command]
async fn spotify_set_volume(volume: u8) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || spotify::set_volume(volume)).await.map_err(|e| e.to_string())?
}

#[tauri::command]
async fn spotify_connect(app: AppHandle, client_id: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || spotify::connect(&client_id)).await.map_err(|e| e.to_string())??;
    let _ = app.emit("settings-changed", ());
    Ok(())
}

#[tauri::command]
fn spotify_disconnect(app: AppHandle) -> Result<(), String> {
    spotify::disconnect()?;
    let _ = app.emit("settings-changed", ());
    Ok(())
}

#[tauri::command]
async fn discord_connect(app: AppHandle, client_id: String, secret: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || discord::connect(&client_id, &secret)).await.map_err(|e| e.to_string())??;
    let _ = app.emit("settings-changed", ());
    Ok(())
}

#[tauri::command]
fn discord_disconnect(app: AppHandle) -> Result<(), String> {
    discord::disconnect()?;
    let _ = app.emit("settings-changed", ());
    Ok(())
}

#[tauri::command]
fn discord_info() -> serde_json::Value {
    let c = config::get();
    let s = discord::state();
    serde_json::json!({
        "configured": discord::is_configured(),
        "linked": discord::is_linked(),
        "live": s.linked,
        "client_id": c.discord_client_id,
        "has_secret": !c.discord_secret_enc.is_empty(),
        "redirect_uri": discord::REDIRECT_URI,
        "error": s.error,
    })
}

#[tauri::command]
fn discord_state() -> discord::Voice {
    discord::state()
}

#[tauri::command]
fn discord_action(action: String) -> Result<(), String> {
    discord::action(&action)
}

#[tauri::command]
fn grab_status() -> serde_json::Value {
    grab::status()
}

#[tauri::command]
fn grab_install(app: AppHandle) {
    grab::install(&app);
}

#[tauri::command]
async fn grab_probe(url: String) -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(move || grab::probe(&url)).await.map_err(|e| e.to_string())?
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
fn grab_start(app: AppHandle, url: String, kind: String, format: Option<u32>, start: Option<f64>, end: Option<f64>, playlist: bool, info: serde_json::Value) -> Result<u32, String> {
    grab::start(&app, url, kind, format, start, end, playlist, info)
}

#[tauri::command]
fn grab_jobs(app: AppHandle) -> Vec<grab::Job> {
    grab::set_app(&app);
    grab::jobs()
}

#[tauri::command]
fn grab_stop(id: u32) {
    grab::stop(id);
}

#[tauri::command]
fn grab_retry(app: AppHandle, id: u32) {
    grab::retry(&app, id);
}

#[tauri::command]
fn grab_clear() {
    grab::clear();
}

#[tauri::command]
fn grab_reveal(id: u32) -> Result<(), String> {
    grab::reveal(id)
}

#[tauri::command]
fn grab_open(id: u32) -> Result<(), String> {
    grab::open(id)
}

#[tauri::command]
fn grab_open_dir() -> Result<(), String> {
    grab::open_dir()
}

#[tauri::command]
async fn grab_spotify_like(id: u32) -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(move || grab::spotify_like(id)).await.map_err(|e| e.to_string())?
}

#[tauri::command]
async fn grab_remove(id: u32) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || grab::remove(id)).await.map_err(|e| e.to_string())?
}

#[tauri::command]
async fn music_delete(id: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || player::delete(&id)).await.map_err(|e| e.to_string())?
}

#[tauri::command]
async fn grab_music_add(id: u32) -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(move || grab::music_add(id)).await.map_err(|e| e.to_string())?
}

/// LocalSend : les tels (et PC) LocalSend du réseau.
#[tauri::command]
async fn phone_scan() -> Result<Vec<localsend::Device>, String> {
    tauri::async_runtime::spawn_blocking(localsend::scan).await.map_err(|e| e.to_string())
}

/// LocalSend : le tel choisi la dernière fois.
#[tauri::command]
fn phone_device() -> Option<localsend::Device> {
    localsend::remembered()
}

#[tauri::command]
fn phone_forget() {
    localsend::forget()
}

/// LocalSend : envoie un téléchargement sur le tel (`dev` = le tel choisi dans la liste, sinon le dernier).
#[tauri::command]
fn phone_send(id: u32, dev: Option<localsend::Device>) -> Result<(), String> {
    grab::phone_send(id, dev)
}

#[tauri::command]
fn phone_cancel(id: u32) {
    grab::phone_cancel(id)
}

/// Ouvre le dossier lu par Spotify et copie son chemin (à coller dans Spotify → Ajouter une source).
#[tauri::command]
fn grab_spotify_dir() -> Result<String, String> {
    let dir = grab::spotify_dir().to_string_lossy().to_string();
    let _ = alerts::copy_text(&dir);
    grab::spotify_dir_open()?;
    Ok(dir)
}

#[tauri::command]
async fn spotify_mode(shuffle: Option<bool>, repeat: Option<String>) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || spotify::set_mode(shuffle, repeat.as_deref())).await.map_err(|e| e.to_string())?
}

#[tauri::command]
async fn music_library() -> serde_json::Value {
    tauri::async_runtime::spawn_blocking(|| serde_json::json!({ "tracks": player::library(), "now": player::now() })).await.unwrap_or(serde_json::Value::Null)
}

#[tauri::command]
async fn music_cover(id: String) -> Option<String> {
    tauri::async_runtime::spawn_blocking(move || player::cover(&id)).await.ok().flatten()
}

#[tauri::command]
async fn music_play(ids: Vec<String>, start: usize, shuffle: bool) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || player::play(ids, start, shuffle)).await.map_err(|e| e.to_string())?
}

#[tauri::command]
fn music_folder() -> Result<(), String> {
    player::open_folder()
}

/// Volume de Ma musique (0-100) : le règle si `volume` est donné, renvoie la valeur actuelle.
#[tauri::command]
fn music_volume(volume: Option<u8>) -> u8 {
    if let Some(v) = volume {
        player::set_volume(v);
    }
    player::volume()
}

#[tauri::command]
fn notif_sound_preview() {
    winnotif::preview();
}

#[tauri::command]
fn open_spotify() -> Result<(), String> {
    shell_open("spotify:")
}

#[tauri::command]
async fn spotify_reauth(app: AppHandle) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(spotify::reconnect).await.map_err(|e| e.to_string())??;
    let _ = app.emit("settings-changed", ());
    Ok(())
}

#[tauri::command]
fn claude_send(app: AppHandle, prompt: String) -> Result<(), String> {
    claude::send(&app, prompt)
}

#[tauri::command]
fn claude_stop() {
    claude::stop();
}

#[tauri::command]
fn claude_reset() {
    claude::reset();
}

#[tauri::command]
async fn claude_status() -> serde_json::Value {
    tauri::async_runtime::spawn_blocking(claude::status).await.unwrap_or(serde_json::Value::Null)
}

#[tauri::command]
async fn claude_usage() -> Option<serde_json::Value> {
    tauri::async_runtime::spawn_blocking(claude::usage).await.ok().flatten()
}

#[tauri::command]
fn claude_login() -> Result<(), String> {
    claude::login()
}

#[tauri::command]
async fn lyrics(artist: String, title: String, album: String, duration: f64) -> Option<Vec<lyrics::Line>> {
    tauri::async_runtime::spawn_blocking(move || lyrics::get(&artist, &title, &album, duration)).await.ok().flatten()
}

#[tauri::command]
async fn media_open(id: String, app: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || focus::open_media_app(&id, &app)).await.map_err(|e| e.to_string())?
}

#[tauri::command]
async fn media_volume(id: String, app: String, delta: f32) -> Result<serde_json::Value, String> {
    let stem = id.trim_end_matches(".exe").rsplit(['\\', '!']).next().unwrap_or("").to_lowercase();
    let mut names = vec![];
    for n in [stem, app.to_lowercase().replace(' ', "")] {
        if !n.is_empty() && !names.contains(&n) {
            names.push(n);
        }
    }
    let (v, own) = tauri::async_runtime::spawn_blocking(move || audio::nudge_media(&names, delta)).await.map_err(|e| e.to_string())??;
    Ok(serde_json::json!({ "volume": v, "app": own }))
}

#[tauri::command]
fn island_focus(app: AppHandle, on: bool) {
    if let Some(w) = app.get_webview_window("island") {
        focus::island_focus(overlay::hwnd_of(&w), on);
        if on {
            let _ = w.set_focus();
        }
    }
}

#[tauri::command]
fn clip_pin(app: AppHandle, id: u32, on: bool) -> Result<(), String> {
    clipboard::pin(&app, id, on)
}

#[tauri::command]
fn spotify_info() -> serde_json::Value {
    let c = config::get();
    serde_json::json!({ "connected": spotify::is_connected(), "client_id": c.spotify_client_id, "redirect_uri": spotify::REDIRECT_URI })
}

#[tauri::command]
fn restart_app(app: AppHandle) {
    restart(&app);
}

#[tauri::command]
async fn screenshot(app: AppHandle, kind: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || match kind.as_str() {
        "full" => screenshot::full(&app),
        _ => screenshot::region(&app),
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
fn copy_text(text: String) -> Result<(), String> {
    alerts::copy_text(&text)
}

#[tauri::command]
fn open_url(url: String) -> Result<(), String> {
    if url.starts_with("https://") || url.starts_with("http://") {
        shell_open(&url)
    } else {
        Err("URL refusée".into())
    }
}

#[tauri::command]
fn open_url_any(url: String) -> Result<(), String> {
    if url.starts_with("ms-settings:") || url.starts_with("ms-windows-store:") {
        shell_open(&url)
    } else {
        Err("URL refusée".into())
    }
}

#[tauri::command]
fn gmail_status() -> Vec<gmail::GmailStatus> {
    gmail::statuses()
}

#[tauri::command]
fn open_screenshot(path: Option<String>) -> Result<(), String> {
    let dir = screenshot::shots_dir();
    match path {
        Some(p) if std::path::Path::new(&p).starts_with(&dir) && std::path::Path::new(&p).exists() => screenshot::open_in_paint(&p),
        _ => shell_open(&dir.to_string_lossy()),
    }
}

// async : créer une fenêtre depuis une commande synchrone peut bloquer sous Windows
#[tauri::command]
async fn open_settings(app: AppHandle) {
    let a = app.clone();
    let _ = app.run_on_main_thread(move || show_settings(&a));
}

#[tauri::command]
fn media_art(id: String) -> Option<serde_json::Value> {
    media::art(&id).map(|(key, art)| serde_json::json!({ "key": key, "art": art }))
}

#[tauri::command]
fn clear_notifications(ids: Vec<u32>) {
    notifications::remove(&ids);
}

#[tauri::command]
fn privacy_state() -> privacy::Privacy {
    privacy::state()
}

#[tauri::command]
fn clip_list() -> Vec<clipboard::Clip> {
    clipboard::list()
}

#[tauri::command]
async fn clip_copy(id: u32) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || clipboard::restore(id)).await.map_err(|e| e.to_string())?
}

#[tauri::command]
fn clip_clear(app: AppHandle) {
    clipboard::clear(&app);
}

#[tauri::command]
fn clip_remove(app: AppHandle, id: u32) {
    clipboard::remove(&app, id);
}

#[tauri::command]
fn reveal_download(path: String) -> Result<(), String> {
    downloads::reveal(&path)
}

#[tauri::command]
fn game_state() -> games::GameState {
    games::state()
}

#[tauri::command]
fn lol_state() -> Option<lol::Coach> {
    lol::state()
}

#[tauri::command]
async fn launch_game(key: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || games::launch(&key)).await.map_err(|e| e.to_string())?
}

#[derive(Serialize, Deserialize)]
struct SettingsDto {
    accounts: Vec<String>,
    auto_copy_otp: bool,
    show_pc_notifications: bool,
    show_all_mails: bool,
    hide_in_fullscreen: bool,
    clear_otp_clipboard: bool,
    notif_blocklist: Vec<String>,
    autostart: bool,
    #[serde(default)]
    show_timer: bool,
    #[serde(default = "yes")]
    osd: bool,
    #[serde(default)]
    lyrics_compact: bool,
    #[serde(default = "yes")]
    notif_island_only: bool,
    #[serde(default = "yes")]
    notif_sound: bool,
    #[serde(default)]
    hidden_tools: Vec<String>,
    #[serde(default)]
    claude_dir: String,
    /// "fr", "en" ou "" (langue de Windows)
    #[serde(default)]
    lang: String,
}

fn yes() -> bool {
    true
}

#[tauri::command]
fn get_settings() -> SettingsDto {
    let c = config::get();
    SettingsDto {
        accounts: c.gmail.iter().map(|a| a.email.clone()).collect(),
        auto_copy_otp: c.auto_copy_otp,
        show_pc_notifications: c.show_pc_notifications,
        show_all_mails: c.show_all_mails,
        hide_in_fullscreen: c.hide_in_fullscreen,
        clear_otp_clipboard: c.clear_otp_clipboard,
        notif_blocklist: c.notif_blocklist,
        autostart: autostart_enabled(),
        show_timer: c.show_timer,
        osd: c.osd,
        lyrics_compact: c.lyrics_compact,
        notif_island_only: c.notif_island_only,
        notif_sound: c.notif_sound,
        hidden_tools: c.hidden_tools,
        claude_dir: c.claude_dir,
        lang: c.lang,
    }
}

#[tauri::command]
fn save_settings(app: AppHandle, s: SettingsDto) -> Result<(), String> {
    osd::set_enabled(s.osd);
    let autostart = s.autostart;
    let lang_changed = config::get().lang != s.lang;
    config::update(|c| {
        c.auto_copy_otp = s.auto_copy_otp;
        c.show_pc_notifications = s.show_pc_notifications;
        c.show_all_mails = s.show_all_mails;
        c.hide_in_fullscreen = s.hide_in_fullscreen;
        c.clear_otp_clipboard = s.clear_otp_clipboard;
        c.notif_blocklist = s.notif_blocklist;
        c.show_timer = s.show_timer;
        c.osd = s.osd;
        c.lyrics_compact = s.lyrics_compact;
        c.notif_island_only = s.notif_island_only;
        c.notif_sound = s.notif_sound;
        c.hidden_tools = s.hidden_tools;
        c.claude_dir = s.claude_dir.trim().to_string();
        c.lang = s.lang;
    })?;
    std::thread::spawn(|| winnotif::sync(false));
    set_autostart(autostart)?;
    if lang_changed {
        if let (Some(t), Ok(m)) = (app.tray_by_id("main"), tray_menu(&app)) {
            let _ = t.set_menu(Some(m));
        }
    }
    let _ = app.emit("settings-changed", ());
    Ok(())
}

#[tauri::command]
async fn add_gmail(app: AppHandle, email: String, password: String) -> Result<(), String> {
    let email = email.trim().to_lowercase();
    // Google affiche le mot de passe d'application par groupes de 4 : on retire les espaces.
    let password: String = password.chars().filter(|c| !c.is_whitespace()).collect();
    let (e2, p2) = (email.clone(), password.clone());
    tauri::async_runtime::spawn_blocking(move || gmail::test_login(&e2, &p2)).await.map_err(|e| e.to_string())??;
    let enc = config::protect(&password)?;
    config::update(|c| {
        c.gmail.retain(|a| a.email != email);
        c.gmail.push(config::GmailAccount { email, password_enc: enc });
    })?;
    gmail::restart(&app);
    Ok(())
}

#[tauri::command]
fn remove_gmail(app: AppHandle, email: String) -> Result<(), String> {
    config::update(|c| c.gmail.retain(|a| a.email != email))?;
    gmail::restart(&app);
    Ok(())
}

#[tauri::command]
fn get_ui_config() -> serde_json::Value {
    let c = config::get();
    serde_json::json!({ "hide_in_fullscreen": c.hide_in_fullscreen, "spotify": spotify::is_connected(), "show_timer": c.show_timer, "lyrics_compact": c.lyrics_compact, "hidden_tools": c.hidden_tools, "lang": config::lang() })
}

#[tauri::command]
fn open_notification_settings() -> Result<(), String> {
    shell_open("ms-settings:privacy-notifications")
}

// ---------------- démarrage automatique (HKCU\...\Run) ----------------

const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";

fn reg(args: &[&str]) -> std::io::Result<std::process::Output> {
    use std::os::windows::process::CommandExt;
    std::process::Command::new("reg").args(args).creation_flags(0x0800_0000).output()
}

fn autostart_enabled() -> bool {
    reg(&["query", RUN_KEY, "/v", "DynamicIsland"]).map(|o| o.status.success()).unwrap_or(false)
}

/// Exe à lancer au démarrage de Windows : la copie déployée (target\release), même si l'île a été lancée
/// depuis le dossier de compilation (target\build-new\release), qui est réécrit à chaque compilation.
fn autostart_exe() -> std::io::Result<std::path::PathBuf> {
    let exe = std::env::current_exe()?;
    let deployed = exe
        .parent()
        .filter(|rel| rel.file_name().map(|n| n == "release").unwrap_or(false))
        .and_then(|rel| rel.parent())
        .filter(|b| b.file_name().map(|n| n == "build-new").unwrap_or(false))
        .and_then(|b| b.parent())
        .zip(exe.file_name())
        .map(|(target, name)| target.join("release").join(name))
        .filter(|p| p.is_file());
    Ok(deployed.unwrap_or(exe))
}

fn set_autostart(on: bool) -> Result<(), String> {
    if on {
        let exe = autostart_exe().map_err(|e| e.to_string())?;
        let val = format!("\"{}\"", exe.display());
        reg(&["add", RUN_KEY, "/v", "DynamicIsland", "/t", "REG_SZ", "/d", &val, "/f"]).map_err(|e| e.to_string())?;
    } else if autostart_enabled() {
        reg(&["delete", RUN_KEY, "/v", "DynamicIsland", "/f"]).map_err(|e| e.to_string())?;
    }
    Ok(())
}

// ---------------- instance unique ----------------

fn already_running() -> bool {
    use windows::core::w;
    use windows::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS};
    use windows::Win32::System::Threading::CreateMutexW;
    // Lors d'une relance, l'ancienne instance met un instant à se fermer : on patiente jusqu'à ~5 s.
    let tries = if std::env::args().any(|a| a == "--wait") { 50 } else { 1 };
    for i in 0..tries {
        unsafe {
            let Ok(h) = CreateMutexW(None, true, w!("Local\\DynamicIslandRustSingleton")) else { return false };
            // Le mutex ne suffit pas : une île lancée depuis une appli empaquetée (MSIX) peut avoir ses objets
            // nommés à part. On regarde donc aussi s'il existe un autre processus « dynamic-island.exe ».
            if GetLastError() != ERROR_ALREADY_EXISTS && !other_island_process() {
                // On "fuit" volontairement le handle : il vit autant que le processus.
                return false;
            }
            let _ = CloseHandle(h);
        }
        if i + 1 < tries {
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }
    true
}

/// Un autre processus portant le même nom d'exe que celui-ci tourne-t-il ?
fn other_island_process() -> bool {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Diagnostics::ToolHelp::*;
    let me = std::process::id();
    let Some(name) = std::env::current_exe().ok().and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_string())) else { return false };
    unsafe {
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else { return false };
        let mut e = PROCESSENTRY32W { dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        let mut found = false;
        let mut ok = Process32FirstW(snap, &mut e).is_ok();
        while ok {
            let len = e.szExeFile.iter().position(|&c| c == 0).unwrap_or(e.szExeFile.len());
            if e.th32ProcessID != me && String::from_utf16_lossy(&e.szExeFile[..len]).eq_ignore_ascii_case(&name) {
                found = true;
                break;
            }
            ok = Process32NextW(snap, &mut e).is_ok();
        }
        let _ = CloseHandle(snap);
        found
    }
}

/// Étape du démarrage en cours (pour savoir où il bloque si l'île démarre mal).
static BOOT_STEP: once_cell::sync::Lazy<parking_lot::Mutex<&'static str>> = once_cell::sync::Lazy::new(|| parking_lot::Mutex::new("main"));
static BOOT_DONE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn step(s: &'static str) {
    *BOOT_STEP.lock() = s;
}

fn main() {
    let t0 = std::time::Instant::now();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let exe = std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_default();
    log(&format!("lancement : pid {} {exe} {args:?}", std::process::id()));
    if already_running() {
        log("lancement : une autre instance tourne déjà, on s'arrête");
        return;
    }
    // Garde-fou : si le démarrage n'est pas fini au bout de 20 s, on note l'étape bloquante.
    std::thread::spawn(move || {
        for _ in 0..4 {
            std::thread::sleep(std::time::Duration::from_secs(20));
            if BOOT_DONE.load(std::sync::atomic::Ordering::SeqCst) {
                return;
            }
            log(&format!("démarrage lent : bloqué à l'étape « {} » depuis {} s", *BOOT_STEP.lock(), t0.elapsed().as_secs()));
        }
    });
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            set_island_rects,
            media_control,
            media_seek,
            screenshot,
            copy_text,
            open_url,
            open_url_any,
            gmail_status,
            open_screenshot,
            open_settings,
            get_settings,
            save_settings,
            add_gmail,
            remove_gmail,
            get_ui_config,
            open_notification_settings,
            get_mixer,
            set_device_volume,
            set_app_volume,
            iphone_diag,
            repair_iphone_service,
            open_phone_link,
            restart_app,
            spotify_state,
            spotify_set_volume,
            spotify_connect,
            spotify_disconnect,
            spotify_info,
            discord_connect,
            discord_disconnect,
            discord_info,
            discord_state,
            discord_action,
            claude_send,
            grab_status,
            grab_install,
            grab_probe,
            grab_start,
            grab_jobs,
            grab_stop,
            grab_retry,
            grab_clear,
            grab_reveal,
            grab_open,
            grab_open_dir,
            grab_spotify_like,
            grab_spotify_dir,
            music_library,
            spotify_mode,
            music_cover,
            music_play,
            music_folder,
            music_volume,
            grab_music_add,
            phone_scan,
            phone_device,
            phone_forget,
            phone_send,
            phone_cancel,
            grab_remove,
            music_delete,
            open_spotify,
            notif_sound_preview,
            spotify_reauth,
            claude_stop,
            claude_reset,
            claude_status,
            claude_usage,
            claude_login,
            lyrics,
            media_open,
            media_volume,
            island_focus,
            clip_pin,
            media_art,
            clear_notifications,
            game_state,
            lol_state,
            launch_game,
            clip_list,
            privacy_state,
            clip_copy,
            clip_clear,
            clip_remove,
            reveal_download,
        ])
        .on_window_event(|window, event| {
            // Fermer l'island (Alt+F4) ne doit pas quitter : on ne ferme que les réglages.
            if let WindowEvent::CloseRequested { api, .. } = event {
                if window.label() == "island" {
                    api.prevent_close();
                }
            }
        })
        .setup(move |app| {
            let handle = app.handle().clone();
            log(&format!("démarrage : fenêtre prête après {} ms", t0.elapsed().as_millis()));
            // Les comptes d'abord : rien de ce qui suit (fenêtre, icône, modules) ne doit les empêcher de se charger.
            step("config");
            config::ensure_backup();
            config::startup_report();
            step("gmail");
            gmail::restart(&handle);

            step("fenêtre de l'île");
            overlay::setup(&handle)?;

            // Icône dans la zone de notification. Juste après l'ouverture de session, la barre des tâches
            // n'est parfois pas encore prête : on réessaie en arrière-plan au lieu d'abandonner le démarrage.
            step("icône");
            if let Err(e) = build_tray(&handle) {
                log(&format!("icône de notification : {e}, nouvel essai dans 5 s"));
                let h = handle.clone();
                std::thread::spawn(move || {
                    for _ in 0..12 {
                        std::thread::sleep(std::time::Duration::from_secs(5));
                        let (tx, rx) = std::sync::mpsc::channel();
                        let h2 = h.clone();
                        let _ = h.run_on_main_thread(move || { let _ = tx.send(build_tray(&h2).map_err(|e| e.to_string())); });
                        if let Ok(Ok(())) = rx.recv_timeout(std::time::Duration::from_secs(10)) {
                            log("icône de notification : ok");
                            return;
                        }
                    }
                });
            }

            // Si le démarrage auto est actif, on pointe vers l'exécutable actuel (ex. passage debug → release).
            // (reg.exe peut être lent à l'ouverture de session : hors du fil principal)
            std::thread::spawn(|| {
                if autostart_enabled() {
                    let _ = set_autostart(true);
                }
            });

            step("modules");
            media::start(handle.clone());
            player::init(handle.clone());
            notifications::start(handle.clone());
            iphone::start(handle.clone());
            games::start(handle.clone());
            lol::start(handle.clone());
            privacy::start(handle.clone());
            clipboard::start(handle.clone());
            downloads::start(handle.clone());
            discord::start(handle.clone());
            osd::start(handle.clone());
            std::thread::spawn(winnotif::start);
            if config::get().gmail.is_empty() {
                show_settings(&handle);
            }
            BOOT_DONE.store(true, std::sync::atomic::Ordering::SeqCst);
            log(&format!("démarrage terminé en {} ms", t0.elapsed().as_millis()));
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("erreur au lancement de WowIsland");
}

/// Menu de l'icône de la barre des tâches, dans la langue choisie.
fn tray_menu(app: &AppHandle) -> tauri::Result<Menu<tauri::Wry>> {
    let fr = config::lang() == "fr";
    let t = |a: &'static str, b: &'static str| if fr { a } else { b };
    let settings_i = MenuItem::with_id(app, "settings", t("Réglages…", "Settings…"), true, None::<&str>)?;
    let shots_i = MenuItem::with_id(app, "shots", t("Ouvrir le dossier des captures", "Open screenshots folder"), true, None::<&str>)?;
    let recenter_i = MenuItem::with_id(app, "recenter", t("Recentrer l'island", "Re-center the island"), true, None::<&str>)?;
    let restart_i = MenuItem::with_id(app, "restart", t("Relancer WowIsland", "Restart WowIsland"), true, None::<&str>)?;
    let quit_i = MenuItem::with_id(app, "quit", t("Quitter", "Quit"), true, None::<&str>)?;
    let sep = PredefinedMenuItem::separator(app)?;
    Menu::with_items(app, &[&settings_i, &shots_i, &recenter_i, &sep, &restart_i, &quit_i])
}

fn build_tray(app: &AppHandle) -> tauri::Result<()> {
    if app.tray_by_id("main").is_some() {
        return Ok(());
    }
    {
            let menu = tray_menu(app)?;
            TrayIconBuilder::with_id("main")
                .icon(app.default_window_icon().cloned().expect("icône"))
                .tooltip("WowIsland")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, e| match e.id.as_ref() {
                    "settings" => show_settings(app),
                    "shots" => { let _ = shell_open(&screenshot::shots_dir().to_string_lossy()); }
                    "recenter" => {
                        if let Some(w) = app.get_webview_window("island") { let _ = overlay::position(&w); }
                    }
                    "restart" => restart(app),
                    "quit" => app.exit(0),
                    _ => {}
                })
                .on_tray_icon_event(|tray, e| {
                    if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = e {
                        show_settings(tray.app_handle());
                    }
                })
                .build(app)?;
    }
    Ok(())
}
