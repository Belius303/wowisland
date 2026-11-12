//! Mixeur audio (WASAPI) : volume / sourdine de chaque sortie audio active et de chaque appli
//! (Spotify, navigateur du live, Discord…), toutes sorties confondues.

use base64::{engine::general_purpose::STANDARD as B64, Engine};
use once_cell::sync::Lazy;
use parking_lot::Mutex;
use serde::Serialize;
use std::collections::HashMap;
use windows::core::{Interface, GUID, HSTRING, PWSTR};
use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Foundation::{CloseHandle, S_OK};
use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
use windows::Win32::Media::Audio::*;
use windows::Win32::System::Com::*;
use windows::Win32::System::Threading::{OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION};

#[derive(Serialize, Clone)]
pub struct Device {
    pub id: String,
    pub name: String,
    pub volume: f32,
    pub muted: bool,
    pub default: bool,
}

#[derive(Serialize, Clone)]
pub struct AppVol {
    /// chemin de l'exécutable (clé de regroupement)
    pub key: String,
    pub name: String,
    pub volume: f32,
    pub muted: bool,
    pub active: bool,
    pub icon: Option<String>,
}

#[derive(Serialize, Clone, Default)]
pub struct Mixer {
    pub devices: Vec<Device>,
    pub apps: Vec<AppVol>,
}

static ICONS: Lazy<Mutex<HashMap<String, Option<String>>>> = Lazy::new(Default::default);
static NAMES: Lazy<Mutex<HashMap<String, String>>> = Lazy::new(Default::default);

fn com() {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
}

fn enumerator() -> windows::core::Result<IMMDeviceEnumerator> {
    com();
    unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }
}

fn pwstr_take(p: PWSTR) -> String {
    unsafe {
        let s = p.to_string().unwrap_or_default();
        CoTaskMemFree(Some(p.0 as _));
        s
    }
}

fn devices(e: &IMMDeviceEnumerator) -> windows::core::Result<Vec<IMMDevice>> {
    unsafe {
        let coll = e.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)?;
        let mut v = vec![];
        for i in 0..coll.GetCount()? {
            v.push(coll.Item(i)?);
        }
        Ok(v)
    }
}

/// Parcourt toutes les sessions audio de toutes les sorties actives.
fn for_each_session(e: &IMMDeviceEnumerator, mut f: impl FnMut(&IAudioSessionControl2, &ISimpleAudioVolume, AudioSessionState)) -> windows::core::Result<()> {
    unsafe {
        for dev in devices(e)? {
            let Ok(mgr) = dev.Activate::<IAudioSessionManager2>(CLSCTX_ALL, None) else { continue };
            let Ok(list) = mgr.GetSessionEnumerator() else { continue };
            for j in 0..list.GetCount().unwrap_or(0) {
                let Ok(ctl) = list.GetSession(j) else { continue };
                let Ok(ctl2) = ctl.cast::<IAudioSessionControl2>() else { continue };
                if ctl2.IsSystemSoundsSession() == S_OK {
                    continue;
                }
                let state = ctl.GetState().unwrap_or(AudioSessionStateExpired);
                if state == AudioSessionStateExpired {
                    continue;
                }
                let Ok(vol) = ctl.cast::<ISimpleAudioVolume>() else { continue };
                f(&ctl2, &vol, state);
            }
        }
    }
    Ok(())
}

pub fn exe_path(pid: u32) -> Option<String> {
    if pid == 0 {
        return None;
    }
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len).is_ok();
        let _ = CloseHandle(h);
        ok.then(|| String::from_utf16_lossy(&buf[..len as usize]))
    }
}

pub fn snapshot() -> Result<Mixer, String> {
    let e = enumerator().map_err(|x| x.to_string())?;
    let mut mixer = Mixer::default();
    unsafe {
        let default_id = e.GetDefaultAudioEndpoint(eRender, eMultimedia).and_then(|d| d.GetId()).map(pwstr_take).unwrap_or_default();
        for dev in devices(&e).map_err(|x| x.to_string())? {
            let id = dev.GetId().map(pwstr_take).unwrap_or_default();
            let name = dev
                .OpenPropertyStore(STGM_READ)
                .and_then(|s| s.GetValue(&PKEY_Device_FriendlyName))
                .map(|v| v.to_string())
                .unwrap_or_else(|_| "Sortie audio".into());
            let Ok(ev) = dev.Activate::<IAudioEndpointVolume>(CLSCTX_ALL, None) else { continue };
            mixer.devices.push(Device {
                default: id == default_id,
                id,
                name: tidy_device_name(&name),
                volume: ev.GetMasterVolumeLevelScalar().unwrap_or(0.0),
                muted: ev.GetMute().map(|b| b.as_bool()).unwrap_or(false),
            });
        }
    }
    mixer.devices.sort_by_key(|d| !d.default);

    let mut apps: HashMap<String, AppVol> = HashMap::new();
    let _ = for_each_session(&e, |ctl2, vol, state| unsafe {
        let pid = ctl2.GetProcessId().unwrap_or(0);
        let Some(path) = exe_path(pid) else { return };
        let key = path.to_lowercase();
        let active = state == AudioSessionStateActive;
        let volume = vol.GetMasterVolume().unwrap_or(1.0);
        let muted = vol.GetMute().map(|b| b.as_bool()).unwrap_or(false);
        let entry = apps.entry(key.clone()).or_insert_with(|| AppVol {
            name: app_name(&path),
            icon: app_icon(&path),
            key,
            volume,
            muted,
            active,
        });
        if active && !entry.active {
            entry.active = true;
            entry.volume = volume;
            entry.muted = muted;
        }
    });
    let mut apps: Vec<AppVol> = apps.into_values().filter(|a| !a.name.eq_ignore_ascii_case("Dynamic Island") && !a.name.eq_ignore_ascii_case("WowIsland")).collect();
    apps.sort_by(|a, b| b.active.cmp(&a.active).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    apps.truncate(8);
    mixer.apps = apps;
    Ok(mixer)
}

pub fn set_device(id: &str, volume: Option<f32>, muted: Option<bool>) -> Result<(), String> {
    let e = enumerator().map_err(|x| x.to_string())?;
    unsafe {
        let dev = e.GetDevice(&HSTRING::from(id)).map_err(|x| x.to_string())?;
        let ev = dev.Activate::<IAudioEndpointVolume>(CLSCTX_ALL, None).map_err(|x| x.to_string())?;
        if let Some(v) = volume {
            ev.SetMasterVolumeLevelScalar(v.clamp(0.0, 1.0), &GUID::zeroed()).map_err(|x| x.to_string())?;
            if v > 0.0 {
                let _ = ev.SetMute(false, &GUID::zeroed());
            }
        }
        if let Some(m) = muted {
            ev.SetMute(m, &GUID::zeroed()).map_err(|x| x.to_string())?;
        }
    }
    Ok(())
}

pub fn set_app(key: &str, volume: Option<f32>, muted: Option<bool>) -> Result<(), String> {
    let e = enumerator().map_err(|x| x.to_string())?;
    for_each_session(&e, |ctl2, vol, _| unsafe {
        let pid = ctl2.GetProcessId().unwrap_or(0);
        if exe_path(pid).map(|p| p.to_lowercase() == key).unwrap_or(false) {
            if let Some(v) = volume {
                let _ = vol.SetMasterVolume(v.clamp(0.0, 1.0), &GUID::zeroed());
                if v > 0.0 {
                    let _ = vol.SetMute(false, &GUID::zeroed());
                }
            }
            if let Some(m) = muted {
                let _ = vol.SetMute(m, &GUID::zeroed());
            }
        }
    })
    .map_err(|x| x.to_string())
}

fn tidy_device_name(n: &str) -> String {
    // "Haut-parleurs (Realtek(R) Audio)" → "Haut-parleurs · Realtek(R) Audio"
    match n.find(" (") {
        Some(i) if n.ends_with(')') => format!("{} · {}", &n[..i], &n[i + 2..n.len() - 1]),
        _ => n.to_string(),
    }
}

/// Nom lisible : description du fichier (ex. "Spotify", "Google Chrome"), sinon nom de l'exe.
fn app_name(path: &str) -> String {
    if let Some(n) = NAMES.lock().get(path) {
        return n.clone();
    }
    let name = file_description(path).filter(|s| !s.trim().is_empty()).unwrap_or_else(|| {
        let stem = std::path::Path::new(path).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let mut c = stem.chars();
        c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
    });
    NAMES.lock().insert(path.to_string(), name.clone());
    name
}

fn file_description(path: &str) -> Option<String> {
    version_string(path, "FileDescription")
}

/// Champ texte des infos de version d'un exécutable (ProductName, FileDescription…).
pub fn version_string(path: &str, field: &str) -> Option<String> {
    use windows::Win32::Storage::FileSystem::{GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW};
    unsafe {
        let p = HSTRING::from(path);
        let size = GetFileVersionInfoSizeW(&p, None);
        if size == 0 {
            return None;
        }
        let mut data = vec![0u8; size as usize];
        GetFileVersionInfoW(&p, None, size, data.as_mut_ptr() as _).ok()?;
        let mut ptr = std::ptr::null_mut();
        let mut len = 0u32;
        if !VerQueryValueW(data.as_ptr() as _, &HSTRING::from("\\VarFileInfo\\Translation"), &mut ptr, &mut len).as_bool() || len < 4 {
            return None;
        }
        let lang = *(ptr as *const u16);
        let cp = *((ptr as *const u16).add(1));
        let key = format!("\\StringFileInfo\\{lang:04x}{cp:04x}\\{field}");
        if !VerQueryValueW(data.as_ptr() as _, &HSTRING::from(key), &mut ptr, &mut len).as_bool() || len == 0 {
            return None;
        }
        let slice = std::slice::from_raw_parts(ptr as *const u16, len as usize);
        let s = String::from_utf16_lossy(slice).trim_end_matches('\0').trim().to_string();
        Some(s)
    }
}

/// Icône de l'exécutable en PNG base64 (mise en cache).
fn app_icon(path: &str) -> Option<String> {
    if let Some(i) = ICONS.lock().get(path) {
        return i.clone();
    }
    let icon = extract_icon(path);
    ICONS.lock().insert(path.to_string(), icon.clone());
    icon
}

fn extract_icon(path: &str) -> Option<String> {
    use windows::Win32::UI::Shell::ExtractIconExW;
    use windows::Win32::UI::WindowsAndMessaging::HICON;
    unsafe {
        let mut large = HICON::default();
        if ExtractIconExW(&HSTRING::from(path), 0, Some(&mut large), None, 1) == 0 || large.is_invalid() {
            return None;
        }
        hicon_png(large, 0)
    }
}

/// Grande icône d'un exécutable (jusqu'à 256 px), réduite à `max` px, en PNG base64.
pub fn exe_icon(path: &str, max: u32) -> Option<String> {
    use windows::Win32::UI::Shell::SHDefExtractIconW;
    use windows::Win32::UI::WindowsAndMessaging::HICON;
    unsafe {
        let mut large = HICON::default();
        let ok = SHDefExtractIconW(&HSTRING::from(path), 0, 0, Some(&mut large), None, 256).is_ok();
        if !ok || large.is_invalid() {
            return extract_icon(path);
        }
        hicon_png(large, max)
    }
}

/// Convertit (et détruit) une HICON en PNG base64 ; `max` > 0 réduit l'image.
unsafe fn hicon_png(icon: windows::Win32::UI::WindowsAndMessaging::HICON, max: u32) -> Option<String> {
    use windows::Win32::Graphics::Gdi::*;
    use windows::Win32::UI::WindowsAndMessaging::{DestroyIcon, GetIconInfo, ICONINFO};
    let mut info = ICONINFO::default();
    let res = (|| {
        GetIconInfo(icon, &mut info).ok()?;
        let mut bm = BITMAP::default();
        GetObjectW(info.hbmColor.into(), std::mem::size_of::<BITMAP>() as i32, Some(&mut bm as *mut _ as _));
        let (w, h) = (bm.bmWidth, bm.bmHeight);
        if w <= 0 || h <= 0 {
            return None;
        }
        let mut bi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let dc = GetDC(None);
        let mut buf = vec![0u8; (w * h * 4) as usize];
        let lines = GetDIBits(dc, info.hbmColor, 0, h as u32, Some(buf.as_mut_ptr() as _), &mut bi, DIB_RGB_COLORS);
        ReleaseDC(None, dc);
        if lines == 0 {
            return None;
        }
        let has_alpha = buf.chunks_exact(4).any(|p| p[3] != 0);
        for p in buf.chunks_exact_mut(4) {
            p.swap(0, 2);
            if !has_alpha {
                p[3] = 255;
            }
        }
        let mut img = image::RgbaImage::from_raw(w as u32, h as u32, buf)?;
        if max > 0 && (w as u32 > max || h as u32 > max) {
            img = image::imageops::resize(&img, max, max, image::imageops::FilterType::Triangle);
        }
        let mut png = std::io::Cursor::new(Vec::new());
        img.write_to(&mut png, image::ImageFormat::Png).ok()?;
        Some(format!("data:image/png;base64,{}", B64.encode(png.into_inner())))
    })();
    if !info.hbmColor.is_invalid() {
        let _ = DeleteObject(info.hbmColor.into());
    }
    if !info.hbmMask.is_invalid() {
        let _ = DeleteObject(info.hbmMask.into());
    }
    let _ = DestroyIcon(icon);
    res
}

#[cfg(test)]
mod tests {
    #[test]
    fn snapshot_lists_outputs() {
        let m = super::snapshot().expect("snapshot");
        for d in &m.devices {
            println!("DEV {} | vol {:.2} muted {} default {}", d.name, d.volume, d.muted, d.default);
        }
        for a in &m.apps {
            println!("APP {} | vol {:.2} muted {} active {} icon {}", a.name, a.volume, a.muted, a.active, a.icon.is_some());
        }
        assert!(!m.devices.is_empty());
    }
}

/// Touche volume : ±2 % (comme Windows) ou sourdine sur la sortie par défaut. Renvoie (volume 0..1, coupé).
pub fn key_step(vk: u32) -> Result<(f32, bool), String> {
    let e = enumerator().map_err(|x| x.to_string())?;
    unsafe {
        let dev = e.GetDefaultAudioEndpoint(eRender, eMultimedia).map_err(|x| x.to_string())?;
        let ev = dev.Activate::<IAudioEndpointVolume>(CLSCTX_ALL, None).map_err(|x| x.to_string())?;
        let mut v = ev.GetMasterVolumeLevelScalar().unwrap_or(0.0);
        let mut m = ev.GetMute().map(|b| b.as_bool()).unwrap_or(false);
        match vk {
            0xAD => {
                m = !m;
                ev.SetMute(m, &GUID::zeroed()).map_err(|x| x.to_string())?;
            }
            0xAF | 0xAE => {
                let step = if vk == 0xAF { 1.0 } else { -1.0 };
                v = (((v * 50.0).round() + step) / 50.0).clamp(0.0, 1.0);
                ev.SetMasterVolumeLevelScalar(v, &GUID::zeroed()).map_err(|x| x.to_string())?;
                if m && vk == 0xAF {
                    m = false;
                    let _ = ev.SetMute(false, &GUID::zeroed());
                }
            }
            _ => {}
        }
        Ok((v, m))
    }
}

/// Molette sur la musique : change le volume de l'appli (par nom d'exe), sinon le volume Windows.
/// Renvoie (nouveau volume 0..1, true si c'est le volume de l'appli).
pub fn nudge_media(names: &[String], delta: f32) -> Result<(f32, bool), String> {
    let e = enumerator().map_err(|x| x.to_string())?;
    let mut out: Option<f32> = None;
    let _ = for_each_session(&e, |ctl2, vol, _| unsafe {
        let pid = ctl2.GetProcessId().unwrap_or(0);
        let Some(path) = exe_path(pid) else { return };
        let exe = path.rsplit('\\').next().unwrap_or("").to_lowercase();
        let exe = exe.trim_end_matches(".exe");
        if !names.iter().any(|n| exe == n || (n.len() >= 4 && exe.contains(n.as_str()))) {
            return;
        }
        let cur = vol.GetMasterVolume().unwrap_or(1.0);
        let nv = (cur + delta).clamp(0.0, 1.0);
        let _ = vol.SetMasterVolume(nv, &GUID::zeroed());
        if nv > 0.0 {
            let _ = vol.SetMute(false, &GUID::zeroed());
        }
        out = Some(nv);
    });
    if let Some(v) = out {
        return Ok((v, true));
    }
    unsafe {
        let dev = e.GetDefaultAudioEndpoint(eRender, eMultimedia).map_err(|x| x.to_string())?;
        let ev = dev.Activate::<IAudioEndpointVolume>(CLSCTX_ALL, None).map_err(|x| x.to_string())?;
        let v = (ev.GetMasterVolumeLevelScalar().unwrap_or(0.0) + delta).clamp(0.0, 1.0);
        ev.SetMasterVolumeLevelScalar(v, &GUID::zeroed()).map_err(|x| x.to_string())?;
        if v > 0.0 {
            let _ = ev.SetMute(false, &GUID::zeroed());
        }
        Ok((v, false))
    }
}
