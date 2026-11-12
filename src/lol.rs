//! Coach League of Legends.
//!
//! Pendant une partie, lit l'état du jeu via l'API locale officielle de Riot
//! (Live Client Data : https://127.0.0.1:2999, disponible uniquement en partie) et le croise avec :
//! - les stats d'OP.GG (serveur MCP public mcp-api.op.gg : build, runes et avantage de lane
//!   pour TON matchup précis) ;
//! - Data Dragon (données statiques officielles de Riot : prix des items, profils AD/AP des champions).
//!
//! Ça donne quelques conseils très courts : niv 1-3 (respecter ou forcer), push / freeze,
//! adversaire mort ou 6, prochain item et l'or qui manque, adaptation à l'équipe adverse.

use crate::log;
use once_cell::sync::Lazy;
use parking_lot::Mutex;
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;
use tauri::{AppHandle, Emitter};

const LIVE: &str = "https://127.0.0.1:2999/liveclientdata/allgamedata";
const DD: &str = "https://ddragon.leagueoflegends.com";
const OPGG: &str = "https://mcp-api.op.gg/mcp";

#[derive(Serialize, Clone, PartialEq, Debug)]
pub struct Tip {
    pub text: String,
    /// lane | item | danger | obj | info
    pub kind: &'static str,
    pub prio: u8,
}

#[derive(Serialize, Clone, PartialEq, Debug)]
pub struct Item {
    pub id: u32,
    pub name: String,
    pub icon: String,
    pub owned: bool,
}

#[derive(Serialize, Clone, PartialEq, Default, Debug)]
pub struct Coach {
    pub champ: String,
    pub champ_icon: String,
    pub opp: Option<String>,
    pub opp_icon: Option<String>,
    /// Top, Jungle, Mid, ADC, Support, ARAM…
    pub role: String,
    /// % de victoire de ton champion dans ce matchup (OP.GG)
    pub wr: Option<u32>,
    /// « Agressif », « Défensif »…
    pub style: Option<String>,
    pub build: Vec<Item>,
    pub next: Option<Item>,
    /// or manquant pour le prochain item (0 = tu peux l'acheter)
    pub missing: Option<i64>,
    /// triés du plus important au moins important
    pub tips: Vec<Tip>,
    /// astuce OP.GG contre l'adversaire de lane (en anglais)
    pub hint: Option<String>,
    /// stats OP.GG pas encore reçues
    pub loading: bool,
}

static STATE: Lazy<Mutex<Option<Coach>>> = Lazy::new(Default::default);
/// matchups OP.GG déjà demandés : clé → None tant que la réponse n'est pas arrivée
static MATCHUPS: Lazy<Mutex<HashMap<String, Option<Arc<Matchup>>>>> = Lazy::new(Default::default);

pub fn state() -> Option<Coach> {
    STATE.lock().clone()
}

pub fn start(app: AppHandle) {
    std::thread::Builder::new().name("lol-coach".into()).spawn(move || run(app)).ok();
}

fn in_league() -> bool {
    crate::games::state()
        .running
        .map(|g| g.name.to_lowercase().contains("league of legends"))
        .unwrap_or(false)
}

/// Client HTTPS pour l'API locale du jeu uniquement (127.0.0.1) : son certificat est auto-signé par Riot.
fn local_agent() -> ureq::Agent {
    let mut b = ureq::AgentBuilder::new().timeout_connect(Duration::from_millis(600)).timeout(Duration::from_secs(3));
    if let Ok(tls) = native_tls::TlsConnector::builder()
        .danger_accept_invalid_certs(true)
        .danger_accept_invalid_hostnames(true)
        .build()
    {
        b = b.tls_connector(Arc::new(tls));
    }
    b.build()
}

fn run(app: AppHandle) {
    let agent = local_agent();
    let mut stat: Option<Arc<Static>> = None;
    let mut sent: Option<Coach> = None;
    let mut logged = String::new();
    loop {
        if !in_league() {
            if sent.take().is_some() {
                *STATE.lock() = None;
                MATCHUPS.lock().clear();
                let _ = app.emit("lol", Option::<Coach>::None);
            }
            std::thread::sleep(Duration::from_secs(3));
            continue;
        }
        // écran de chargement : l'API ne répond pas encore
        let Some(data) = agent.get(LIVE).call().ok().and_then(|r| r.into_json::<Value>().ok()) else {
            std::thread::sleep(Duration::from_secs(2));
            continue;
        };
        if stat.is_none() {
            stat = load_static().map(Arc::new);
            if stat.is_none() {
                log("coach LoL : Data Dragon injoignable");
                std::thread::sleep(Duration::from_secs(10));
                continue;
            }
        }
        let st = stat.as_ref().unwrap();
        if let Some(coach) = build(&data, st) {
            let tag = format!("{} vs {}", coach.champ, coach.opp.as_deref().unwrap_or("?"));
            if tag != logged {
                log(&format!("coach LoL : {tag} ({})", coach.role));
                logged = tag;
            }
            if sent.as_ref() != Some(&coach) {
                *STATE.lock() = Some(coach.clone());
                let _ = app.emit("lol", Some(coach.clone()));
                sent = Some(coach);
            }
        }
        std::thread::sleep(Duration::from_secs(2));
    }
}

// ---------------------------------------------------------------------------
// Données statiques (Data Dragon)
// ---------------------------------------------------------------------------

struct Champ {
    id: String,
    key: u64,
    name_en: String,
    tags: Vec<String>,
    attack: f64,
    magic: f64,
}

struct ItemData {
    name: String,
    total: i64,
    from: Vec<u32>,
    boots: bool,
}

pub struct Static {
    version: String,
    champs: HashMap<String, Champ>,
    items: HashMap<u32, ItemData>,
}

fn get_json(url: &str) -> Option<Value> {
    crate::http().get(url).timeout(Duration::from_secs(20)).call().ok()?.into_json().ok()
}

fn load_static() -> Option<Static> {
    let versions = get_json(&format!("{DD}/api/versions.json"))?;
    let version = versions.get(0)?.as_str()?.to_string();
    let ch = get_json(&format!("{DD}/cdn/{version}/data/en_US/champion.json"))?;
    let mut champs = HashMap::new();
    for (id, c) in ch.get("data")?.as_object()? {
        champs.insert(
            id.clone(),
            Champ {
                id: id.clone(),
                key: c["key"].as_str().and_then(|k| k.parse().ok()).unwrap_or(0),
                name_en: c["name"].as_str().unwrap_or(id).to_string(),
                tags: c["tags"].as_array().map(|a| a.iter().filter_map(|t| t.as_str().map(String::from)).collect()).unwrap_or_default(),
                attack: c["info"]["attack"].as_f64().unwrap_or(0.0),
                magic: c["info"]["magic"].as_f64().unwrap_or(0.0),
            },
        );
    }
    let it = get_json(&format!("{DD}/cdn/{version}/data/{}/item.json", dd_locale()))?;
    let mut items = HashMap::new();
    for (id, v) in it.get("data")?.as_object()? {
        let Ok(id) = id.parse::<u32>() else { continue };
        items.insert(
            id,
            ItemData {
                name: v["name"].as_str().unwrap_or("").to_string(),
                total: v["gold"]["total"].as_i64().unwrap_or(0),
                from: v["from"].as_array().map(|a| a.iter().filter_map(|x| x.as_str()?.parse().ok()).collect()).unwrap_or_default(),
                boots: v["tags"].as_array().map(|a| a.iter().any(|t| t == "Boots")).unwrap_or(false),
            },
        );
    }
    log(&format!("coach LoL : Data Dragon {version} ({} champions, {} items)", champs.len(), items.len()));
    Some(Static { version, champs, items })
}

impl Static {
    fn champ_icon(&self, id: &str) -> String {
        format!("{DD}/cdn/{}/img/champion/{id}.png", self.version)
    }
    fn item(&self, id: u32, owned: bool) -> Option<Item> {
        let d = self.items.get(&id)?;
        Some(Item { id, name: d.name.clone(), icon: format!("{DD}/cdn/{}/img/item/{id}.png", self.version), owned })
    }
    /// Or qu'il reste à payer pour `id`, en déduisant les composants déjà possédés.
    fn cost_left(&self, id: u32, avail: &mut Vec<u32>) -> i64 {
        if let Some(i) = avail.iter().position(|&x| x == id) {
            avail.remove(i);
            return 0;
        }
        let Some(it) = self.items.get(&id) else { return 0 };
        let comps: i64 = it.from.iter().map(|c| self.items.get(c).map(|x| x.total).unwrap_or(0)).sum();
        (it.total - comps).max(0) + it.from.iter().map(|c| self.cost_left(*c, avail)).sum::<i64>()
    }
}

// ---------------------------------------------------------------------------
// OP.GG (serveur MCP public)
// ---------------------------------------------------------------------------

/// Langue des noms d'items / conseils OP.GG : celle de l'interface.
fn dd_locale() -> &'static str {
    if crate::config::lang() == "en" { "en_US" } else { "fr_FR" }
}

#[derive(Default, Debug)]
struct Matchup {
    wr: Option<f64>,
    /// Some(true) : ton champion gagne la lane en général
    adv_me: Option<bool>,
    style: Option<String>,
    core: Vec<u32>,
    boots: Option<u32>,
    hint: Option<String>,
}

/// Appelle un outil du serveur MCP d'OP.GG et renvoie le texte de sa réponse.
fn opgg(tool: &str, args: Value) -> Option<String> {
    let body = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": tool, "arguments": args}});
    let resp = crate::http()
        .post(OPGG)
        .set("Content-Type", "application/json")
        .set("Accept", "application/json, text/event-stream")
        .timeout(Duration::from_secs(25))
        .send_string(&body.to_string())
        .ok()?;
    let mut b = Vec::new();
    std::io::Read::read_to_end(&mut std::io::Read::take(resp.into_reader(), 8 << 20), &mut b).ok()?;
    let txt = String::from_utf8(b).unwrap_or_else(|e| e.into_bytes().iter().map(|&c| c as char).collect());
    // réponse JSON directe, ou flux SSE « data: {...} »
    let part = txt.lines().find_map(|l| l.strip_prefix("data:")).map(str::trim).unwrap_or(txt.trim());
    let v: Value = serde_json::from_str(part).ok()?;
    if v.get("error").is_some() {
        return None;
    }
    v["result"]["content"][0]["text"].as_str().map(String::from)
}

/// Nom attendu par OP.GG : nom anglais en MAJUSCULES_SNAKE (« Dr. Mundo » → DR_MUNDO, « K'Sante » → KSANTE).
fn opgg_key(name: &str) -> String {
    let mut s = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            s.push(c.to_ascii_uppercase());
        } else if (c == ' ' || c == '&') && !s.ends_with('_') {
            s.push('_');
        }
    }
    s.trim_matches('_').to_string()
}

fn ids(v: &Value) -> Vec<u32> {
    v["ids"].as_array().map(|a| a.iter().filter_map(|x| x.as_u64().map(|x| x as u32)).collect()).unwrap_or_default()
}

fn fetch_matchup(me: &Champ, opp: Option<&Champ>, pos: &str, mode: &str) -> Matchup {
    let norm = |s: &str| s.to_lowercase().chars().filter(|c| c.is_alphanumeric()).collect::<String>();
    if let Some(o) = opp {
        for (a, b) in [(opgg_key(&me.name_en), opgg_key(&o.name_en)), (me.id.to_uppercase(), o.id.to_uppercase())] {
            let Some(text) = opgg("lol_get_lane_matchup_guide", json!({"position": pos, "my_champion": a, "opponent_champion": b, "lang": dd_locale()})) else {
                continue;
            };
            let Ok(v) = serde_json::from_str::<Value>(&text) else { continue };
            let d = &v["data"];
            let wr = d["counters"].as_array().and_then(|a| a.iter().find(|c| c["champion_id"].as_u64() == Some(o.key))).and_then(|c| {
                let (p, w) = (c["play"].as_f64()?, c["win"].as_f64()?);
                (p >= 40.0).then(|| w / p)
            });
            let my_name = norm(v["my_champion"].as_str().unwrap_or(""));
            let adv = d["lane_advantage_champion"].as_str().filter(|s| !s.is_empty()).map(|s| norm(s) == my_name);
            let style = d["recommended_play_style"].as_str().map(|s| match s {
                "aggressive" => "Agressif".to_string(),
                "defensive" => "Défensif".to_string(),
                "passive" => "Passif".to_string(),
                "balanced" => "Équilibré".to_string(),
                o => o.to_string(),
            });
            return Matchup {
                wr,
                adv_me: adv,
                style,
                core: d["core_items"].get(0).map(ids).unwrap_or_default(),
                boots: d["boots"].get(0).and_then(|b| ids(b).first().copied()),
                hint: d["opponent_champion_tip"].as_str().filter(|s| !s.is_empty()).map(String::from),
            };
        }
    }
    // pas d'adversaire de lane (ARAM, partie normale sans rôles…) : build général du champion
    let game_mode = if mode == "ARAM" { "aram" } else { "ranked" };
    let pos = if pos == "none" { "mid" } else { pos };
    let fields = ["data.core_items.{ids[],ids_names[],pick_rate,play,win}", "data.boots.{ids[],ids_names[],pick_rate,play,win}"];
    for name in [opgg_key(&me.name_en), me.id.to_uppercase()] {
        let Some(text) = opgg(
            "lol_get_champion_analysis",
            json!({"game_mode": game_mode, "champion": name, "position": pos, "lang": dd_locale(), "desired_output_fields": fields}),
        ) else {
            continue;
        };
        // format compact d'OP.GG : CoreItems([6676,6696,6694],[…],…) ×2 (core puis bottes)
        let re = regex::Regex::new(r"CoreItems\(\[([0-9,\s]*)\]").unwrap();
        let lists: Vec<Vec<u32>> = re
            .captures_iter(&text)
            .map(|c| c[1].split(',').filter_map(|x| x.trim().parse().ok()).collect())
            .collect();
        if lists.is_empty() {
            continue;
        }
        return Matchup {
            core: lists.first().cloned().unwrap_or_default(),
            boots: lists.get(1).and_then(|b| b.first().copied()),
            ..Default::default()
        };
    }
    Matchup::default()
}

/// Matchup en cache, ou demandé en arrière-plan (None tant qu'il n'est pas arrivé).
fn matchup(st: &Static, me: &str, opp: Option<&str>, pos: &str, mode: &str) -> Option<Arc<Matchup>> {
    let key = format!("{me}|{}|{pos}|{mode}", opp.unwrap_or(""));
    let mut cache = MATCHUPS.lock();
    if let Some(m) = cache.get(&key) {
        return m.clone();
    }
    cache.insert(key.clone(), None);
    drop(cache);
    let (Some(me_c), opp_c) = (st.champs.get(me), opp.and_then(|o| st.champs.get(o))) else {
        MATCHUPS.lock().insert(key, Some(Arc::new(Matchup::default())));
        return None;
    };
    let me_c = Champ { id: me_c.id.clone(), key: me_c.key, name_en: me_c.name_en.clone(), tags: vec![], attack: 0.0, magic: 0.0 };
    let opp_c = opp_c.map(|o| Champ { id: o.id.clone(), key: o.key, name_en: o.name_en.clone(), tags: vec![], attack: 0.0, magic: 0.0 });
    let (pos, mode) = (pos.to_string(), mode.to_string());
    std::thread::spawn(move || {
        let m = fetch_matchup(&me_c, opp_c.as_ref(), &pos, &mode);
        log(&format!("coach LoL : OP.GG {} vs {} → {} items, wr {:?}", me_c.id, opp_c.as_ref().map(|o| o.id.as_str()).unwrap_or("-"), m.core.len(), m.wr));
        MATCHUPS.lock().insert(key, Some(Arc::new(m)));
    });
    None
}

// ---------------------------------------------------------------------------
// Moteur de conseils
// ---------------------------------------------------------------------------

/// Gros soigneurs (identifiants Data Dragon) : justifient un item anti-heal.
const HEALERS: &[&str] = &[
    "Aatrox", "Soraka", "Yuumi", "Vladimir", "Sylas", "DrMundo", "Warwick", "Swain", "Briar", "Illaoi", "Fiddlesticks",
    "Nami", "Sona", "Seraphine", "Senna", "Zac", "Volibear", "Trundle", "Olaf", "Gwen", "Viego", "Belveth", "Nasus",
    "Renata", "Milio", "Kayn", "Elise", "Maokai", "Udyr", "Irelia", "Samira", "Yone", "Hecarim",
];

fn champ_id(p: &Value) -> String {
    p["rawChampionName"].as_str().and_then(|s| s.rsplit('_').next()).unwrap_or("").to_string()
}

fn has_spell(p: &Value, spell: &str) -> bool {
    ["summonerSpellOne", "summonerSpellTwo"]
        .iter()
        .any(|k| p["summonerSpells"][k]["rawDisplayName"].as_str().map(|s| s.contains(spell)).unwrap_or(false))
}

fn num(v: &Value) -> i64 {
    v.as_i64().or_else(|| v.as_f64().map(|f| f as i64)).unwrap_or(0)
}

pub fn build(d: &Value, st: &Static) -> Option<Coach> {
    let players = d["allPlayers"].as_array()?;
    let ap = &d["activePlayer"];
    let same = |p: &Value, k: &str, a: &str| {
        let x = ap[a].as_str().unwrap_or("");
        !x.is_empty() && p[k].as_str() == Some(x)
    };
    let me = players
        .iter()
        .find(|p| same(p, "riotId", "riotId") || same(p, "summonerName", "summonerName") || same(p, "riotIdGameName", "riotIdGameName"))?;
    let team = me["team"].as_str().unwrap_or("");
    let enemies: Vec<&Value> = players.iter().filter(|p| p["team"].as_str() != Some(team)).collect();
    let mode = d["gameData"]["gameMode"].as_str().unwrap_or("CLASSIC").to_string();
    let t = d["gameData"]["gameTime"].as_f64().unwrap_or(0.0);
    let my_id = champ_id(me);

    let mut pos = me["position"].as_str().unwrap_or("").to_string();
    if pos.is_empty() && has_spell(me, "SummonerSmite") {
        pos = "JUNGLE".into();
    }
    let classic = mode == "CLASSIC";
    let opp = if classic && !pos.is_empty() { enemies.iter().find(|p| p["position"].as_str() == Some(pos.as_str())).copied() } else { None };
    let (role, opgg_pos) = match (classic, pos.as_str()) {
        (false, _) if mode == "ARAM" => ("ARAM", "mid"),
        (_, "TOP") => ("Top", "top"),
        (_, "JUNGLE") => ("Jungle", "jungle"),
        (_, "MIDDLE") => ("Mid", "mid"),
        (_, "BOTTOM") => ("ADC", "adc"),
        (_, "UTILITY") => ("Support", "support"),
        _ => ("", "none"),
    };
    let opp_id = opp.map(champ_id);
    let m = matchup(st, &my_id, opp_id.as_deref(), opgg_pos, &mode);

    let mut c = Coach {
        champ: me["championName"].as_str().unwrap_or(&my_id).to_string(),
        champ_icon: st.champ_icon(&my_id),
        opp: opp.and_then(|o| o["championName"].as_str().map(String::from)),
        opp_icon: opp_id.as_deref().map(|o| st.champ_icon(o)),
        role: role.into(),
        loading: m.is_none(),
        ..Default::default()
    };

    // ----- items
    let owned: Vec<u32> = me["items"].as_array().map(|a| a.iter().filter_map(|i| i["itemID"].as_u64().map(|x| x as u32)).collect()).unwrap_or_default();
    let owned_set: HashSet<u32> = owned.iter().copied().collect();
    let has_boots = owned.iter().any(|i| st.items.get(i).map(|x| x.boots && x.total > 300).unwrap_or(false));
    let gold = ap["currentGold"].as_f64().unwrap_or(0.0) as i64;
    if let Some(m) = &m {
        c.wr = m.wr.map(|w| (w * 100.0).round() as u32);
        c.style = m.style.clone();
        c.hint = m.hint.clone();
        let mut order: Vec<u32> = m.core.iter().take(1).copied().collect();
        if let Some(b) = m.boots {
            order.push(b);
        }
        order.extend(m.core.iter().skip(1).copied());
        c.build = order.iter().filter_map(|&i| st.item(i, owned_set.contains(&i) || (Some(i) == m.boots && has_boots))).collect();
        if let Some(n) = c.build.iter().find(|i| !i.owned) {
            let left = st.cost_left(n.id, &mut owned.clone());
            c.missing = Some((left - gold).max(0));
            c.next = Some(n.clone());
        }
    }

    // ----- conseils
    let mut tips: Vec<Tip> = Vec::new();
    let mut tip = |text: String, kind: &'static str, prio: u8| tips.push(Tip { text, kind, prio });
    let dead = me["isDead"].as_bool().unwrap_or(false);
    let lvl = num(&me["level"]);
    let next_name = c.next.as_ref().map(|n| n.name.clone());
    let can_buy = c.missing == Some(0);

    if dead {
        if let (Some(n), true) = (&next_name, can_buy) {
            tip(format!("Achète {n}"), "item", 95);
        } else if let (Some(n), Some(miss)) = (&next_name, c.missing) {
            tip(format!("{n} : il manque {miss} po"), "item", 50);
        }
    } else if let (Some(n), true) = (&next_name, can_buy) {
        if mode == "ARAM" {
            tip(format!("{n} dispo à ta mort"), "item", 30);
        } else {
            tip(format!("Recall : {n}"), "item", 72);
        }
    }

    if let (Some(o), true) = (opp, pos != "JUNGLE") {
        let oname = c.opp.clone().unwrap_or_default();
        let olvl = num(&o["level"]);
        let (ok, od) = (num(&o["scores"]["kills"]), num(&o["scores"]["deaths"]));
        let (mk, md) = (num(&me["scores"]["kills"]), num(&me["scores"]["deaths"]));
        let lane_phase = t < 14.0 * 60.0;
        if o["isDead"].as_bool().unwrap_or(false) && !dead {
            let r = o["respawnTimer"].as_f64().unwrap_or(0.0).round() as i64;
            if r >= 6 {
                if lane_phase {
                    tip(format!("{oname} mort {r}s : push + plates"), "lane", 85);
                } else {
                    tip(format!("{oname} mort {r}s : push la tour"), "lane", 85);
                }
            }
        }
        if lane_phase && !dead {
            if olvl >= 6 && lvl < 6 {
                tip(format!("{oname} a son ulti : prudence"), "danger", 80);
            } else if lvl >= 6 && olvl < 6 {
                tip("Ulti avant lui : cherche l'all-in".into(), "lane", 75);
            }
            if ok >= 2 && ok >= mk + 2 {
                tip(format!("{oname} fed : farm safe, ward"), "danger", 65);
            }
            if lvl <= 3 && t < 6.0 * 60.0 {
                let adv = m.as_ref().and_then(|m| m.adv_me.or(m.wr.map(|w| w >= 0.5)));
                match adv {
                    Some(true) => tip("Niv 1-3 : force les trades".into(), "lane", 60),
                    Some(false) => tip(format!("Niv 1-3 : respecte {oname}"), "lane", 60),
                    None => tip("Niv 1-3 : trades courts, farm".into(), "lane", 50),
                }
            } else if t > 150.0 {
                // avance approximative dans la lane : niveaux, farm, kills
                let lead = (lvl - olvl) * 2 + (num(&me["scores"]["creepScore"]) - num(&o["scores"]["creepScore"])) / 15 + (mk - md) - (ok - od);
                let s = match lead {
                    l if l >= 4 => "Tu mènes : freeze, prive-le de farm",
                    l if l >= 2 => "Avantage : slow push puis plates",
                    l if l <= -3 => "En retard : freeze sous ta tour",
                    _ => "Égal : trade quand il last-hit",
                };
                tip(s.into(), "lane", 40);
            }
        }
    }

    // objectifs (drake toutes les 5 min, Nashor à 20 min puis 6 min après sa mort)
    if classic && !dead {
        let last = |name: &str| {
            d["events"]["Events"]
                .as_array()
                .and_then(|a| a.iter().filter(|e| e["EventName"] == name).filter_map(|e| e["EventTime"].as_f64()).last())
        };
        let drake = last("DragonKill").map(|x| x + 300.0).unwrap_or(300.0);
        let baron = last("BaronKill").map(|x| x + 360.0).unwrap_or(1200.0);
        let near = |at: f64| (0.0..=60.0).contains(&(at - t)).then(|| (at - t).round() as i64);
        if let (Some(s), true) = (near(drake), ["MIDDLE", "BOTTOM", "UTILITY", "JUNGLE"].contains(&pos.as_str())) {
            tip(format!("Drake dans {s}s : prio ta lane"), "obj", 55);
        }
        if let Some(s) = near(baron) {
            tip(format!("Nashor dans {s}s : vision"), "obj", 55);
        }
    }

    // adaptation à l'équipe adverse
    let mine = st.champs.get(&my_id);
    let (mut ad, mut apn) = (0, 0);
    let mut healers = 0;
    for e in &enemies {
        let id = champ_id(e);
        if let Some(ch) = st.champs.get(&id) {
            if ch.magic > ch.attack + 1.0 {
                apn += 1;
            } else if ch.attack > ch.magic + 1.0 {
                ad += 1;
            }
        }
        if HEALERS.contains(&id.as_str()) {
            healers += 1;
        }
    }
    let item_name = |id: u32| st.items.get(&id).map(|i| i.name.clone());
    if t > 6.0 * 60.0 && healers >= 2 {
        let id = match mine {
            Some(ch) if ch.tags.first().map(|t| t == "Tank").unwrap_or(false) => 3075, // Cotte épineuse
            Some(ch) if ch.magic > ch.attack => 3165,                                // Morellonomicon
            Some(ch) if ch.tags.first().map(|t| t == "Marksman").unwrap_or(false) => 3033, // Rappel mortel
            _ => 6609,                                                                // Chempunk
        };
        let alt = [3123, 3916, 3076]; // composants anti-heal (Appel du bourreau, Orbe d'oubli, Veste épineuse)
        let got = owned.iter().any(|i| *i == id || alt.contains(i));
        if let (Some(n), false) = (item_name(id).or_else(|| item_name(3123)), got) {
            tip(format!("{healers} soigneurs : {n}"), "item", 45);
        }
    }
    if !has_boots && pos != "UTILITY" {
        if ad >= 4 {
            if let Some(n) = item_name(3047) {
                tip(format!("{ad} AD en face : {n}"), "item", 35);
            }
        } else if apn >= 3 {
            if let Some(n) = item_name(3111) {
                tip(format!("{apn} AP en face : {n}"), "item", 35);
            }
        }
    }
    if let (Some(n), Some(miss)) = (&next_name, c.missing) {
        if miss > 0 && !dead {
            tip(format!("Prochain : {n} (-{miss} po)"), "item", 20);
        }
    }
    if let Some(s) = &c.style {
        if t < 90.0 {
            tip(format!("Matchup : jeu {}", s.to_lowercase()), "info", 25);
        }
    }

    let mut seen = HashSet::new();
    tips.sort_by(|a, b| b.prio.cmp(&a.prio));
    tips.retain(|t| seen.insert(t.text.clone()));
    tips.truncate(4);
    c.tips = tips;
    Some(c)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn player(name: &str, champ_fr: &str, id: &str, team: &str, pos: &str, lvl: i64, k: i64, d: i64, cs: i64, items: &[u32]) -> Value {
        json!({
            "riotId": format!("{name}#EUW"), "riotIdGameName": name, "summonerName": name,
            "championName": champ_fr, "rawChampionName": format!("game_character_displayname_{id}"),
            "team": team, "position": pos, "level": lvl, "isDead": false, "respawnTimer": 0.0,
            "scores": {"kills": k, "deaths": d, "assists": 0, "creepScore": cs},
            "items": items.iter().map(|i| json!({"itemID": i})).collect::<Vec<_>>(),
            "summonerSpells": {"summonerSpellOne": {"rawDisplayName": "GeneratedTip_SummonerSpell_SummonerFlash_DisplayName"},
                               "summonerSpellTwo": {"rawDisplayName": "GeneratedTip_SummonerSpell_SummonerTeleport_DisplayName"}}
        })
    }

    fn game(t: f64, my_lvl: i64, opp_lvl: i64, gold: f64, opp_dead: bool) -> Value {
        let mut opp = player("Bob", "Garen", "Garen", "CHAOS", "TOP", opp_lvl, 0, 1, 40, &[]);
        if opp_dead {
            opp["isDead"] = json!(true);
            opp["respawnTimer"] = json!(18.4);
        }
        json!({
            "activePlayer": {"riotId": "Moi#EUW", "summonerName": "Moi", "currentGold": gold, "level": my_lvl},
            "allPlayers": [
                player("Moi", "Darius", "Darius", "ORDER", "TOP", my_lvl, 1, 0, 52, &[1055, 1001]),
                player("A", "Lee Sin", "LeeSin", "ORDER", "JUNGLE", 5, 0, 0, 30, &[]),
                opp,
                player("C", "Soraka", "Soraka", "CHAOS", "UTILITY", 4, 0, 0, 5, &[]),
                player("D", "Vladimir", "Vladimir", "CHAOS", "MIDDLE", 6, 0, 0, 60, &[]),
                player("E", "Kayle", "Kayle", "CHAOS", "BOTTOM", 5, 0, 0, 50, &[]),
                player("F", "Sylas", "Sylas", "CHAOS", "JUNGLE", 5, 0, 0, 40, &[]),
            ],
            "events": {"Events": [{"EventName": "GameStart", "EventTime": 0.0}]},
            "gameData": {"gameMode": "CLASSIC", "gameTime": t}
        })
    }

    #[test]
    #[ignore]
    fn coach_live() {
        let st = load_static().expect("ddragon");
        assert_eq!(opgg_key("Dr. Mundo"), "DR_MUNDO");
        assert_eq!(opgg_key("K'Sante"), "KSANTE");
        // 1er passage : OP.GG demandé en arrière-plan
        let c = build(&game(60.0, 2, 2, 300.0, false), &st).unwrap();
        assert!(c.loading);
        for _ in 0..60 {
            if MATCHUPS.lock().values().all(|m| m.is_some()) {
                break;
            }
            std::thread::sleep(Duration::from_millis(500));
        }
        for (label, g) in [
            ("début", game(70.0, 2, 2, 300.0, false)),
            ("adversaire mort", game(420.0, 7, 6, 900.0, true)),
            ("ulti adverse", game(400.0, 5, 6, 1200.0, false)),
            ("recall", game(600.0, 8, 8, 3400.0, false)),
            ("drake", game(1450.0, 12, 11, 500.0, false)),
        ] {
            let c = build(&g, &st).unwrap();
            println!("--- {label}: {} vs {:?} [{}] wr {:?} {:?}", c.champ, c.opp, c.role, c.wr, c.style);
            println!("    build: {:?}", c.build.iter().map(|i| format!("{}{}", i.name, if i.owned { "✓" } else { "" })).collect::<Vec<_>>());
            println!("    next: {:?} manque {:?}", c.next.as_ref().map(|n| &n.name), c.missing);
            for t in &c.tips {
                println!("    [{}:{}] {}", t.kind, t.prio, t.text);
            }
            println!("    hint: {:?}", c.hint);
        }
    }
}
