//! Notifications Windows (Discord, WhatsApp, Teams… et l'iPhone via « Lien avec le téléphone »)
//! via UserNotificationListener. Les applis non empaquetées ne reçoivent pas l'événement
//! NotificationChanged, on interroge donc la liste toutes les 700 ms.

use crate::{alerts, config, otp};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::time::Duration;
use tauri::{AppHandle, Emitter};
use windows::Foundation::Size;
use windows::UI::Notifications::Management::{UserNotificationListener, UserNotificationListenerAccessStatus as Access};
use windows::UI::Notifications::{KnownNotificationBindings, NotificationKinds, UserNotification};

#[derive(Serialize, Clone)]
pub struct NotifEvent {
    pub id: u32,
    pub app: String,
    pub title: String,
    pub body: String,
    pub icon: Option<String>,
    /// true si la notif vient de l'iPhone (Lien avec le téléphone)
    pub phone: bool,
}

pub fn start(app: AppHandle) {
    std::thread::Builder::new().name("notifications".into()).spawn(move || run(app)).ok();
    // logos des applis iPhone courantes, téléchargés une fois pour toutes (petit cache sur le disque)
    std::thread::Builder::new()
        .name("phone-icons".into())
        .spawn(|| {
            for (name, _) in KNOWN_BUNDLES {
                let _ = store_icon_bytes(name);
            }
        })
        .ok();
}

fn run(app: AppHandle) {
    let listener = match UserNotificationListener::Current() {
        Ok(l) => l,
        Err(e) => {
            let _ = app.emit("notif-access", format!("Indisponible : {e}"));
            return;
        }
    };
    let mut status = listener.GetAccessStatus().unwrap_or(Access::Unspecified);
    if status != Access::Allowed {
        status = listener.RequestAccessAsync().and_then(|o| o.join()).unwrap_or(Access::Denied);
    }
    if status != Access::Allowed {
        let _ = app.emit("notif-access", "denied");
        // On réessaie régulièrement : l'utilisateur peut autoriser l'accès dans les paramètres.
        loop {
            std::thread::sleep(Duration::from_secs(10));
            if listener.GetAccessStatus().map(|s| s == Access::Allowed).unwrap_or(false) {
                break;
            }
        }
    }
    let _ = app.emit("notif-access", "allowed");

    let mut seen: HashSet<u32> = HashSet::new();
    let mut first = true;
    let mut icons: HashMap<String, Option<String>> = HashMap::new();
    let mut listener = listener;
    let mut fails = 0u32;
    loop {
        let res = listener.GetNotificationsAsync(NotificationKinds::Toast).and_then(|o| o.join());
        if res.is_err() {
            // service de notifications de Windows redémarré : on se reconnecte
            fails += 1;
            if fails % 5 == 0 {
                if let Ok(l) = UserNotificationListener::Current() {
                    listener = l;
                }
                if fails == 5 {
                    crate::log("notifications : lecture impossible, reconnexion");
                }
            }
        } else {
            if fails >= 5 {
                crate::log("notifications : reconnecté");
            }
            fails = 0;
        }
        if let Ok(list) = res {
            let n = list.Size().unwrap_or(0);
            let mut current = HashSet::new();
            for i in 0..n {
                let Ok(notif) = list.GetAt(i) else { continue };
                let Ok(id) = notif.Id() else { continue };
                current.insert(id);
                if seen.contains(&id) {
                    continue;
                }
                seen.insert(id);
                if first {
                    continue; // notifications déjà présentes au démarrage
                }
                if let Some(ev) = read(&notif, &mut icons) {
                    dispatch(&app, ev);
                }
            }
            seen.retain(|id| current.contains(id));
            first = false;
        }
        std::thread::sleep(Duration::from_millis(700));
    }
}

fn read(n: &UserNotification, icons: &mut HashMap<String, Option<String>>) -> Option<NotifEvent> {
    let id = n.Id().ok()?;
    let info = n.AppInfo().ok();
    let display = info.as_ref().and_then(|i| i.DisplayInfo().ok());
    let app = display.as_ref().and_then(|d| d.DisplayName().ok()).map(|s| s.to_string()).unwrap_or_else(|| "Notification".into());
    let aumid = info.as_ref().and_then(|i| i.AppUserModelId().ok()).map(|s| s.to_string()).unwrap_or_default();

    let binding = n.Notification().ok()?.Visual().ok()?.GetBinding(&KnownNotificationBindings::ToastGeneric().ok()?).ok()?;
    let elems = binding.GetTextElements().ok()?;
    // on garde les positions (Lien avec le téléphone : [appli iPhone, expéditeur, message], l'appli peut être vide)
    let raw: Vec<String> = (0..elems.Size().unwrap_or(0))
        .map(|i| elems.GetAt(i).and_then(|e| e.Text()).map(|t| t.to_string().trim().to_string()).unwrap_or_default())
        .collect();

    let phone = aumid.contains("YourPhone") || app.contains("Phone Link") || app.contains("Lien avec") || app.contains("Mobile connect");
    if phone && raw.len() >= 2 {
        let src_app = if raw[0].is_empty() { "iPhone".to_string() } else { raw[0].clone() };
        let rest: Vec<&String> = raw[1..].iter().filter(|t| !t.is_empty()).collect();
        if rest.is_empty() {
            return None;
        }
        let (title, body) = if rest.len() == 1 {
            (src_app.clone(), rest[0].clone())
        } else {
            (rest[0].clone(), rest[1..].iter().map(|s| s.as_str()).collect::<Vec<_>>().join("\n"))
        };
        let ck = format!("phone:{src_app}");
        let icon = match icons.get(&ck) {
            Some(Some(i)) => Some(i.clone()),
            _ => {
                // pas trouvée : on réessaiera à la prochaine notif (l'icône peut arriver juste après)
                let found = phone_app_icon(&src_app);
                if found.is_some() {
                    icons.insert(ck, found.clone());
                }
                found
            }
        };
        return Some(NotifEvent { id, app: src_app, title, body, icon, phone: true });
    }

    let mut texts: Vec<String> = raw.into_iter().filter(|t| !t.is_empty()).collect();
    if texts.is_empty() {
        return None;
    }
    let title = texts.remove(0);
    let body = texts.join("\n");

    let icon = icons
        .entry(app.clone())
        .or_insert_with(|| {
            display
                .as_ref()
                .and_then(|d| d.GetLogo(Size { Width: 96.0, Height: 96.0 }).ok())
                .and_then(|r| crate::media::read_thumbnail_sized(&windows::core::Interface::cast(&r).ok()?, 96).ok())
        })
        .clone();

    Some(NotifEvent { id, app, title, body, icon, phone: false })
}

/// Icône d'une appli de l'iPhone : Lien avec le téléphone garde les icônes des applis qui ont notifié
/// dans TempState\app_icons\app_icon_<bundle id><version>.jpg
fn phone_app_icon(app: &str) -> Option<String> {
    let dir = std::path::PathBuf::from(std::env::var("LOCALAPPDATA").ok()?).join(r"Packages\Microsoft.YourPhone_8wekyb3d8bbwe\TempState\app_icons");
    let files: Vec<(String, std::path::PathBuf, std::time::SystemTime)> = std::fs::read_dir(&dir)
        .ok()?
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_lowercase();
            let bundle = name.strip_prefix("app_icon_")?.to_string();
            Some((bundle, e.path(), e.metadata().and_then(|m| m.modified()).ok()?))
        })
        .collect();
    let key: String = app.to_lowercase().chars().filter(|c| c.is_alphanumeric()).collect();
    let known = KNOWN_BUNDLES.iter().find(|(n, _)| *n == key).map(|(_, b)| b.to_lowercase());
    let hit = files
        .iter()
        .find(|(b, _, _)| known.as_deref().map(|k| b.starts_with(k)).unwrap_or(false))
        .or_else(|| {
            // ex. "WhatsApp" → net.whatsapp.whatsapp, "Spotify" → com.spotify.client
            if key.len() < 3 {
                return None;
            }
            files.iter().find(|(b, _, _)| b.split('.').any(|part| part.trim_end_matches(|c: char| c.is_ascii_digit()) == key))
        })
        .or_else(|| {
            // icône tout juste écrite par Lien avec le téléphone pour cette notif
            let now = std::time::SystemTime::now();
            files.iter().filter(|(_, _, m)| now.duration_since(*m).map(|d| d.as_secs() < 8).unwrap_or(false)).max_by_key(|(_, _, m)| *m)
        });
    let bytes = match hit {
        Some(h) => std::fs::read(&h.1).ok()?,
        // pas (encore) dans le cache de Lien avec le téléphone : logo officiel de l'App Store
        None => store_icon_bytes(app)?,
    };
    crate::media::shrink(&bytes, 96)
}

fn app_key(app: &str) -> String {
    app.to_lowercase().chars().filter(|c| c.is_alphanumeric()).collect()
}

static STORE_MISS: once_cell::sync::Lazy<parking_lot::Mutex<HashSet<String>>> = once_cell::sync::Lazy::new(Default::default);

/// Icône iOS officielle d'une appli via l'API publique de l'App Store (seul le nom de l'appli est envoyé),
/// gardée dans %APPDATA%\DynamicIsland\phone_icons.
fn store_icon_bytes(app: &str) -> Option<Vec<u8>> {
    let key = app_key(app);
    if key.is_empty() || key == "iphone" {
        return None;
    }
    let dir = config::dir().join("phone_icons");
    let file = dir.join(format!("{key}.jpg"));
    if let Ok(b) = std::fs::read(&file) {
        return Some(b);
    }
    if STORE_MISS.lock().contains(&key) {
        return None;
    }
    let found = (|| {
        let agent = crate::http();
        let get = |url: &str| -> Option<serde_json::Value> {
            agent.get(url).timeout(Duration::from_secs(5)).call().ok()?.into_json::<serde_json::Value>().ok()
        };
        let results = |v: Option<serde_json::Value>| -> Vec<serde_json::Value> {
            v.and_then(|v| v.get("results").and_then(|r| r.as_array()).cloned()).unwrap_or_default()
        };
        let mut list = Vec::new();
        let known = KNOWN_BUNDLES.iter().find(|(n, _)| *n == key).map(|(_, b)| *b);
        if let Some(bundle) = known {
            list = results(get(&format!("https://itunes.apple.com/lookup?bundleId={bundle}&country=fr")));
        }
        // les applis Apple absentes de l'App Store (Messages, Téléphone…) : pas de recherche par nom
        if list.is_empty() && !known.map(|b| b.starts_with("com.apple.")).unwrap_or(false) {
            let term: String = app.chars().map(|c| if c.is_alphanumeric() { c } else { '+' }).collect();
            list = results(get(&format!("https://itunes.apple.com/search?term={term}&entity=software&country=fr&limit=6")));
            // on garde le résultat dont le nom correspond le mieux
            let best = list.iter().position(|r| {
                let n = app_key(r.get("trackName").and_then(|v| v.as_str()).unwrap_or(""));
                n == key || n.starts_with(&key)
            });
            match best {
                Some(i) => list = vec![list[i].clone()],
                None if key.len() >= 4 => list.truncate(1),
                None => list.clear(),
            }
        }
        let r = list.first()?;
        let art = r.get("artworkUrl512").or_else(|| r.get("artworkUrl100"))?.as_str()?.replace("512x512bb", "256x256bb");
        let resp = agent.get(&art).timeout(Duration::from_secs(8)).call().ok()?;
        let mut b = Vec::new();
        std::io::Read::read_to_end(&mut std::io::Read::take(resp.into_reader(), 2 << 20), &mut b).ok()?;
        (b.len() > 200).then_some(b)
    })();
    match found {
        Some(b) => {
            let _ = std::fs::create_dir_all(&dir);
            let _ = std::fs::write(&file, &b);
            Some(b)
        }
        None => {
            STORE_MISS.lock().insert(key);
            None
        }
    }
}

/// Nom affiché → identifiant d'appli iOS, pour les applis courantes.
const KNOWN_BUNDLES: &[(&str, &str)] = &[
    ("instagram", "com.burbn.instagram"),
    ("snapchat", "com.toyopagroup.picaboo"),
    ("whatsapp", "net.whatsapp.WhatsApp"),
    ("messages", "com.apple.MobileSMS"),
    ("imessage", "com.apple.MobileSMS"),
    ("telephone", "com.apple.mobilephone"),
    ("téléphone", "com.apple.mobilephone"),
    ("phone", "com.apple.mobilephone"),
    ("facetime", "com.apple.facetime"),
    ("mail", "com.apple.mobilemail"),
    ("discord", "com.hammerandchisel.discord"),
    ("messenger", "com.facebook.Messenger"),
    ("facebook", "com.facebook.Facebook"),
    ("telegram", "ph.telegra.Telegraph"),
    ("tiktok", "com.zhiliaoapp.musically"),
    ("x", "com.atebits.Tweetie2"),
    ("twitter", "com.atebits.Tweetie2"),
    ("gmail", "com.google.Gmail"),
    ("youtube", "com.google.ios.youtube"),
    ("signal", "org.whispersystems.signal"),
    ("twitch", "tv.twitch"),
    ("spotify", "com.spotify.client"),
    ("teams", "com.microsoft.skype.teams"),
    ("outlook", "com.microsoft.Office.Outlook"),
    ("reddit", "com.reddit.Reddit"),
    ("bereal", "AlexisBarreyat.BeReal"),
    ("threads", "com.burbn.barcelona"),
    ("linkedin", "com.linkedin.LinkedIn"),
    ("netflix", "com.netflix.Netflix"),
    ("ubereats", "com.ubercab.UberEats"),
    ("uber", "com.ubercab.UberClient"),
    ("revolut", "com.revolut.revolut"),
    ("vinted", "fr.vinted"),
    ("leboncoin", "fr.leboncoin"),
    ("chatgpt", "com.openai.chat"),
    ("pinterest", "pinterest"),
    ("tinder", "com.cardify.tinder"),
    ("amazon", "com.amazon.Amazon"),
    ("paypal", "com.yourcompany.PPClient"),
    ("googlemaps", "com.google.Maps"),
    ("maps", "com.google.Maps"),
    ("waze", "com.waze.iphone"),
    ("duolingo", "com.duolingo.DuolingoMobile"),
    ("chrome", "com.google.chrome.ios"),
    ("skype", "com.skype.skype"),
    ("airbnb", "com.airbnb.app"),
    ("shazam", "com.shazam.Shazam"),
    ("deezer", "com.deezer.Deezer"),
    ("primevideo", "com.amazon.aiv.AIVApp"),
    ("disney", "com.disney.disneyplus"),
    ("binance", "com.czzhao.binance"),
    ("lydia", "com.lydia-app"),
    ("photos", "com.apple.mobileslideshow"),
    ("notes", "com.apple.mobilenotes"),
    ("appstore", "com.apple.AppStore"),
    ("rappels", "com.apple.reminders"),
    ("calendrier", "com.apple.mobilecal"),
    ("horloge", "com.apple.mobiletimer"),
];

/// Retire des notifications du centre de notifications Windows (quand on les efface dans l'island).
pub fn remove(ids: &[u32]) {
    if let Ok(l) = UserNotificationListener::Current() {
        for id in ids {
            let _ = l.RemoveNotification(*id);
        }
    }
}

fn dispatch(app: &AppHandle, ev: NotifEvent) {
    let cfg = config::get();
    let app_l = ev.app.to_lowercase();
    if cfg.notif_blocklist.iter().any(|b| !b.trim().is_empty() && app_l.contains(&b.trim().to_lowercase())) {
        return;
    }
    // Codes reçus par SMS / notif (ex. iMessage via Lien avec le téléphone) → copiés aussi !
    if let Some(code) = otp::find_code(&ev.title, &ev.body) {
        let src = if ev.phone { alerts::Source::Phone } else { alerts::Source::Pc };
        let from = if ev.phone { ev.title.clone() } else { ev.app.clone() };
        alerts::found_secret(app, src, &from, &ev.body, otp::Found::Code(code));
        return;
    }
    if !cfg.show_pc_notifications && !ev.phone {
        return;
    }
    let _ = app.emit("notification", ev);
}

#[cfg(test)]
mod tests {
    use super::*;
    /// Diagnostic : quelles applis ont des notifs dans le centre de notifications (noms seulement, pas le contenu)
    #[test]
    fn list_sources() {
        let l = UserNotificationListener::Current().unwrap();
        println!("NOTIF access {:?}", l.GetAccessStatus().map(|s| s.0));
        let list = l.GetNotificationsAsync(NotificationKinds::Toast).unwrap().join().unwrap();
        let mut by: std::collections::BTreeMap<String, (usize, bool)> = Default::default();
        for i in 0..list.Size().unwrap() {
            let n = list.GetAt(i).unwrap();
            let info = n.AppInfo().ok();
            let name = info.as_ref().and_then(|i| i.DisplayInfo().ok()).and_then(|d| d.DisplayName().ok()).map(|s| s.to_string()).unwrap_or_default();
            let aumid = info.as_ref().and_then(|i| i.AppUserModelId().ok()).map(|s| s.to_string()).unwrap_or_default();
            let generic = n.Notification().ok().and_then(|x| x.Visual().ok()).and_then(|v| v.GetBinding(&KnownNotificationBindings::ToastGeneric().ok()?).ok()).is_some();
            let e = by.entry(format!("{name} [{aumid}]")).or_default();
            e.0 += 1;
            e.1 |= generic;
        }
        for (k, (c, g)) in by { println!("NOTIF {k}: {c} (generic={g})"); }
        // structure des notifs de Lien avec le téléphone : nb de textes, 1er texte (nom de l'appli), longueurs des autres
        for i in 0..list.Size().unwrap() {
            let n = list.GetAt(i).unwrap();
            let aumid = n.AppInfo().and_then(|i| i.AppUserModelId()).map(|s| s.to_string()).unwrap_or_default();
            if !aumid.contains("YourPhone") { continue; }
            let Some(b) = n.Notification().ok().and_then(|x| x.Visual().ok()).and_then(|v| v.GetBinding(&KnownNotificationBindings::ToastGeneric().ok()?).ok()) else { continue };
            let el = b.GetTextElements().unwrap();
            let texts: Vec<String> = (0..el.Size().unwrap()).filter_map(|j| el.GetAt(j).and_then(|e| e.Text()).ok().map(|s| s.to_string())).collect();
            let hints: Vec<String> = b.Hints().map(|h| h.into_iter().map(|kv| format!("{}={}", kv.Key().unwrap(), kv.Value().unwrap())).collect()).unwrap_or_default();
            println!("PL texts={} first={:?} lens={:?} colon_in_2nd={} hints={:?} template={:?}", texts.len(), texts.first(), texts.iter().map(|t| t.chars().count()).collect::<Vec<_>>(), texts.get(1).map(|t| t.contains(':')).unwrap_or(false), hints, b.Template().map(|s| s.to_string()));
        }
    }
}

#[cfg(test)]
mod store_tests {
    #[test]
    fn store_icons() {
        for app in ["Reddit", "WhatsApp", "X", "Discord", "Glovo", "Deliveroo", "Messages", "Uber Eats", "Vinted"] {
            let b = super::store_icon_bytes(app);
            println!("{app}: {:?}", b.map(|b| b.len()));
        }
    }
}
