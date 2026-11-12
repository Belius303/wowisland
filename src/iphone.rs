//! Batterie de l'iPhone branché en USB, via le service « Apple Mobile Device » (usbmuxd, port 27015)
//! installé avec l'appli Appareils Apple / iTunes. L'iPhone doit avoir fait « Faire confiance ».
//! Détecte aussi les cas d'échec (service arrêté, pas de confiance) pour les expliquer dans l'island.

use crate::log;
use idevice::lockdown::LockdownClient;
use idevice::provider::IdeviceProvider;
use idevice::usbmuxd::{Connection, UsbmuxdAddr, UsbmuxdConnection};
use idevice::IdeviceService;
use serde::Serialize;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter};

pub const SERVICE: &str = "Apple Mobile Device Service";

#[derive(Serialize, Clone, PartialEq, Default, Debug)]
pub struct PhoneState {
    pub connected: bool,
    pub name: String,
    pub battery: Option<u8>,
    pub charging: bool,
    pub full: bool,
    /// message si l'iPhone est branché mais illisible (service arrêté, pas de confiance…)
    pub error: Option<String>,
    /// "service" quand le service Apple est arrêté (l'UI propose de réparer)
    pub error_kind: Option<String>,
}

#[derive(Serialize, Clone, Default)]
pub struct Diag {
    /// "running" | "stopped" | "missing"
    pub service: String,
    pub usb_present: bool,
    pub apple_devices_installed: bool,
    pub phone_link_installed: bool,
    /// le processus de Lien avec le téléphone tourne (sinon aucune notif de l'iPhone n'arrive)
    pub phone_link_running: bool,
    /// Lien avec le téléphone a déjà affiché au moins une notification sur ce PC
    pub phone_link_ever_notified: bool,
    pub last: PhoneState,
}

static LAST: once_cell::sync::Lazy<parking_lot::Mutex<PhoneState>> = once_cell::sync::Lazy::new(Default::default);

pub fn start(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        let mut last = PhoneState::default();
        let mut last_query = Instant::now() - Duration::from_secs(600);
        let mut current_udid: Option<String> = None;
        let mut last_launch: Option<Instant> = None;
        loop {
            let state = match usb_device().await {
                Ok(Some(dev)) => {
                    let fresh = current_udid.as_deref() != Some(dev.udid.as_str());
                    // batterie toutes les 15 s (ou tout de suite si nouveau branchement)
                    if fresh || last_query.elapsed() > Duration::from_secs(15) || last.error.is_some() && last_query.elapsed() > Duration::from_secs(5) {
                        current_udid = Some(dev.udid.clone());
                        last_query = Instant::now();
                        match query(&dev).await {
                            Ok(s) => s,
                            Err(e) => {
                                log(&format!("iphone: {e}"));
                                PhoneState { connected: true, name: "iPhone".into(), error: Some(e), ..Default::default() }
                            }
                        }
                    } else {
                        last.clone()
                    }
                }
                Ok(None) => {
                    current_udid = None;
                    PhoneState::default()
                }
                // usbmuxd injoignable : si un iPhone est physiquement branché, on explique pourquoi.
                Err(_) => {
                    current_udid = None;
                    if apple_usb_present() {
                        let store_app = package_installed(APPLE_DEVICES);
                        // Appareils Apple installé mais son lanceur ne tourne pas : on le démarre (au plus 1 fois / min).
                        if store_app && last_launch.map(|t: Instant| t.elapsed() > Duration::from_secs(60)).unwrap_or(true) {
                            last_launch = Some(Instant::now());
                            match start_apple_launcher() {
                                Ok(()) => log("iphone: lanceur Appareils Apple démarré"),
                                Err(e) => log(&format!("iphone: lanceur Appareils Apple: {e}")),
                            }
                        }
                        let msg = match service_state().as_str() {
                            "missing" => "Installe « Appareils Apple » (Microsoft Store) pour lire la batterie",
                            _ if store_app => "Ouvre « Appareils Apple » une fois pour lancer la connexion USB",
                            _ => "Service Apple Mobile Device arrêté — ouvre les réglages pour le réparer",
                        };
                        PhoneState { connected: true, name: "iPhone".into(), error: Some(msg.into()), error_kind: Some("service".into()), ..Default::default() }
                    } else {
                        PhoneState::default()
                    }
                }
            };
            if state != last {
                let _ = app.emit("iphone", &state);
                *LAST.lock() = state.clone();
                last = state;
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    });
}

async fn usb_device() -> Result<Option<idevice::usbmuxd::UsbmuxdDevice>, String> {
    let mut mux = UsbmuxdConnection::default().await.map_err(|e| e.to_string())?;
    let devices = mux.get_devices().await.map_err(|e| e.to_string())?;
    Ok(devices.into_iter().find(|d| d.connection_type == Connection::Usb))
}

async fn query(dev: &idevice::usbmuxd::UsbmuxdDevice) -> Result<PhoneState, String> {
    let provider = dev.to_provider(UsbmuxdAddr::default(), "dynamic-island");
    let mut lockdown = LockdownClient::connect(&provider).await.map_err(|e| e.to_string())?;
    let name = lockdown
        .get_value(Some("DeviceName"), None)
        .await
        .ok()
        .and_then(|v| v.as_string().map(String::from))
        .unwrap_or_else(|| "iPhone".into());
    let pairing = provider
        .get_pairing_file()
        .await
        .map_err(|_| "Déverrouille l'iPhone et appuie sur « Faire confiance »".to_string())?;
    lockdown
        .start_session(&pairing)
        .await
        .map_err(|e| format!("Déverrouille l'iPhone (session refusée : {e})"))?;
    let v = lockdown.get_value(None, Some("com.apple.mobile.battery")).await.map_err(|e| e.to_string())?;
    let d = v.as_dictionary().ok_or("réponse batterie invalide")?;
    let battery = d.get("BatteryCurrentCapacity").and_then(|x| x.as_unsigned_integer()).map(|x| x.min(100) as u8);
    let charging = d.get("BatteryIsCharging").and_then(|x| x.as_boolean()).unwrap_or(false);
    let full = d.get("FullyCharged").and_then(|x| x.as_boolean()).unwrap_or(false);
    Ok(PhoneState { connected: true, name, battery, charging: charging || full, full, error: None, error_kind: None })
}

/// Un appareil Apple (VID 05AC) est-il branché en USB ?
pub fn apple_usb_present() -> bool {
    use windows::core::w;
    use windows::Win32::Devices::DeviceAndDriverInstallation::*;
    unsafe {
        let Ok(set) = SetupDiGetClassDevsW(None, w!("USB"), None, DIGCF_PRESENT | DIGCF_ALLCLASSES) else { return false };
        let mut found = false;
        let mut i = 0;
        loop {
            let mut data = SP_DEVINFO_DATA { cbSize: std::mem::size_of::<SP_DEVINFO_DATA>() as u32, ..Default::default() };
            if SetupDiEnumDeviceInfo(set, i, &mut data).is_err() {
                break;
            }
            let mut buf = [0u16; 512];
            if SetupDiGetDeviceInstanceIdW(set, &data, Some(&mut buf), None).is_ok() {
                let id = String::from_utf16_lossy(&buf).to_uppercase();
                if id.contains("VID_05AC") {
                    found = true;
                    break;
                }
            }
            i += 1;
        }
        let _ = SetupDiDestroyDeviceInfoList(set);
        found
    }
}

/// Le démon usbmuxd d'Apple répond-il ? (service iTunes ou processus d'« Appareils Apple »)
fn usbmuxd_reachable() -> bool {
    std::net::TcpStream::connect_timeout(&([127, 0, 0, 1], 27015).into(), Duration::from_millis(300)).is_ok()
}

/// "running" | "stopped" | "missing"
pub fn service_state() -> String {
    use windows::core::HSTRING;
    use windows::Win32::System::Services::*;
    // La version Microsoft Store d'« Appareils Apple » n'installe pas de service Windows :
    // son processus AppleMobileDeviceProcess écoute directement sur le port 27015.
    if usbmuxd_reachable() {
        return "running".into();
    }
    if package_installed(APPLE_DEVICES) {
        return "stopped".into();
    }
    unsafe {
        let Ok(scm) = OpenSCManagerW(None, None, SC_MANAGER_CONNECT) else { return "missing".into() };
        let r = match OpenServiceW(scm, &HSTRING::from(SERVICE), SERVICE_QUERY_STATUS) {
            Ok(svc) => {
                let mut st = SERVICE_STATUS::default();
                let s = if QueryServiceStatus(svc, &mut st).is_ok() && st.dwCurrentState == SERVICE_RUNNING { "running" } else { "stopped" };
                let _ = CloseServiceHandle(svc);
                s
            }
            Err(_) => "missing",
        };
        let _ = CloseServiceHandle(scm);
        r.into()
    }
}

const PHONE_LINK: &str = "Microsoft.YourPhone_8wekyb3d8bbwe";

/// Un processus de ce nom tourne-t-il ?
fn process_running(exe: &str) -> bool {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Diagnostics::ToolHelp::*;
    unsafe {
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else { return false };
        let mut e = PROCESSENTRY32W { dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        let mut found = false;
        let mut ok = Process32FirstW(snap, &mut e).is_ok();
        while ok {
            let len = e.szExeFile.iter().position(|&c| c == 0).unwrap_or(e.szExeFile.len());
            if String::from_utf16_lossy(&e.szExeFile[..len]).eq_ignore_ascii_case(exe) {
                found = true;
                break;
            }
            ok = Process32NextW(snap, &mut e).is_ok();
        }
        let _ = CloseHandle(snap);
        found
    }
}

/// Windows garde une clé par appli qui a déjà affiché une notification.
fn phone_link_ever_notified() -> bool {
    use windows::core::HSTRING;
    use windows::Win32::System::Registry::*;
    let path = HSTRING::from(format!(r"Software\Microsoft\Windows\CurrentVersion\Notifications\Settings\{PHONE_LINK}!App"));
    let mut key = HKEY::default();
    unsafe {
        let ok = RegOpenKeyExW(HKEY_CURRENT_USER, &path, None, KEY_READ, &mut key).is_ok();
        if ok {
            let _ = RegCloseKey(key);
        }
        ok
    }
}

/// Ouvre Lien avec le téléphone (il reste ensuite en arrière-plan pour relayer les notifs).
pub fn open_phone_link() -> Result<(), String> {
    crate::shell_open(&format!(r"shell:AppsFolder\{PHONE_LINK}!App"))
}

const APPLE_DEVICES: &str = "AppleInc.AppleDevices_nzyj5cx40ttqa";

/// Démarre le lanceur en arrière-plan d'« Appareils Apple » (sa tâche de démarrage), sans ouvrir l'appli.
fn start_apple_launcher() -> Result<(), String> {
    use windows::core::HSTRING;
    use windows::Management::Deployment::PackageManager;
    let dir = PackageManager::new()
        .and_then(|pm| pm.FindPackagesByUserSecurityIdPackageFamilyName(&HSTRING::new(), &HSTRING::from(APPLE_DEVICES)))
        .and_then(|it| it.First())
        .and_then(|i| i.Current())
        .and_then(|p| p.InstalledPath())
        .map_err(|e| e.to_string())?
        .to_string();
    let exe = std::path::Path::new(&dir).join("AppleMobileDeviceLauncher.exe");
    if !exe.exists() {
        return Err(format!("{} introuvable", exe.display()));
    }
    std::process::Command::new(&exe).spawn().map(|_| ()).map_err(|e| e.to_string())
}

fn package_installed(family: &str) -> bool {
    use windows::core::HSTRING;
    use windows::Management::Deployment::PackageManager;
    PackageManager::new()
        .and_then(|pm| pm.FindPackagesByUserSecurityIdPackageFamilyName(&HSTRING::new(), &HSTRING::from(family)))
        .map(|it| it.First().map(|i| i.HasCurrent().unwrap_or(false)).unwrap_or(false))
        .unwrap_or(false)
}

pub fn diag() -> Diag {
    Diag {
        service: service_state(),
        usb_present: apple_usb_present(),
        apple_devices_installed: package_installed(APPLE_DEVICES),
        phone_link_installed: package_installed(PHONE_LINK),
        phone_link_running: process_running("PhoneExperienceHost.exe"),
        phone_link_ever_notified: phone_link_ever_notified(),
        last: LAST.lock().clone(),
    }
}

/// Passe le service en démarrage automatique et le démarre (invite UAC, validée par l'utilisateur).
pub fn repair_service() -> Result<(), String> {
    use windows::core::HSTRING;
    // Version Microsoft Store : pas de service Windows, on (re)lance son processus, puis l'appli si besoin.
    if package_installed(APPLE_DEVICES) {
        let r = start_apple_launcher();
        std::thread::sleep(Duration::from_secs(3));
        if !usbmuxd_reachable() {
            return crate::shell_open(&format!("shell:AppsFolder\\{APPLE_DEVICES}!App")).or(r);
        }
        return Ok(());
    }
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;
    let args = format!(
        "-NoProfile -WindowStyle Hidden -Command \"Set-Service -Name '{SERVICE}' -StartupType Automatic; Start-Service -Name '{SERVICE}'\""
    );
    let r = unsafe { ShellExecuteW(None, &HSTRING::from("runas"), &HSTRING::from("powershell.exe"), &HSTRING::from(args), None, SW_HIDE) };
    if r.0 as isize > 32 { Ok(()) } else { Err("Autorisation refusée".into()) }
}

#[cfg(test)]
mod tests {
    #[test]
    fn read_battery() {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(async {
            match super::usb_device().await {
                Ok(Some(d)) => println!("IPHONE device {:?} -> {:?}", d.connection_type, super::query(&d).await),
                Ok(None) => println!("IPHONE usbmuxd ok, no USB device"),
                Err(e) => println!("IPHONE usbmuxd err {e}"),
            }
        });
    }
}
