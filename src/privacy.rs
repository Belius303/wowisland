//! Indicateur micro / caméra (comme les points orange et vert de l'iPhone).
//! Windows note dans le registre quelle appli utilise le micro ou la webcam
//! (CapabilityAccessManager\ConsentStore) : LastUsedTimeStop = 0 → en cours d'utilisation.

use serde::Serialize;
use std::collections::HashSet;
use std::time::Duration;
use tauri::{AppHandle, Emitter};
use windows::core::{HSTRING, PWSTR};
use windows::Win32::System::Registry::*;

#[derive(Serialize, Clone, PartialEq, Default)]
pub struct Privacy {
    pub mic: Vec<String>,
    pub cam: Vec<String>,
}

static CURRENT: once_cell::sync::Lazy<parking_lot::Mutex<Privacy>> = once_cell::sync::Lazy::new(Default::default);

/// État actuel (l'interface le demande au démarrage : l'événement initial peut partir avant qu'elle écoute).
pub fn state() -> Privacy {
    CURRENT.lock().clone()
}

const STORE: &str = r"Software\Microsoft\Windows\CurrentVersion\CapabilityAccessManager\ConsentStore";

pub fn start(app: AppHandle) {
    std::thread::Builder::new()
        .name("privacy".into())
        .spawn(move || {
            let mut last = Privacy::default();
            let mut first = true;
            loop {
                let running = running_exes();
                let now = Privacy { mic: in_use("microphone", &running), cam: in_use("webcam", &running) };
                if now != last || first {
                    *CURRENT.lock() = now.clone();
                    let _ = app.emit("privacy", &now);
                    last = now;
                    first = false;
                }
                std::thread::sleep(Duration::from_millis(1000));
            }
        })
        .ok();
}

/// Applis qui utilisent la capacité en ce moment.
fn in_use(cap: &str, running: &HashSet<String>) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    let base = format!(r"{STORE}\{cap}");
    // applis du Store (clé = nom du paquet)
    for (name, active) in children(&base) {
        if name == "NonPackaged" || !active {
            continue;
        }
        push(&mut out, packaged_name(&name));
    }
    // applis classiques (clé = chemin de l'exe avec # à la place de \)
    for (name, active) in children(&format!(r"{base}\NonPackaged")) {
        if !active {
            continue;
        }
        let exe = name.rsplit('#').next().unwrap_or(&name).to_lowercase();
        // une appli qui a planté peut laisser « en cours » : on vérifie qu'elle tourne encore
        if !running.contains(&exe) || exe == "dynamic-island.exe" {
            continue;
        }
        push(&mut out, pretty_exe(&exe));
    }
    out
}

fn push(v: &mut Vec<String>, s: String) {
    if !s.is_empty() && !v.contains(&s) {
        v.push(s);
    }
}

fn packaged_name(pkg: &str) -> String {
    let base = pkg.split('_').next().unwrap_or(pkg);
    let last = base.rsplit('.').next().unwrap_or(base);
    match last.to_lowercase().as_str() {
        "windowscamera" => "Caméra".into(),
        "windowssoundrecorder" => "Enregistreur vocal".into(),
        "yourphone" | "phonelink" => "Lien avec le téléphone".into(),
        "teams" | "msteams" => "Teams".into(),
        _ => last.to_string(),
    }
}

fn pretty_exe(exe: &str) -> String {
    let stem = exe.trim_end_matches(".exe");
    let known = [
        ("discord", "Discord"),
        ("zen", "Zen"),
        ("firefox", "Firefox"),
        ("chrome", "Chrome"),
        ("msedge", "Edge"),
        ("opera", "Opera"),
        ("brave", "Brave"),
        ("obs64", "OBS"),
        ("obs", "OBS"),
        ("zoom", "Zoom"),
        ("ms-teams", "Teams"),
        ("teams", "Teams"),
        ("steam", "Steam"),
        ("steamwebhelper", "Steam"),
        ("whatsapp", "WhatsApp"),
        ("telegram", "Telegram"),
        ("skype", "Skype"),
        ("slack", "Slack"),
    ];
    for (k, v) in known {
        if stem == k {
            return v.into();
        }
    }
    let mut c = stem.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

/// Sous-clés de `path` (HKCU) avec un indicateur « en cours d'utilisation ».
fn children(path: &str) -> Vec<(String, bool)> {
    let mut out = vec![];
    unsafe {
        let mut key = HKEY::default();
        if RegOpenKeyExW(HKEY_CURRENT_USER, &HSTRING::from(path), None, KEY_READ, &mut key).is_err() {
            return out;
        }
        let mut i = 0u32;
        loop {
            let mut buf = [0u16; 512];
            let mut len = buf.len() as u32;
            if RegEnumKeyExW(key, i, Some(PWSTR(buf.as_mut_ptr())), &mut len, None, None, None, None).is_err() {
                break;
            }
            i += 1;
            let child = String::from_utf16_lossy(&buf[..len as usize]);
            let start = reg_u64(key, &child, "LastUsedTimeStart").unwrap_or(0);
            let stop = reg_u64(key, &child, "LastUsedTimeStop");
            out.push((child, start > 0 && stop == Some(0)));
        }
        let _ = RegCloseKey(key);
    }
    out
}

unsafe fn reg_u64(root: HKEY, sub: &str, val: &str) -> Option<u64> {
    let mut v = 0u64;
    let mut len = 8u32;
    RegGetValueW(root, &HSTRING::from(sub), &HSTRING::from(val), RRF_RT_QWORD, None, Some(&mut v as *mut _ as _), Some(&mut len))
        .ok()
        .ok()?;
    Some(v)
}

/// Noms (en minuscules) des exécutables en cours.
pub fn running_exes() -> HashSet<String> {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Diagnostics::ToolHelp::*;
    let mut set = HashSet::new();
    unsafe {
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else { return set };
        let mut e = PROCESSENTRY32W { dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        if Process32FirstW(snap, &mut e).is_ok() {
            loop {
                let n = e.szExeFile.iter().position(|&c| c == 0).unwrap_or(e.szExeFile.len());
                set.insert(String::from_utf16_lossy(&e.szExeFile[..n]).to_lowercase());
                if Process32NextW(snap, &mut e).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snap);
    }
    set
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore]
    fn dump_privacy() {
        let r = super::running_exes();
        println!("mic {:?} cam {:?}", super::in_use("microphone", &r), super::in_use("webcam", &r));
    }
}
