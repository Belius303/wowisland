//! Fenêtre overlay : position en haut au centre, styles Win32 (pas d'Alt-Tab, pas de vol de focus),
//! click-through hors de l'island (hit-test au curseur) et détection plein écran.

use once_cell::sync::Lazy;
use parking_lot::Mutex;
use serde::Deserialize;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager, PhysicalPosition, WebviewWindow};
use windows::Win32::Foundation::{HWND, POINT};
use windows::Win32::UI::Shell::{SHQueryUserNotificationState, QUERY_USER_NOTIFICATION_STATE};
use windows::Win32::UI::WindowsAndMessaging::{
    GetCursorPos, GetWindowLongPtrW, SetWindowLongPtrW, SetWindowPos, GWL_EXSTYLE, HWND_TOPMOST,
    SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
};

/// Rectangles interactifs en pixels CSS (logiques), relatifs à la fenêtre.
#[derive(Deserialize, Clone, Copy, Debug)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    /// 0 = l'île (le survol l'ouvre), 1 = zone cliquable qui n'ouvre pas l'île (flèches)
    #[serde(default)]
    pub zone: u8,
}

static RECTS: Lazy<Mutex<Vec<Rect>>> = Lazy::new(|| Mutex::new(vec![]));
/// Tant que > maintenant, la fenêtre reste interactive (ex. pendant une capture).
static SUSPEND_UNTIL: Lazy<Mutex<Option<Instant>>> = Lazy::new(|| Mutex::new(None));

pub fn set_rects(r: Vec<Rect>) {
    *RECTS.lock() = r;
}

pub fn hwnd_of(win: &WebviewWindow) -> HWND {
    HWND(win.hwnd().map(|h| h.0).unwrap_or(std::ptr::null_mut()))
}

pub fn setup(app: &AppHandle) -> tauri::Result<()> {
    let win = app.get_webview_window("island").expect("fenêtre island");
    position(&win)?;

    let hwnd = hwnd_of(&win);
    unsafe {
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex | (WS_EX_TOOLWINDOW.0 | WS_EX_NOACTIVATE.0) as isize);
    }
    win.set_ignore_cursor_events(true)?;
    win.show()?;

    let handle = app.clone();
    std::thread::Builder::new().name("overlay".into()).spawn(move || run(handle)).ok();
    Ok(())
}

/// Centre la fenêtre en haut de l'écran principal.
pub fn position(win: &WebviewWindow) -> tauri::Result<()> {
    if let Some(m) = win.primary_monitor()? {
        let size = win.outer_size()?;
        let x = m.position().x + (m.size().width as i32 - size.width as i32) / 2;
        let y = m.position().y;
        win.set_position(PhysicalPosition::new(x, y))?;
    }
    Ok(())
}

fn run(app: AppHandle) {
    let Some(win) = app.get_webview_window("island") else { return };
    let mut inside = false;
    let mut over_island = false;
    let mut last_fs_check = Instant::now() - Duration::from_secs(5);
    let mut fullscreen = false;
    let mut last_topmost = Instant::now();

    loop {
        std::thread::sleep(Duration::from_millis(16));

        // --- hit-test curseur ---
        let mut pt = POINT::default();
        if unsafe { GetCursorPos(&mut pt) }.is_err() {
            continue;
        }
        let (Ok(pos), Ok(scale)) = (win.outer_position(), win.scale_factor()) else { continue };
        let lx = (pt.x - pos.x) as f64 / scale;
        let ly = (pt.y - pos.y) as f64 / scale;
        let suspended = SUSPEND_UNTIL.lock().map(|t| t > Instant::now()).unwrap_or(false);
        let hit = RECTS.lock().iter().find(|r| lx >= r.x - 2.0 && lx <= r.x + r.w + 2.0 && ly >= r.y - 2.0 && ly <= r.y + r.h + 2.0).map(|r| r.zone);
        let now_inside = suspended || hit.is_some();
        let now_island = suspended || hit == Some(0);

        if now_inside != inside {
            inside = now_inside;
            let _ = win.set_ignore_cursor_events(!inside);
        }
        // les flèches restent cliquables sans ouvrir l'île
        if now_island != over_island {
            over_island = now_island;
            let _ = app.emit("hover", over_island);
        }

        // --- plein écran (jeux, vidéos, présentations) ---
        if last_fs_check.elapsed() > Duration::from_millis(800) {
            last_fs_check = Instant::now();
            let fs = is_fullscreen_app_active();
            if fs != fullscreen {
                fullscreen = fs;
                let _ = app.emit("fullscreen", fs);
            }
        }

        // Re-affirme le "topmost" de temps en temps (certaines applis le volent).
        if last_topmost.elapsed() > Duration::from_secs(3) && !fullscreen {
            last_topmost = Instant::now();
            unsafe {
                let _ = SetWindowPos(hwnd_of(&win), Some(HWND_TOPMOST), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
            }
        }
    }
}

fn is_fullscreen_app_active() -> bool {
    // 2 = QUNS_BUSY (appli plein écran), 3 = D3D plein écran, 4 = mode présentation
    match unsafe { SHQueryUserNotificationState() } {
        Ok(QUERY_USER_NOTIFICATION_STATE(s)) => matches!(s, 2 | 3 | 4),
        Err(_) => false,
    }
}
