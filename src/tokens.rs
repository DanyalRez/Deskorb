//! Limites d'usage Claude en direct, les mêmes que l'écran « Limites d'utilisation » de Claude :
//! session de 5 h et semaine, en % avec leur heure de réinitialisation. Demandées à Anthropic avec le jeton
//! de connexion de Claude Code (~/.claude/.credentials.json), renouvelé ici quand il expire, comme le fait
//! Claude Code. Le jeton ne quitte jamais ce fichier et api.anthropic.com / platform.claude.com.

use std::{
    path::PathBuf,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e"; // client OAuth de Claude Code

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Window {
    pub pct: f32, // 0 à 1
    pub reset_at: Option<SystemTime>,
}

impl Window {
    pub fn left(&self) -> Option<Duration> {
        self.reset_at.map(|t| t.duration_since(SystemTime::now()).unwrap_or_default())
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Usage {
    pub session: Window,
    pub weekly: Window,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Error {
    SignedOut, // pas de jeton, ou renouvellement refusé : `claude auth login`
    Offline,     // réseau ou serveur indisponible : on garde les dernières valeurs
    RateLimited, // 429 : Anthropic limite la fréquence des demandes, il faut espacer
}

fn credentials() -> Option<PathBuf> {
    Some(PathBuf::from(std::env::var_os("USERPROFILE")?).join(".claude").join(".credentials.json"))
}

/// Usage actuel. Bloquant (réseau) : à appeler hors du fil de l'interface.
pub fn read() -> Result<Usage, Error> {
    let path = credentials().ok_or(Error::SignedOut)?;
    let text = std::fs::read_to_string(&path).map_err(|_| Error::SignedOut)?;
    let expired = num_field(&text, "expiresAt").unwrap_or(0.0) < (now_ms() + 60_000) as f64;
    let token = if expired { refresh(&path, &text)? } else { str_field(&text, "accessToken").ok_or(Error::SignedOut)?.to_string() };
    match fetch(&token)? {
        (200, body) => parse_usage(&body).ok_or(Error::Offline),
        (429, _) => Err(Error::RateLimited),
        // jeton révoqué avant son heure : un seul renouvellement (le fichier n'a pas encore tourné)
        (401, _) if !expired => match fetch(&refresh(&path, &text)?)? {
            (200, body) => parse_usage(&body).ok_or(Error::Offline),
            _ => Err(Error::SignedOut),
        },
        (401, _) => Err(Error::SignedOut),
        _ => Err(Error::Offline),
    }
}

fn fetch(token: &str) -> Result<(u32, String), Error> {
    let headers = format!("Authorization: Bearer {token}\r\nanthropic-beta: oauth-2025-04-20\r\n");
    crate::win::https("GET", "api.anthropic.com", "/api/oauth/usage", &headers, None).ok_or(Error::Offline)
}

/// Échange le jeton de renouvellement contre un nouveau couple, et le réécrit dans le fichier de Claude Code
/// (les deux jetons tournent : garder l'ancien déconnecterait Claude Code).
fn refresh(path: &PathBuf, text: &str) -> Result<String, Error> {
    let old_refresh = str_field(text, "refreshToken").ok_or(Error::SignedOut)?;
    let body = format!(r#"{{"grant_type":"refresh_token","refresh_token":"{old_refresh}","client_id":"{CLIENT_ID}"}}"#);
    let (status, reply) = crate::win::https("POST", "platform.claude.com", "/v1/oauth/token",
        "Content-Type: application/json\r\n", Some(body.as_bytes())).ok_or(Error::Offline)?;
    match status {
        200 => {}
        // 429 : le serveur demande d'espacer, le jeton de renouvellement reste bon
        429 => return Err(Error::RateLimited),
        400..500 => return Err(Error::SignedOut),
        _ => return Err(Error::Offline),
    }
    let access = str_field(&reply, "access_token").ok_or(Error::Offline)?;
    let new_refresh = str_field(&reply, "refresh_token").unwrap_or(old_refresh);
    let expires_at = now_ms() + num_field(&reply, "expires_in").unwrap_or(3600.0) as u64 * 1000;
    let old_access = str_field(text, "accessToken").unwrap_or_default();
    let old_expires = num_field(text, "expiresAt").map_or(String::new(), |e| format!(r#""expiresAt":{e}"#));
    let mut updated = text.replacen(old_access, access, 1).replacen(old_refresh, new_refresh, 1);
    if !old_expires.is_empty() {
        updated = updated.replacen(&old_expires, &format!(r#""expiresAt":{expires_at}"#), 1);
    }
    // écriture atomique : un fichier à moitié écrit déconnecterait Claude Code
    let tmp = path.with_extension("json.deskorb");
    std::fs::write(&tmp, updated).and_then(|_| std::fs::rename(&tmp, path)).map_err(|_| Error::Offline)?;
    Ok(access.to_string())
}

/// `{"five_hour":{"utilization":39.0,"resets_at":"…"},"seven_day":{…}}` → les deux fenêtres.
fn parse_usage(body: &str) -> Option<Usage> {
    let window = |key: &str| -> Option<Window> {
        let s = &body[body.find(&format!("\"{key}\":"))?..];
        let s = &s[..s.find('}').unwrap_or(s.len())];
        Some(Window {
            pct: (num_field(s, "utilization")? / 100.0).clamp(0.0, 1.0) as f32,
            reset_at: str_field(s, "resets_at").and_then(epoch).map(|t| UNIX_EPOCH + Duration::from_secs(t.max(0) as u64)),
        })
    };
    Some(Usage { session: window("five_hour")?, weekly: window("seven_day")? })
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

/// Valeur d'une clé chaîne `"k":"v"` (première occurrence). Les guillemets des chaînes imbriquées sont
/// échappés, donc chercher `"clé":` ne tombe que sur de vraies clés.
fn str_field<'a>(s: &'a str, k: &str) -> Option<&'a str> {
    let i = s.find(&format!("\"{k}\":\""))? + k.len() + 4;
    Some(&s[i..i + s[i..].find('"')?])
}

/// Valeur d'une clé nombre `"k":12.5` (première occurrence).
fn num_field(s: &str, k: &str) -> Option<f64> {
    let i = s.find(&format!("\"{k}\":"))? + k.len() + 3;
    let v = s[i..].trim_start();
    v[..v.find(|c: char| !(c.is_ascii_digit() || matches!(c, '.' | '-' | 'e' | 'E' | '+'))).unwrap_or(v.len())].parse().ok()
}

/// "2026-10-25T16:32:55.326Z" ou "…+02:00" → secondes Unix (UTC).
fn epoch(ts: &str) -> Option<i64> {
    let n = |r: std::ops::Range<usize>| ts.get(r)?.parse::<i64>().ok();
    let (y, m, d) = (n(0..4)?, n(5..7)?, n(8..10)?);
    // jours depuis 1970 (algorithme « days from civil » de H. Hinnant)
    let (y, m) = if m <= 2 { (y - 1, m + 9) } else { (y, m - 3) };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + (153 * m + 2) / 5 + d - 1;
    let days = era * 146097 + doe - 719468;
    let t = days * 86400 + n(11..13)? * 3600 + n(14..16)? * 60 + n(17..19)?;
    // décalage horaire éventuel après les secondes (et les fractions)
    let tail = &ts[19..];
    let tz = tail.find(['+', '-']).map_or(0, |i| {
        let sign = if &tail[i..=i] == "-" { -1 } else { 1 };
        let num = |r: std::ops::Range<usize>| tail.get(r).and_then(|v| v.parse::<i64>().ok()).unwrap_or(0);
        sign * (num(i + 1..i + 3) * 3600 + num(i + 4..i + 6) * 60)
    });
    Some(t - tz)
}

/// « 3 h 03 », « 14 min », « 2 j 18 h » (« 2 d 18 h » en anglais).
pub fn fmt_left(d: Duration, fr: bool) -> String {
    let m = d.as_secs() / 60;
    match m {
        1440.. => format!("{} {} {} h", m / 1440, if fr { "j" } else { "d" }, m % 1440 / 60),
        60.. => format!("{} h {:02}", m / 60, m % 60),
        _ => format!("{m} min"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_usage_reply() {
        let body = r#"{"five_hour":{"utilization":39.0,"resets_at":"2026-10-25T21:32:00.000000+00:00"},"seven_day":{"utilization":42,"resets_at":"2026-10-30T15:00:00+02:00"},"seven_day_opus":null}"#;
        let u = parse_usage(body).unwrap();
        assert!((u.session.pct - 0.39).abs() < 1e-6 && (u.weekly.pct - 0.42).abs() < 1e-6);
        assert_eq!(u.session.reset_at, Some(UNIX_EPOCH + Duration::from_secs(epoch("2026-10-25T21:32:00Z").unwrap() as u64)));
        assert_eq!(epoch("2026-10-30T15:00:00+02:00"), epoch("2026-10-30T13:00:00Z"));
        // fenêtre sans réinitialisation prévue (rien consommé)
        let idle = parse_usage(r#"{"five_hour":{"utilization":0.0,"resets_at":null},"seven_day":{"utilization":3.0,"resets_at":null}}"#).unwrap();
        assert_eq!((idle.session.pct, idle.session.reset_at), (0.0, None));
        assert_eq!(parse_usage(r#"{"error":"x"}"#), None);
    }

    #[test]
    fn fields_and_format() {
        let creds = r#"{"claudeAiOauth":{"accessToken":"a1","refreshToken":"r1","expiresAt":1792945975000,"scopes":["x"]}}"#;
        assert_eq!(str_field(creds, "accessToken"), Some("a1"));
        assert_eq!(num_field(creds, "expiresAt"), Some(1792945975000.0));
        assert_eq!(epoch("1970-01-02T00:00:01.000Z"), Some(86401));
        assert_eq!(fmt_left(Duration::from_secs(3 * 3600 + 3 * 60 + 30), true), "3 h 03");
        assert_eq!(fmt_left(Duration::from_secs(14 * 60), false), "14 min");
        assert_eq!(fmt_left(Duration::from_secs(2 * 86400 + 18 * 3600 + 5), true), "2 j 18 h");
    }
}
