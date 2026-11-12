//! Détection de codes de vérification (OTP / 2FA) et de liens de connexion « magiques »
//! dans le texte d'un mail ou d'une notification (FR / EN / ES / DE / IT / PT).

use once_cell::sync::Lazy;
use regex::Regex;

#[derive(Debug, Clone, PartialEq)]
pub enum Found {
    Code(String),
    Link(String),
}

static KEYWORDS: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"(?ix)
        code | otp | passcode | password | mot\s+de\s+passe | pin\b | token | jeton |
        v[ée]rif | verify | confirm | authenti | connexion | se\s+connecter | login | log\s*in | sign\s*[- ]?in |
        2fa | two[- ]factor | deux\s+facteurs | double\s+authentification | s[ée]curit[ée] | security |
        one[- ]time | usage\s+unique | à\s+usage | c[óo]digo | verificaci | best[äa]tigung | sicherheitscode | anmelde |
        codice | verifica | acesso | コード | 验证码 | 인증",
    )
    .unwrap()
});

/// Mots qui, juste avant un nombre, indiquent que ce n'est PAS un code.
static NEGATIVE_BEFORE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)(€|\$|£|eur|usd|tél|tel|phone|n°|no\.|commande|order|facture|invoice|ref|réf|référence|colis|suivi|tracking|siret|iban|rue|bp|cedex|#)\s*[:.]?\s*$").unwrap()
});
static NEGATIVE_AFTER: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)^\s*(€|\$|eur|usd|%|px|ms|go|gb|mo|mb|km|kg|h\b|min\b|jours|days|ans|years|rue|avenue|bd)").unwrap()
});

static CAND_NUMERIC: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?:^|[^\w.,/:-])(\d{4}[ -]\d{4}|\d{3}[ -]\d{3}|\d{4,8})(?:$|[^\w/:%€$-])").unwrap());
static CAND_ALNUM: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?:^|[^\w-])((?:[A-Z]-)?[A-Z0-9]{5,10}|[A-Z0-9]{3,4}-[A-Z0-9]{3,4})(?:$|[^\w-])").unwrap());
static URL_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r#"https?://[^\s"'<>\)\]]+"#).unwrap());

static LINK_GOOD: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)verif|confirm|activ|magic|login|log-in|signin|sign-in|sign_in|auth|token|connexion|connect|valid|reset|password|otp|session|access|approve").unwrap()
});
static LINK_BAD: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)unsubscribe|desinscri|désinscri|optout|opt-out|privacy|confidentialit|terms|cgu|preferences|préférences|support|help|aide|facebook\.com|twitter\.com|\bx\.com/|instagram|linkedin|youtube|\.(png|jpe?g|gif|svg|css|woff2?)(\?|$)|tracking-pixel|/open\?|beacon").unwrap()
});

pub fn has_keywords(text: &str) -> bool {
    KEYWORDS.is_match(text)
}

/// Cherche le meilleur code dans `subject` + `body`.
pub fn find_code(subject: &str, body: &str) -> Option<String> {
    let full = format!("{subject}\n{body}");
    if !has_keywords(&full) {
        return None;
    }
    let mut best: Option<(i32, String)> = None;
    let consider = |text: &str, bonus: i32, best: &mut Option<(i32, String)>| {
        let kw_positions: Vec<usize> = KEYWORDS.find_iter(text).map(|m| m.end()).collect();
        let mut push = |start: usize, end: usize, raw: &str, base: i32| {
            let code: String = raw.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
            let before = &text[..start];
            let after = &text[end..];
            let mut score = base + bonus;
            // proximité d'un mot-clé (avant le code de préférence)
            let dist = kw_positions
                .iter()
                .map(|&k| if k <= start { (start - k) as i32 } else if k >= end { ((k - end) as i32) * 2 } else { 0 })
                .min()
                .unwrap_or(10_000);
            score += match dist {
                0..=25 => 60,
                26..=80 => 40,
                81..=200 => 20,
                201..=500 => 5,
                _ => -20,
            };
            let before_tail: String = before.chars().rev().take(14).collect::<Vec<_>>().into_iter().rev().collect();
            let after_head: String = after.chars().take(8).collect();
            if NEGATIVE_BEFORE.is_match(&before_tail) || NEGATIVE_AFTER.is_match(&after_head) {
                score -= 80;
            }
            if before_tail.trim_end().ends_with(':') {
                score += 15;
            }
            // Les années (1990–2039) et heures sont rarement des codes.
            if code.len() == 4 && code.chars().all(|c| c.is_ascii_digit()) {
                let n: u32 = code.parse().unwrap_or(0);
                if (1990..=2039).contains(&n) {
                    score -= 45;
                }
            }
            if code.chars().all(|c| c == code.chars().next().unwrap()) {
                score -= 50; // 000000, 111111…
            }
            if code.len() == 6 {
                score += 12; // le format le plus courant
            }
            if best.as_ref().map(|(s, _)| score > *s).unwrap_or(true) {
                *best = Some((score, code));
            }
        };
        for c in CAND_NUMERIC.captures_iter(text) {
            let m = c.get(1).unwrap();
            push(m.start(), m.end(), m.as_str(), 20);
        }
        for c in CAND_ALNUM.captures_iter(text) {
            let m = c.get(1).unwrap();
            let s = m.as_str();
            let digits = s.chars().filter(|c| c.is_ascii_digit()).count();
            let letters = s.chars().filter(|c| c.is_ascii_alphabetic()).count();
            // Exige un mélange lettres/chiffres (évite les mots en majuscules comme "GOOGLE").
            if digits >= 2 && letters >= 1 && digits + letters >= 5 {
                // "G-123456" (Google) → on garde tel quel mais on privilégie la partie chiffrée
                let base = if s.starts_with(|c: char| c.is_ascii_alphabetic()) && s.get(1..2) == Some("-") { 25 } else { 8 };
                push(m.start(), m.end(), s, base);
            }
        }
    };
    consider(subject, 15, &mut best);
    consider(body, 0, &mut best);
    best.filter(|(s, _)| *s >= 55).map(|(_, mut c)| {
        // "G-123456" → "123456"
        if c.len() > 6 && c.starts_with('G') && c[1..].chars().all(|x| x.is_ascii_digit()) {
            c = c[1..].to_string();
        }
        c
    })
}

/// Cherche un lien de vérification / connexion. `links` = (texte de l'ancre, url) extraits du HTML.
pub fn find_link(subject: &str, body: &str, links: &[(String, String)]) -> Option<String> {
    let full = format!("{subject}\n{body}");
    if !has_keywords(&full) {
        return None;
    }
    let mut best: Option<(i32, String)> = None;
    let mut all: Vec<(String, String)> = links.to_vec();
    for m in URL_RE.find_iter(body) {
        all.push((String::new(), m.as_str().to_string()));
    }
    for (text, url) in all {
        if LINK_BAD.is_match(&url) || LINK_BAD.is_match(&text) || url.len() > 2000 {
            continue;
        }
        let mut score = 0;
        if LINK_GOOD.is_match(&url) {
            score += 30;
        }
        if KEYWORDS.is_match(&text) || LINK_GOOD.is_match(&text) {
            score += 45;
        }
        if url.contains("token=") || url.contains("code=") || url.contains("key=") {
            score += 15;
        }
        if score >= 45 && best.as_ref().map(|(s, _)| score > *s).unwrap_or(true) {
            best = Some((score, url.trim_end_matches(['.', ',', ';']).to_string()));
        }
    }
    best.map(|(_, u)| u)
}

pub fn analyze(subject: &str, body: &str, links: &[(String, String)]) -> Option<Found> {
    if let Some(c) = find_code(subject, body) {
        return Some(Found::Code(c));
    }
    find_link(subject, body, links).map(Found::Link)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn google() {
        assert_eq!(find_code("G-482913 est votre code de validation Google", ""), Some("482913".into()));
    }
    #[test]
    fn french_body() {
        let b = "Bonjour,\n\nVoici votre code de vérification : 739 204\n\nIl expire dans 10 minutes. Commande n° 20231234.";
        assert_eq!(find_code("Votre code", b), Some("739204".into()));
    }
    #[test]
    fn english_subject() {
        assert_eq!(find_code("Your Discord verification code", "Use 551902 to sign in. © 2025 Discord"), Some("551902".into()));
    }
    #[test]
    fn alnum() {
        assert_eq!(find_code("Epic Games", "Your security code is: K7Q2PZ"), Some("K7Q2PZ".into()));
    }
    #[test]
    fn no_keyword() {
        assert_eq!(find_code("Facture", "Montant 1234 € payé le 12/03"), None);
    }
    #[test]
    fn price_not_code() {
        assert_eq!(find_code("Promo", "Code promo : économisez 2500 € sur votre commande"), None);
    }
    #[test]
    fn link() {
        let links = vec![
            ("Se désinscrire".to_string(), "https://x.com/unsubscribe?u=1".to_string()),
            ("Confirmer mon adresse".to_string(), "https://app.example.com/e/abc123".to_string()),
        ];
        assert_eq!(find_link("Confirmez votre e-mail", "Cliquez ci-dessous", &links), Some("https://app.example.com/e/abc123".into()));
    }
}
