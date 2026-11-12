//! Envoi de fichiers vers un tel avec LocalSend (https://localsend.org), protocole v2 ouvert.
//! L'île ne fait qu'envoyer : elle cherche elle-même les appareils LocalSend du réseau (connexion vers
//! le port 53317 de chaque adresse du Wi-Fi) au lieu d'écouter les annonces, donc aucune connexion
//! entrante et pas de règle de pare-feu. Le tel doit avoir LocalSend ouvert, sur le même réseau.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::io::Read;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream, ToSocketAddrs, UdpSocket};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

pub const PORT: u16 = 53317;

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(default)]
pub struct Device {
    pub alias: String,
    pub ip: String,
    pub port: u16,
    pub https: bool,
    pub fingerprint: String,
    pub model: String,
    /// "mobile" | "desktop" | "web" | "headless" | "server"
    pub kind: String,
}

#[derive(Serialize, Deserialize, Default)]
#[serde(default)]
struct Saved {
    /// identifiant de l'île auprès des tels (fixe) : empreinte SHA-256 de son certificat
    me: String,
    /// certificat auto-signé de l'île + sa clé (PEM). Les tels LocalSend exigent un certificat
    /// client en HTTPS : sans lui ils coupent la connexion.
    cert: String,
    key: String,
    /// dernier tel choisi
    dev: Option<Device>,
}

fn saved_path() -> PathBuf {
    crate::config::dir().join("localsend.json")
}

fn load() -> Saved {
    let mut s: Saved = std::fs::read_to_string(saved_path()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    if s.cert.is_empty() || s.key.is_empty() {
        match make_cert() {
            Ok((cert, key, fp)) => {
                s.cert = cert;
                s.key = key;
                s.me = fp;
            }
            Err(e) => crate::log(&format!("LocalSend : certificat impossible à créer ({e})")),
        }
        store(&s);
    }
    s
}

/// Certificat auto-signé comme celui de LocalSend (CN « LocalSend User »), valable 25 ans.
fn make_cert() -> Result<(String, String, String), String> {
    // clé RSA : Windows (SChannel) ne sait pas utiliser une clé ECDSA fournie en PKCS#8
    use rsa::pkcs8::EncodePrivateKey;
    let rsa_key = rsa::RsaPrivateKey::new(&mut rsa::rand_core::OsRng, 2048).map_err(|e| e.to_string())?;
    let pem = rsa_key.to_pkcs8_pem(rsa::pkcs8::LineEnding::LF).map_err(|e| e.to_string())?;
    let key = rcgen::KeyPair::from_pkcs8_pem_and_sign_algo(&pem, &rcgen::PKCS_RSA_SHA256).map_err(|e| e.to_string())?;
    let mut params = rcgen::CertificateParams::new(Vec::<String>::new()).map_err(|e| e.to_string())?;
    let mut dn = rcgen::DistinguishedName::new();
    dn.push(rcgen::DnType::CommonName, "LocalSend User");
    params.distinguished_name = dn;
    params.not_before = rcgen::date_time_ymd(2024, 1, 1);
    params.not_after = rcgen::date_time_ymd(2050, 1, 1);
    let cert = params.self_signed(&key).map_err(|e| e.to_string())?;
    let fp = ring::digest::digest(&ring::digest::SHA256, cert.der()).as_ref().iter().map(|b| format!("{b:02X}")).collect();
    Ok((cert.pem(), key.serialize_pem(), fp))
}

static TLS: once_cell::sync::Lazy<Option<Arc<native_tls::TlsConnector>>> = once_cell::sync::Lazy::new(|| {
    let s = load();
    let mut b = native_tls::TlsConnector::builder();
    b.danger_accept_invalid_certs(true).danger_accept_invalid_hostnames(true);
    match native_tls::Identity::from_pkcs8(s.cert.as_bytes(), s.key.as_bytes()) {
        Ok(id) => {
            b.identity(id);
        }
        Err(e) => crate::log(&format!("LocalSend : certificat illisible ({e})")),
    }
    b.build().ok().map(Arc::new)
});

fn store(s: &Saved) {
    if let Ok(t) = serde_json::to_string_pretty(s) {
        let p = saved_path();
        let tmp = p.with_extension("tmp");
        if std::fs::write(&tmp, t).is_ok() {
            let _ = std::fs::rename(&tmp, &p);
        }
    }
}

pub fn remembered() -> Option<Device> {
    load().dev
}

pub fn remember(d: &Device) {
    let mut s = load();
    s.dev = Some(d.clone());
    store(&s);
}

pub fn forget() {
    let mut s = load();
    s.dev = None;
    store(&s);
}

/// Ce que l'île annonce d'elle-même (le tel l'affiche : « Île · NOM-DU-PC »).
fn my_info() -> Value {
    let pc = std::env::var("COMPUTERNAME").unwrap_or_else(|_| "PC".into());
    json!({
        "alias": format!("Île · {pc}"),
        "version": "2.1",
        "deviceModel": "Windows",
        "deviceType": "desktop",
        "fingerprint": load().me,
        "port": PORT,
        "protocol": "https",
        "download": false,
    })
}

/// Les tels LocalSend ont un certificat auto-signé : on chiffre sans vérifier l'identité (comme LocalSend),
/// et l'île présente le sien (exigé par le tel).
fn agent(read_secs: u64) -> ureq::Agent {
    let mut b = ureq::AgentBuilder::new().timeout_connect(Duration::from_millis(1500)).timeout_read(Duration::from_secs(read_secs)).timeout_write(Duration::from_secs(60));
    if let Some(tls) = TLS.clone() {
        b = b.tls_connector(tls);
    }
    b.build()
}

fn base(d: &Device) -> String {
    format!("{}://{}:{}/api/localsend/v2", if d.https { "https" } else { "http" }, d.ip, d.port)
}

/* ---------------- recherche des appareils ---------------- */

fn private(ip: &Ipv4Addr) -> bool {
    let o = ip.octets();
    o[0] == 10 || (o[0] == 172 && (16..32).contains(&o[1])) || (o[0] == 192 && o[1] == 168)
}

/// Adresses IPv4 locales du PC : d'abord celle de la route par défaut (le vrai Wi-Fi / Ethernet),
/// puis les autres cartes réseau.
fn local_ips() -> Vec<Ipv4Addr> {
    let mut v = vec![];
    if let Ok(s) = UdpSocket::bind("0.0.0.0:0") {
        // aucun paquet n'est envoyé : connect() sur de l'UDP ne fait que choisir la carte réseau
        if s.connect("8.8.8.8:80").is_ok() {
            if let Ok(SocketAddr::V4(a)) = s.local_addr() {
                v.push(*a.ip());
            }
        }
    }
    if let Ok(host) = std::env::var("COMPUTERNAME") {
        if let Ok(addrs) = (host.as_str(), 0).to_socket_addrs() {
            for a in addrs {
                if let IpAddr::V4(ip) = a.ip() {
                    if !v.contains(&ip) {
                        v.push(ip);
                    }
                }
            }
        }
    }
    v.retain(private);
    v
}

/// Demande à une adresse si c'est un appareil LocalSend (HTTPS, sinon HTTP si le chiffrement est désactivé sur le tel).
fn ask(ip: Ipv4Addr, port: u16, me: &Value) -> Option<Device> {
    for https in [true, false] {
        let d = Device { ip: ip.to_string(), port, https, ..Default::default() };
        let r = agent(3).post(&format!("{}/register", base(&d))).send_json(me.clone());
        match r {
            Ok(resp) => {
                let v: Value = resp.into_json().ok()?;
                let s = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
                return Some(Device { alias: s("alias"), fingerprint: s("fingerprint"), model: s("deviceModel"), kind: s("deviceType"), ..d });
            }
            // il a répondu en HTTP(S) mais pas comme LocalSend : inutile d'essayer l'autre
            Err(ureq::Error::Status(..)) => return None,
            Err(_) => continue,
        }
    }
    None
}

/// Les appareils LocalSend du réseau local (≈ 1 à 3 s). Le PC lui-même est ignoré.
pub fn scan() -> Vec<Device> {
    let mine = local_ips();
    let mut targets: Vec<Ipv4Addr> = vec![];
    let mut seen_nets = HashSet::new();
    for ip in mine.iter().take(4) {
        let o = ip.octets();
        if !seen_nets.insert((o[0], o[1], o[2])) {
            continue;
        }
        for last in 1..=254u8 {
            let t = Ipv4Addr::new(o[0], o[1], o[2], last);
            if !mine.contains(&t) {
                targets.push(t);
            }
        }
    }
    // 1) qui a le port 53317 ouvert (connexions en parallèle, 400 ms max chacune)
    let open: Vec<Ipv4Addr> = parallel(targets, |ip| {
        TcpStream::connect_timeout(&SocketAddr::new(IpAddr::V4(ip), PORT), Duration::from_millis(400)).ok().map(|_| ip)
    });
    // 2) lesquels sont des LocalSend
    let me = my_info();
    let mut devs: Vec<Device> = parallel(open, move |ip| ask(ip, PORT, &me));
    devs.sort_by(|a, b| (a.kind != "mobile", a.alias.to_lowercase()).cmp(&(b.kind != "mobile", b.alias.to_lowercase())));
    crate::log(&format!("LocalSend : {} appareil(s) trouvé(s) sur {} réseau(x)", devs.len(), seen_nets.len()));
    devs
}

fn parallel<T: Send + 'static, R: Send + 'static>(items: Vec<T>, f: impl Fn(T) -> Option<R> + Send + Sync + 'static) -> Vec<R> {
    let f = Arc::new(f);
    let handles: Vec<_> = items
        .into_iter()
        .filter_map(|it| {
            let f = f.clone();
            std::thread::Builder::new().stack_size(256 * 1024).spawn(move || f(it)).ok()
        })
        .collect();
    handles.into_iter().filter_map(|h| h.join().ok().flatten()).collect()
}

/// Retrouve le tel choisi : à sa dernière adresse si elle répond encore, sinon en refaisant une recherche
/// (le Wi-Fi a pu lui donner une autre adresse).
pub fn locate(d: &Device) -> Option<Device> {
    let me = my_info();
    // en HTTPS l'identifiant (fingerprint) peut être vide : on ne s'y fie que s'il est rempli
    let same_fp = |x: &Device| !x.fingerprint.is_empty() && x.fingerprint == d.fingerprint;
    if let Ok(ip) = d.ip.parse::<Ipv4Addr>() {
        if let Some(found) = ask(ip, d.port, &me) {
            if same_fp(&found) || found.alias == d.alias {
                return Some(found);
            }
        }
    }
    let all = scan();
    all.iter().find(|x| same_fp(x)).or_else(|| all.iter().find(|x| x.alias == d.alias)).cloned()
}

/* ---------------- envoi ---------------- */

fn mime(p: &Path) -> &'static str {
    match p.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase().as_str() {
        "gif" => "image/gif",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "mp4" | "m4v" => "video/mp4",
        "webm" => "video/webm",
        "mkv" => "video/x-matroska",
        "mov" => "video/quicktime",
        "mp3" => "audio/mpeg",
        "m4a" => "audio/mp4",
        "ogg" | "opus" => "audio/ogg",
        "flac" => "audio/flac",
        "wav" => "audio/wav",
        "txt" | "srt" | "vtt" => "text/plain",
        _ => "application/octet-stream",
    }
}

pub enum Step {
    /// en attente d'acceptation sur le tel
    Waiting,
    /// octets envoyés / total
    Sending(u64, u64),
}

struct Counting<R, F: FnMut(u64) -> bool> {
    inner: R,
    on: F,
}

impl<R: Read, F: FnMut(u64) -> bool> Read for Counting<R, F> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        if !(self.on)(n as u64) {
            return Err(std::io::Error::new(std::io::ErrorKind::Interrupted, "annulé"));
        }
        Ok(n)
    }
}

fn status_msg(code: u16) -> String {
    match code {
        401 => "Le tel demande un code PIN : désactive « Exiger un PIN » dans LocalSend".into(),
        403 => "Refusé sur le tel".into(),
        409 => "Le tel est occupé par un autre envoi".into(),
        429 => "Trop d'essais : réessaie dans un instant".into(),
        c => format!("Le tel a répondu {c}"),
    }
}

/// Envoie les fichiers au tel. `step` est appelé pendant l'envoi ; s'il renvoie false, l'envoi est annulé.
/// Renvoie le nombre de fichiers envoyés.
pub fn send(d: &Device, files: &[PathBuf], mut step: impl FnMut(Step) -> bool) -> Result<usize, String> {
    let mut meta = serde_json::Map::new();
    let mut total = 0u64;
    for (i, p) in files.iter().enumerate() {
        let size = std::fs::metadata(p).map_err(|_| format!("Fichier introuvable : {}", p.display()))?.len();
        total += size;
        let id = format!("f{i}");
        let name = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| id.clone());
        meta.insert(id.clone(), json!({ "id": id, "fileName": name, "size": size, "fileType": mime(p) }));
    }
    if !step(Step::Waiting) {
        return Err("Annulé".into());
    }
    // le tel affiche « Accepter ? » : on attend la réponse (2 min max)
    let resp = agent(130).post(&format!("{}/prepare-upload", base(d))).send_json(json!({ "info": my_info(), "files": meta }));
    let resp = match resp {
        Ok(r) => r,
        Err(ureq::Error::Status(code, _)) => return Err(status_msg(code)),
        Err(e) => {
            crate::log(&format!("LocalSend : prepare-upload échoué ({:?})", e.kind()));
            return Err("Tel injoignable : ouvre LocalSend sur le tel (même Wi-Fi)".into());
        }
    };
    if resp.status() == 204 {
        return Ok(0);
    }
    let v: Value = resp.into_json().map_err(|_| "Réponse illisible du tel".to_string())?;
    let session = v.get("sessionId").and_then(|x| x.as_str()).ok_or("Réponse illisible du tel")?.to_string();
    let tokens = v.get("files").and_then(|x| x.as_object()).cloned().unwrap_or_default();
    let cancel = |a: &ureq::Agent| {
        let _ = a.post(&format!("{}/cancel?sessionId={}", base(d), session)).call();
    };
    let up = agent(60);
    let mut sent = 0u64;
    let mut n = 0;
    let mut stopped = false;
    for (i, p) in files.iter().enumerate() {
        let id = format!("f{i}");
        // fichier refusé sur le tel (l'utilisateur peut décocher des fichiers) : on passe
        let Some(token) = tokens.get(&id).and_then(|x| x.as_str()) else {
            sent += meta[&id]["size"].as_u64().unwrap_or(0);
            continue;
        };
        let size = meta[&id]["size"].as_u64().unwrap_or(0);
        let file = std::fs::File::open(p).map_err(|e| e.to_string())?;
        let reader = Counting {
            inner: file,
            on: |k| {
                sent += k;
                let go = step(Step::Sending(sent, total));
                stopped |= !go;
                go
            },
        };
        let r = up
            .post(&format!("{}/upload?sessionId={}&fileId={}&token={}", base(d), session, id, token))
            .set("Content-Type", mime(p))
            .set("Content-Length", &size.to_string())
            .send(reader);
        match r {
            Ok(_) => n += 1,
            Err(_) if stopped => {
                cancel(&up);
                return Err("Annulé".into());
            }
            Err(ureq::Error::Status(code, _)) => {
                cancel(&up);
                return Err(if code == 403 { "Envoi annulé sur le tel".into() } else { status_msg(code) });
            }
            Err(e) => {
                crate::log(&format!("LocalSend : envoi interrompu ({:?})", e.kind()));
                cancel(&up);
                return Err("Envoi interrompu (Wi-Fi ou LocalSend fermé sur le tel ?)".into());
            }
        }
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    /// cargo test --release --target-dir target/build-new --bin dynamic-island localsend_scan -- --ignored --nocapture
    #[test]
    #[ignore]
    fn localsend_scan() {
        println!("IP locales : {:?}", super::local_ips());
        let t = std::time::Instant::now();
        for d in super::scan() {
            println!("{} · {} · {} · {}:{} https={}", d.alias, d.kind, d.model, d.ip, d.port, d.https);
        }
        println!("{:?}", t.elapsed());
    }
}
