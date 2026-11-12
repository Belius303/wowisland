//! Mettre une fenêtre au premier plan : l'appli de la musique (clic sur la pochette), ou l'île elle-même
//! le temps de taper dans la recherche du presse-papiers (elle ne prend jamais le focus sinon).

use once_cell::sync::Lazy;
use parking_lot::Mutex;
use windows::core::BOOL;
use windows::Win32::Foundation::{HWND, LPARAM};
use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
use windows::Win32::UI::WindowsAndMessaging::*;

/// Fenêtre active avant que l'île prenne le focus (on la lui rend ensuite).
static PREV: Lazy<Mutex<isize>> = Lazy::new(|| Mutex::new(0));

pub fn force_foreground(hwnd: HWND) {
    unsafe {
        if IsIconic(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_RESTORE);
        }
        let fg = GetForegroundWindow();
        let fg_tid = GetWindowThreadProcessId(fg, None);
        let me = GetCurrentThreadId();
        let attach = fg_tid != 0 && fg_tid != me;
        if attach {
            let _ = AttachThreadInput(me, fg_tid, true);
        }
        let _ = BringWindowToTop(hwnd);
        let _ = SetForegroundWindow(hwnd);
        if attach {
            let _ = AttachThreadInput(me, fg_tid, false);
        }
    }
}

/// Ouvre l'appli qui joue la musique : sa fenêtre si elle est déjà ouverte, sinon l'appli du Store.
pub fn open_media_app(id: &str, app: &str) -> Result<(), String> {
    let stem = |s: &str| s.trim_end_matches(".exe").trim_end_matches(".EXE").rsplit(['\\', '!']).next().unwrap_or("").to_lowercase();
    let mut names: Vec<String> = vec![];
    for n in [stem(id), app.to_lowercase().replace(' ', "")] {
        if !n.is_empty() && !names.contains(&n) {
            names.push(n);
        }
    }
    if let Some(h) = find_window(&names) {
        force_foreground(h);
        return Ok(());
    }
    if id.contains('!') {
        return crate::shell_open(&format!("shell:AppsFolder\\{id}"));
    }
    Err("Fenêtre de l'appli introuvable".into())
}

fn find_window(names: &[String]) -> Option<HWND> {
    struct Ctx<'a> {
        names: &'a [String],
        found: Option<HWND>,
    }
    unsafe extern "system" fn cb(h: HWND, l: LPARAM) -> BOOL {
        let ctx = &mut *(l.0 as *mut Ctx);
        if !IsWindowVisible(h).as_bool() || GetWindow(h, GW_OWNER).map(|o| !o.is_invalid()).unwrap_or(false) || GetWindowTextLengthW(h) == 0 {
            return true.into();
        }
        let ex = GetWindowLongPtrW(h, GWL_EXSTYLE) as u32;
        if ex & WS_EX_TOOLWINDOW.0 != 0 {
            return true.into();
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(h, Some(&mut pid));
        if let Some(path) = crate::audio::exe_path(pid) {
            let exe = path.rsplit('\\').next().unwrap_or("").trim_end_matches(".exe").trim_end_matches(".EXE").to_lowercase();
            if ctx.names.iter().any(|n| exe == *n || (n.len() >= 4 && exe.contains(n.as_str()))) {
                ctx.found = Some(h);
                return false.into();
            }
        }
        true.into()
    }
    let mut ctx = Ctx { names, found: None };
    unsafe {
        let _ = EnumWindows(Some(cb), LPARAM(&mut ctx as *mut _ as isize));
    }
    ctx.found
}

/// L'île accepte le clavier (on) ou redevient « sans focus » et rend la main à la fenêtre d'avant (off).
pub fn island_focus(hwnd: HWND, on: bool) {
    unsafe {
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        if on {
            let fg = GetForegroundWindow();
            if fg != hwnd {
                *PREV.lock() = fg.0 as isize;
            }
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex & !(WS_EX_NOACTIVATE.0 as isize));
            force_foreground(hwnd);
        } else {
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex | WS_EX_NOACTIVATE.0 as isize);
            let prev = std::mem::take(&mut *PREV.lock());
            if prev != 0 && GetForegroundWindow() == hwnd {
                force_foreground(HWND(prev as _));
            }
        }
    }
}
