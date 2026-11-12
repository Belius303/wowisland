//! Paroles synchronisées via LRCLIB (base publique et gratuite, sans compte : lrclib.net).
//! Seuls le titre, l'artiste, l'album et la durée du morceau sont envoyés. Résultats gardés en mémoire.

use once_cell::sync::Lazy;
use parking_lot::Mutex;
use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;
use std::time::Duration;

#[derive(Serialize, Clone)]
pub struct Line {
    /// secondes
    pub t: f64,
    pub text: String,
}

static CACHE: Lazy<Mutex<HashMap<String, Option<Vec<Line>>>>> = Lazy::new(Default::default);

pub fn get(artist: &str, title: &str, album: &str, duration: f64) -> Option<Vec<Line>> {
    let key = format!("{}|{}", artist.to_lowercase(), title.to_lowercase());
    if let Some(v) = CACHE.lock().get(&key) {
        return v.clone();
    }
    let r = fetch(artist, title, album, duration);
    let mut c = CACHE.lock();
    if c.len() > 300 {
        c.clear();
    }
    c.insert(key, r.clone());
    r
}

fn fetch(artist: &str, title: &str, album: &str, duration: f64) -> Option<Vec<Line>> {
    let first_artist = artist.split([',', '&']).next().unwrap_or(artist).split(" feat").next().unwrap_or(artist).trim().to_string();
    let clean = clean_title(title);
    // 1) correspondance exacte (titre, artiste, album, durée)
    if duration > 0.0 {
        if let Some(v) = call("get", &[("track_name", title), ("artist_name", artist), ("album_name", album), ("duration", &format!("{}", duration.round()))]) {
            if let Some(l) = synced(&v) {
                return Some(l);
            }
            if v.get("instrumental").and_then(|x| x.as_bool()) == Some(true) {
                return None;
            }
        }
    }
    // 2) recherche (titre nettoyé + 1er artiste), on garde un résultat de durée proche
    for (t, a) in [(clean.as_str(), first_artist.as_str()), (title, artist)] {
        if let Some(Value::Array(list)) = call("search", &[("track_name", t), ("artist_name", a)]) {
            let best = list.iter().filter(|x| synced(x).is_some()).find(|x| {
                let d = x.get("duration").and_then(|d| d.as_f64()).unwrap_or(0.0);
                duration <= 0.0 || d <= 0.0 || (d - duration).abs() <= 4.0
            });
            if let Some(b) = best {
                return synced(b);
            }
        }
    }
    None
}

fn call(path: &str, q: &[(&str, &str)]) -> Option<Value> {
    let mut req = crate::http().get(&format!("https://lrclib.net/api/{path}")).timeout(Duration::from_secs(8));
    for (k, v) in q {
        if !v.is_empty() {
            req = req.query(k, v);
        }
    }
    req.call().ok()?.into_json().ok()
}

/// « Señorita - Remastered 2011 (feat. X) [Live] » → « Señorita »
fn clean_title(t: &str) -> String {
    let mut s = t.to_string();
    for sep in [" - ", " (", " [", " feat.", " ft."] {
        if let Some(i) = s.find(sep) {
            if i > 0 {
                s.truncate(i);
            }
        }
    }
    s.trim().to_string()
}

fn synced(v: &Value) -> Option<Vec<Line>> {
    let raw = v.get("syncedLyrics")?.as_str()?;
    let mut out = vec![];
    for line in raw.lines() {
        let mut rest = line.trim();
        let mut times = vec![];
        while let Some(stripped) = rest.strip_prefix('[') {
            let Some(end) = stripped.find(']') else { break };
            if let Some(t) = parse_time(&stripped[..end]) {
                times.push(t);
            }
            rest = &stripped[end + 1..];
        }
        let text = rest.trim().to_string();
        for t in times {
            out.push(Line { t, text: text.clone() });
        }
    }
    out.sort_by(|a, b| a.t.partial_cmp(&b.t).unwrap_or(std::cmp::Ordering::Equal));
    if out.iter().filter(|l| !l.text.is_empty()).count() < 3 {
        return None;
    }
    Some(out)
}

fn parse_time(s: &str) -> Option<f64> {
    let (m, rest) = s.split_once(':')?;
    let m: f64 = m.trim().parse().ok()?;
    let sec: f64 = rest.trim().replace(',', ".").parse().ok()?;
    Some(m * 60.0 + sec)
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore]
    fn fetch_senorita() {
        let l = super::get("Shawn Mendes, Camila Cabello", "Señorita", "Señorita", 191.0);
        println!("{} lignes", l.map(|x| x.len()).unwrap_or(0));
    }
}
