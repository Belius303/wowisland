//! Analyse d'une capture, 100 % locale : texte (OCR intégré à Windows, Windows.Media.Ocr)
//! et QR code (décodeur rqrr). Rien n'est envoyé en ligne.

use image::RgbaImage;
use windows::Graphics::Imaging::{BitmapPixelFormat, SoftwareBitmap};
use windows::Media::Ocr::OcrEngine;
use windows::Storage::Streams::DataWriter;

/// Texte reconnu (lignes séparées par \n), ou None s'il n'y en a pas.
pub fn text(img: &RgbaImage) -> Result<Option<String>, String> {
    let engine = OcrEngine::TryCreateFromUserProfileLanguages().map_err(|e| format!("OCR indisponible : {e}"))?;
    let max = OcrEngine::MaxImageDimension().unwrap_or(2600).max(200);
    let (w, h) = (img.width(), img.height());
    // trop grande → on réduit ; petite zone → on agrandit (l'OCR lit mal les petits caractères)
    let scale = if w.max(h) > max {
        max as f64 / w.max(h) as f64
    } else if w.max(h) < 700 {
        (2.0f64).min(max as f64 / w.max(h).max(1) as f64)
    } else {
        1.0
    };
    let resized;
    let src = if (scale - 1.0).abs() > 0.01 {
        let nw = ((w as f64 * scale).round() as u32).max(1);
        let nh = ((h as f64 * scale).round() as u32).max(1);
        resized = image::imageops::resize(img, nw, nh, image::imageops::FilterType::Triangle);
        &resized
    } else {
        img
    };
    let mut bgra = src.as_raw().clone();
    for px in bgra.chunks_exact_mut(4) {
        px.swap(0, 2);
    }
    let run = || -> windows::core::Result<String> {
        let dw = DataWriter::new()?;
        dw.WriteBytes(&bgra)?;
        let buf = dw.DetachBuffer()?;
        let bmp = SoftwareBitmap::CreateCopyFromBuffer(&buf, BitmapPixelFormat::Bgra8, src.width() as i32, src.height() as i32)?;
        let res = engine.RecognizeAsync(&bmp)?.join()?;
        let mut out = vec![];
        for line in res.Lines()? {
            let t = line.Text()?.to_string_lossy();
            if !t.trim().is_empty() {
                out.push(t.trim().to_string());
            }
        }
        Ok(out.join("\n"))
    };
    let s = run().map_err(|e| e.to_string())?;
    Ok((!s.trim().is_empty()).then_some(s))
}

/// Contenu du premier QR code trouvé dans l'image.
pub fn qr(img: &RgbaImage) -> Option<String> {
    let try_decode = |im: &RgbaImage| -> Option<String> {
        let (w, h) = (im.width() as usize, im.height() as usize);
        let raw = im.as_raw();
        let mut prepared = rqrr::PreparedImage::prepare_from_greyscale(w, h, |x, y| {
            let i = (y * w + x) * 4;
            ((raw[i] as u32 * 77 + raw[i + 1] as u32 * 150 + raw[i + 2] as u32 * 29) >> 8) as u8
        });
        prepared.detect_grids().into_iter().find_map(|g| g.decode().ok().map(|(_, s)| s)).filter(|s| !s.is_empty())
    };
    if let Some(s) = try_decode(img) {
        return Some(s);
    }
    // QR très gros sur un écran 4K : une version réduite aide parfois le détecteur
    if img.width().max(img.height()) > 1600 {
        let small = image::imageops::resize(img, img.width() / 2, img.height() / 2, image::imageops::FilterType::Triangle);
        return try_decode(&small);
    }
    None
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore]
    fn ocr_runs() {
        // image blanche avec un rectangle noir : l'OCR doit tourner sans erreur (texte vide)
        let mut img = image::RgbaImage::from_pixel(400, 120, image::Rgba([255, 255, 255, 255]));
        for x in 50..350 {
            for y in 50..60 {
                img.put_pixel(x, y, image::Rgba([0, 0, 0, 255]));
            }
        }
        println!("ocr = {:?}", super::text(&img));
        println!("qr = {:?}", super::qr(&img));
    }
}

#[cfg(test)]
mod screen_test {
    /// Capture l'écran principal et cherche une phrase témoin + un QR (n'affiche jamais le texte lu).
    #[test]
    #[ignore]
    fn screen_probe() {
        let img = match std::env::var("OCR_IMG") {
            Ok(p) => image::open(p).unwrap().to_rgba8(),
            Err(_) => {
                let (w, h, rgba) = crate::screenshot::capture_primary_pub().unwrap();
                image::RgbaImage::from_raw(w, h, rgba).unwrap()
            }
        };
        let t0 = std::time::Instant::now();
        let text = super::text(&img).unwrap().unwrap_or_default();
        let t1 = t0.elapsed().as_millis();
        let qr = super::qr(&img);
        println!("ocr {} ms, {} car., témoin trouvé : {}", t1, text.chars().count(), text.contains("Bonjour depuis l'île"));
        if std::env::var("OCR_IMG").is_ok() {
            println!("texte (image de test) : {text:?}");
        };
        println!("qr ({} ms) = {:?}", t0.elapsed().as_millis() - t1, qr);
    }
}
