//! Captures d'écran : zone (outil Capture d'écran de Windows) ou écran complet (GDI),
//! enregistrées dans Images\Captures d'écran et copiées dans le presse-papiers.

use base64::{engine::general_purpose::STANDARD as B64, Engine};
use image::ImageEncoder;
use serde::Serialize;
use std::path::PathBuf;
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN};

#[derive(Serialize, Clone)]
pub struct ShotEvent {
    pub id: u64,
    pub path: Option<String>,
    pub thumb: String,
    pub width: u32,
    pub height: u32,
}

pub fn shots_dir() -> PathBuf {
    use windows::Win32::System::Com::CoTaskMemFree;
    use windows::Win32::UI::Shell::{FOLDERID_Screenshots, SHGetKnownFolderPath, KF_FLAG_DEFAULT};
    let p = unsafe {
        SHGetKnownFolderPath(&FOLDERID_Screenshots, KF_FLAG_DEFAULT, None).ok().map(|pw| {
            let s = pw.to_string().unwrap_or_default();
            CoTaskMemFree(Some(pw.0 as _));
            PathBuf::from(s)
        })
    };
    let dir = p.filter(|p| !p.as_os_str().is_empty()).unwrap_or_else(|| {
        PathBuf::from(std::env::var("USERPROFILE").unwrap_or_default()).join("Pictures").join("Screenshots")
    });
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// Capture plein écran (écran principal), en masquant l'island pendant la prise.
pub fn full(app: &AppHandle) -> Result<(), String> {
    let win = app.get_webview_window("island");
    if let Some(w) = &win {
        let _ = w.hide();
    }
    std::thread::sleep(Duration::from_millis(180));
    let res = capture_primary();
    if let Some(w) = &win {
        let _ = w.show();
    }
    let (w, h, rgba) = res?;
    let img = image::RgbaImage::from_raw(w, h, rgba).ok_or("image invalide")?;

    let mut png = std::io::Cursor::new(Vec::new());
    image::codecs::png::PngEncoder::new_with_quality(&mut png, image::codecs::png::CompressionType::Fast, image::codecs::png::FilterType::Sub)
        .write_image(img.as_raw(), w, h, image::ExtendedColorType::Rgba8)
        .map_err(|e| e.to_string())?;
    let png = png.into_inner();

    // D'abord le presse-papiers (c'est ce qu'on veut coller tout de suite), puis le fichier.
    let owner = win.as_ref().map(crate::overlay::hwnd_of);
    if let Err(e) = crate::alerts::copy_image(owner, w, h, img.as_raw(), Some(&png)) {
        crate::log(&format!("capture: copie impossible: {e}"));
    }
    let name = format!("Capture {}.png", chrono::Local::now().format("%Y-%m-%d %H%M%S"));
    let path = shots_dir().join(name);
    std::fs::write(&path, &png).map_err(|e| e.to_string())?;
    emit(app, &img, Some(path.to_string_lossy().into_owned()));
    Ok(())
}

/// Capture d'une zone avec l'outil natif ; on surveille le presse-papiers pour afficher la miniature.
pub fn region(app: &AppHandle) -> Result<(), String> {
    let before = clipboard_signature();
    let started = std::time::SystemTime::now();
    crate::shell_open("ms-screenclip:")?;
    let app = app.clone();
    std::thread::spawn(move || {
        for _ in 0..120 {
            std::thread::sleep(Duration::from_millis(500));
            let sig = clipboard_signature();
            if sig.is_some() && sig != before {
                let Ok(mut cb) = arboard::Clipboard::new() else { return };
                let Ok(data) = cb.get_image() else { return };
                let Some(img) = image::RgbaImage::from_raw(data.width as u32, data.height as u32, data.bytes.into_owned()) else { return };
                // L'outil Capture enregistre souvent lui-même le fichier : on le réutilise, sinon on l'écrit.
                std::thread::sleep(Duration::from_millis(700));
                let path = newest_since(started).or_else(|| {
                    let p = shots_dir().join(format!("Capture {}.png", chrono::Local::now().format("%Y-%m-%d %H%M%S")));
                    img.save(&p).ok().map(|_| p)
                });
                emit(&app, &img, path.map(|p| p.to_string_lossy().into_owned()));
                return;
            }
        }
    });
    Ok(())
}

fn newest_since(t: std::time::SystemTime) -> Option<PathBuf> {
    std::fs::read_dir(shots_dir())
        .ok()?
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let m = e.metadata().ok()?.modified().ok()?;
            let ext = e.path().extension()?.to_string_lossy().to_lowercase();
            (m >= t && (ext == "png" || ext == "jpg")).then(|| (m, e.path()))
        })
        .max_by_key(|(m, _)| *m)
        .map(|(_, p)| p)
}

/// Ouvre une capture dans Paint.
pub fn open_in_paint(path: &str) -> Result<(), String> {
    use windows::core::HSTRING;
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    let r = unsafe { ShellExecuteW(None, &HSTRING::from("open"), &HSTRING::from("mspaint.exe"), &HSTRING::from(format!("\"{path}\"")), None, SW_SHOWNORMAL) };
    if r.0 as isize > 32 { Ok(()) } else { crate::shell_open(path) }
}

fn clipboard_signature() -> Option<(usize, usize, u64)> {
    let mut cb = arboard::Clipboard::new().ok()?;
    let img = cb.get_image().ok()?;
    let sample: u64 = img.bytes.iter().step_by(997).take(4096).map(|b| *b as u64).sum();
    Some((img.width, img.height, sample))
}

/// Résultat de l'analyse locale d'une capture (envoyé juste après la miniature).
#[derive(Serialize, Clone)]
pub struct ShotInfo {
    pub id: u64,
    pub text: Option<String>,
    pub qr: Option<String>,
}

static SHOT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

fn emit(app: &AppHandle, img: &image::RgbaImage, path: Option<String>) {
    let id = SHOT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let thumb = image::imageops::thumbnail(img, 320, (320 * img.height() / img.width().max(1)).max(1));
    let mut png = std::io::Cursor::new(Vec::new());
    if thumb.write_to(&mut png, image::ImageFormat::Png).is_ok() {
        let _ = app.emit(
            "screenshot",
            ShotEvent { id, path, thumb: format!("data:image/png;base64,{}", B64.encode(png.into_inner())), width: img.width(), height: img.height() },
        );
    }
    // OCR + QR en arrière-plan : la miniature s'affiche tout de suite, les boutons arrivent ensuite
    let (app, img) = (app.clone(), img.clone());
    std::thread::spawn(move || {
        let t0 = std::time::Instant::now();
        let qr = crate::ocr::qr(&img);
        let text = crate::ocr::text(&img).unwrap_or_else(|e| {
            crate::log(&format!("capture: {e}"));
            None
        });
        crate::log(&format!(
            "capture analysée en {} ms : texte {} car., QR {}",
            t0.elapsed().as_millis(),
            text.as_ref().map(|t| t.chars().count()).unwrap_or(0),
            if qr.is_some() { "oui" } else { "non" }
        ));
        let _ = app.emit("shot_info", ShotInfo { id, text, qr });
    });
}

fn capture_primary() -> Result<(u32, u32, Vec<u8>), String> {
    unsafe {
        let w = GetSystemMetrics(SM_CXSCREEN);
        let h = GetSystemMetrics(SM_CYSCREEN);
        let screen = GetDC(None);
        let mem = CreateCompatibleDC(Some(screen));
        let bmp = CreateCompatibleBitmap(screen, w, h);
        let old = SelectObject(mem, bmp.into());
        let ok = BitBlt(mem, 0, 0, w, h, Some(screen), 0, 0, SRCCOPY | CAPTUREBLT);
        let mut bi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h, // top-down
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut buf = vec![0u8; (w * h * 4) as usize];
        let lines = GetDIBits(mem, bmp, 0, h as u32, Some(buf.as_mut_ptr() as _), &mut bi, DIB_RGB_COLORS);
        SelectObject(mem, old);
        let _ = DeleteObject(bmp.into());
        let _ = DeleteDC(mem);
        ReleaseDC(None, screen);
        ok.map_err(|e| e.to_string())?;
        if lines == 0 {
            return Err("GetDIBits a échoué".into());
        }
        // BGRA → RGBA, alpha opaque
        for px in buf.chunks_exact_mut(4) {
            px.swap(0, 2);
            px[3] = 255;
        }
        Ok((w as u32, h as u32, buf))
    }
}

#[cfg(test)]
pub fn capture_primary_pub() -> Result<(u32, u32, Vec<u8>), String> {
    capture_primary()
}
