//! Point central quand un code / lien de vérification est trouvé (Gmail ou notification).

use crate::{config, otp::Found};
use serde::Serialize;
use tauri::{AppHandle, Emitter};

#[derive(Serialize, Clone, Copy)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    Gmail,
    Phone,
    Pc,
}

#[derive(Serialize, Clone)]
pub struct SecretEvent {
    pub source: Source,
    pub from: String,
    pub subject: String,
    pub code: Option<String>,
    pub link: Option<String>,
    pub copied: bool,
}

pub fn found_secret(app: &AppHandle, source: Source, from: &str, subject: &str, found: Found) {
    let cfg = config::get();
    let (code, link) = match found {
        Found::Code(c) => (Some(c), None),
        Found::Link(l) => (None, Some(l)),
    };
    let mut copied = false;
    if let (Some(c), true) = (&code, cfg.auto_copy_otp) {
        copied = copy_text(c).is_ok();
        if copied && cfg.clear_otp_clipboard {
            let c = c.clone();
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_secs(90));
                if let Ok(mut cb) = arboard::Clipboard::new() {
                    if cb.get_text().map(|t| t == c).unwrap_or(false) {
                        let _ = cb.clear();
                    }
                }
            });
        }
    }
    let _ = app.emit(
        "secret",
        SecretEvent { source, from: from.into(), subject: subject.into(), code, link, copied },
    );
}

pub fn copy_text(text: &str) -> Result<(), String> {
    // codes / liens copiés par l'île : pas dans l'historique du presse-papiers
    crate::clipboard::ignore_text(text);
    // Le presse-papiers peut être verrouillé brièvement par une autre appli : on réessaie.
    for _ in 0..5 {
        if let Ok(mut cb) = arboard::Clipboard::new() {
            if cb.set_text(text.to_string()).is_ok() {
                return Ok(());
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(60));
    }
    Err("Presse-papiers indisponible".into())
}

/// Copie une image RGBA dans le presse-papiers (CF_DIB + format "PNG"), compatible Paint, Discord,
/// navigateurs, Office… `png` = l'image déjà encodée (optionnel mais recommandé).
pub fn copy_image(owner: Option<windows::Win32::Foundation::HWND>, w: u32, h: u32, rgba: &[u8], png: Option<&[u8]>) -> Result<(), String> {
    use windows::core::w;
    use windows::Win32::Foundation::{HANDLE, HGLOBAL};
    use windows::Win32::Graphics::Gdi::{BITMAPINFOHEADER, BI_RGB};
    use windows::Win32::System::DataExchange::*;
    use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};

    // DIB 32 bits, lignes de bas en haut, BGRA
    let header = BITMAPINFOHEADER {
        biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
        biWidth: w as i32,
        biHeight: h as i32,
        biPlanes: 1,
        biBitCount: 32,
        biCompression: BI_RGB.0,
        biSizeImage: w * h * 4,
        ..Default::default()
    };
    let mut dib = Vec::with_capacity(40 + (w * h * 4) as usize);
    dib.extend_from_slice(unsafe { std::slice::from_raw_parts(&header as *const _ as *const u8, 40) });
    for y in (0..h as usize).rev() {
        let row = &rgba[y * w as usize * 4..(y + 1) * w as usize * 4];
        for px in row.chunks_exact(4) {
            dib.extend_from_slice(&[px[2], px[1], px[0], 255]);
        }
    }

    unsafe fn to_global(bytes: &[u8]) -> Result<HGLOBAL, String> {
        let g = GlobalAlloc(GMEM_MOVEABLE, bytes.len()).map_err(|e| e.to_string())?;
        let p = GlobalLock(g) as *mut u8;
        if p.is_null() {
            return Err("GlobalLock".into());
        }
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), p, bytes.len());
        let _ = GlobalUnlock(g);
        Ok(g)
    }

    unsafe {
        let mut opened = false;
        for _ in 0..10 {
            if OpenClipboard(owner).is_ok() {
                opened = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        if !opened {
            return Err("Presse-papiers occupé".into());
        }
        let res = (|| -> Result<(), String> {
            EmptyClipboard().map_err(|e| e.to_string())?;
            let g = to_global(&dib)?;
            SetClipboardData(8 /* CF_DIB */, Some(HANDLE(g.0))).map_err(|e| e.to_string())?;
            if let Some(png) = png {
                let fmt = RegisterClipboardFormatW(w!("PNG"));
                if fmt != 0 {
                    let g = to_global(png)?;
                    let _ = SetClipboardData(fmt, Some(HANDLE(g.0)));
                }
            }
            Ok(())
        })();
        let _ = CloseClipboard();
        res
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn copy_image_to_clipboard() {
        let (w, h) = (64u32, 48u32);
        let rgba: Vec<u8> = (0..w * h).flat_map(|i| [(i % 255) as u8, 120, 200, 255]).collect();
        super::copy_image(None, w, h, &rgba, None).expect("copy");
    }
}
