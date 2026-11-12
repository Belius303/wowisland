//! Volume et luminosité affichés dans l'île (à la place de l'affichage de Windows pour le volume).
//! Touches volume : un hook clavier bas niveau les intercepte et règle le volume lui-même (±2 %, comme Windows),
//! donc le cadre de Windows n'apparaît pas. Si l'île ne répond plus, Windows ignore le hook et les touches
//! refonctionnent normalement. Luminosité (PC portable) : événement WMI, l'affichage de Windows reste.

use once_cell::sync::Lazy;
use parking_lot::Mutex;
use serde::Serialize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use tauri::{AppHandle, Emitter};
use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;

#[derive(Serialize, Clone)]
pub struct Osd {
    /// "volume" ou "brightness"
    pub kind: &'static str,
    /// 0..100
    pub value: u32,
    pub muted: bool,
}

static ENABLED: AtomicBool = AtomicBool::new(true);
static TX: Lazy<Mutex<Option<mpsc::Sender<u32>>>> = Lazy::new(Default::default);

const VK_MUTE: u32 = 0xAD;
const VK_DOWN: u32 = 0xAE;
const VK_UP: u32 = 0xAF;

pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::SeqCst);
}

pub fn start(app: AppHandle) {
    set_enabled(crate::config::get().osd);
    let (tx, rx) = mpsc::channel::<u32>();
    *TX.lock() = Some(tx);

    // réglage du volume hors du hook (le hook doit rendre la main tout de suite)
    let a = app.clone();
    std::thread::Builder::new()
        .name("osd-volume".into())
        .spawn(move || {
            while let Ok(vk) = rx.recv() {
                match crate::audio::key_step(vk) {
                    Ok((v, m)) => {
                        let _ = a.emit("osd", Osd { kind: "volume", value: (v * 100.0).round() as u32, muted: m });
                    }
                    Err(e) => crate::log(&format!("osd volume : {e}")),
                }
            }
        })
        .ok();

    std::thread::Builder::new()
        .name("osd-hook".into())
        .spawn(|| unsafe {
            let hmod = GetModuleHandleW(None).ok();
            let hook = SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook_proc), hmod.map(|h| h.into()), 0);
            if hook.is_err() {
                crate::log("osd : hook clavier refusé");
                return;
            }
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        })
        .ok();

    std::thread::Builder::new()
        .name("osd-brightness".into())
        .spawn(move || {
            if let Err(e) = brightness_loop(&app) {
                crate::log(&format!("osd luminosité : {e}"));
            }
        })
        .ok();
}

unsafe extern "system" fn hook_proc(code: i32, w: WPARAM, l: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 && ENABLED.load(Ordering::Relaxed) {
        let k = &*(l.0 as *const KBDLLHOOKSTRUCT);
        let injected = (k.flags.0 & LLKHF_INJECTED.0) != 0;
        if !injected && matches!(k.vkCode, VK_MUTE | VK_DOWN | VK_UP) {
            let msg = w.0 as u32;
            if msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN {
                if let Some(tx) = TX.lock().as_ref() {
                    let _ = tx.send(k.vkCode);
                }
            }
            // on avale la touche : pas de cadre de volume Windows
            return LRESULT(1);
        }
    }
    CallNextHookEx(None, code, w, l)
}

/// Attend les changements de luminosité (WmiMonitorBrightnessEvent). Absent sur un PC fixe : on sort.
fn brightness_loop(app: &AppHandle) -> windows::core::Result<()> {
    use windows::core::BSTR;
    use windows::Win32::System::Com::*;
    use windows::Win32::System::Variant::VARIANT;
    use windows::Win32::System::Wmi::*;
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let loc: IWbemLocator = CoCreateInstance(&WbemLocator, None, CLSCTX_INPROC_SERVER)?;
        let svc = loc.ConnectServer(&BSTR::from("ROOT\\WMI"), &BSTR::new(), &BSTR::new(), &BSTR::new(), 0, &BSTR::new(), None)?;
        CoSetProxyBlanket(&svc, 10, 0, None, RPC_C_AUTHN_LEVEL_CALL, RPC_C_IMP_LEVEL_IMPERSONATE, None, EOAC_NONE)?;
        let en = svc.ExecNotificationQuery(
            &BSTR::from("WQL"),
            &BSTR::from("SELECT * FROM WmiMonitorBrightnessEvent"),
            WBEM_FLAG_RETURN_IMMEDIATELY | WBEM_FLAG_FORWARD_ONLY,
            None,
        )?;
        let mut last: Option<u32> = None;
        loop {
            let mut objs: [Option<IWbemClassObject>; 1] = [None];
            let mut n = 0u32;
            let hr = en.Next(WBEM_INFINITE, &mut objs, &mut n);
            if hr.is_err() {
                return Err(hr.into());
            }
            let Some(obj) = objs[0].take() else { continue };
            let mut v = VARIANT::default();
            if obj.Get(windows::core::w!("Brightness"), 0, &mut v, None, None).is_err() {
                continue;
            }
            let b = variant_u32(&v);
            if Some(b) != last && ENABLED.load(Ordering::Relaxed) {
                last = Some(b);
                let _ = app.emit("osd", Osd { kind: "brightness", value: b.min(100), muted: false });
            }
        }
    }
}

fn variant_u32(v: &windows::Win32::System::Variant::VARIANT) -> u32 {
    use windows::Win32::System::Variant::*;
    unsafe {
        let inner = &v.Anonymous.Anonymous;
        match inner.vt {
            VT_UI1 => inner.Anonymous.bVal as u32,
            VT_I4 => inner.Anonymous.lVal.max(0) as u32,
            VT_UI4 => inner.Anonymous.ulVal,
            VT_I2 => inner.Anonymous.iVal.max(0) as u32,
            _ => 0,
        }
    }
}
