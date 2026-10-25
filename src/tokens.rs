//! Consommation de tokens : utilisés, restants, % et temps avant réinitialisation du quota.

use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

pub struct Usage {
    pub used: u64,
    pub limit: u64,
    pub reset_at: SystemTime,
}

impl Usage {
    pub fn remaining(&self) -> u64 {
        self.limit.saturating_sub(self.used)
    }

    pub fn pct(&self) -> f32 {
        if self.limit == 0 { 0.0 } else { (self.used as f32 / self.limit as f32).min(1.0) }
    }

    pub fn left(&self) -> Duration {
        self.reset_at.duration_since(SystemTime::now()).unwrap_or_default()
    }
}

const WINDOW: i64 = 5 * 3600;

/// Messages déjà comptés : les .jsonl ne font que grandir, on ne relit que la fin de chaque fichier.
#[derive(Default)]
struct Cache {
    read: HashMap<PathBuf, u64>, // octets déjà lus
    seen: HashSet<String>,       // id du message : Claude Code écrit une ligne par bloc de contenu, même usage
    entries: Vec<(i64, u64)>,    // (horodatage s, tokens)
}

thread_local! {
    static CACHE: RefCell<Cache> = RefCell::new(Cache::default());
}

/// Fenêtre de 5 h en cours, calculée depuis les transcriptions de Claude Code (~/.claude/projects).
/// Comptage façon ccusage : entrée + sortie + écriture et lecture du cache.
/// `limit` : choisie dans les réglages ; sinon le plus gros bloc passé (comme `ccusage --token-limit max`),
/// faute de limite publiée par Anthropic.
pub fn read(limit: Option<u64>) -> Option<Usage> {
    let root = PathBuf::from(std::env::var_os("USERPROFILE")?).join(".claude").join("projects");
    CACHE.with_borrow_mut(|c| {
        let mut files = Vec::new();
        walk(&root, &mut files);
        for f in files {
            scan(c, &f);
        }
        c.entries.sort_unstable();
        let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs() as i64;
        let b = blocks(&c.entries);
        let limit = limit.or_else(|| b.iter().map(|b| b.2).max())?;
        let (used, reset) = match b.last() {
            Some(&(start, last, sum)) if now < start + WINDOW && now - last < WINDOW => (sum, start + WINDOW),
            _ => (0, now),
        };
        Some(Usage { used, limit, reset_at: UNIX_EPOCH + Duration::from_secs(reset as u64) })
    })
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = e.path();
        if p.is_dir() {
            walk(&p, out);
        } else if p.extension().is_some_and(|x| x == "jsonl") {
            out.push(p);
        }
    }
}

/// Lit ce qui a été ajouté au fichier depuis la dernière fois (lignes complètes seulement).
fn scan(c: &mut Cache, path: &Path) {
    let Ok(mut f) = File::open(path) else { return };
    let len = f.metadata().map_or(0, |m| m.len());
    let from = *c.read.get(path).unwrap_or(&0);
    if len <= from || f.seek(SeekFrom::Start(from)).is_err() {
        return;
    }
    let mut buf = Vec::new();
    if f.read_to_end(&mut buf).is_err() {
        return;
    }
    let end = buf.iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1); // ligne en cours d'écriture : plus tard
    c.read.insert(path.to_path_buf(), from + end as u64);
    for line in String::from_utf8_lossy(&buf[..end]).lines() {
        if let Some((id, t, n)) = parse(line) {
            if c.seen.insert(id.to_string()) {
                c.entries.push((t, n));
            }
        }
    }
}

/// (id du message, horodatage, tokens) d'une réponse de l'assistant. Les guillemets des chaînes JSON
/// imbriquées sont échappés, donc chercher `"clé":` ne tombe que sur de vraies clés.
fn parse(line: &str) -> Option<(&str, i64, u64)> {
    if !line.contains(r#""type":"assistant""#) {
        return None;
    }
    let usage = &line[line.find(r#""usage":{"#)?..];
    let num = |k: &str| -> u64 {
        usage.find(&format!("\"{k}\":")).map_or(0, |i| {
            let s = &usage[i + k.len() + 3..];
            s[..s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len())].parse().unwrap_or(0)
        })
    };
    let n = num("input_tokens") + num("output_tokens") + num("cache_creation_input_tokens") + num("cache_read_input_tokens");
    let msg = &line[line.find(r#""message":{"#)?..];
    let id = str_field(msg, "id")?;
    Some((id, epoch(str_field(line, "timestamp")?)?, n))
}

fn str_field<'a>(s: &'a str, k: &str) -> Option<&'a str> {
    let i = s.find(&format!("\"{k}\":\""))? + k.len() + 4;
    Some(&s[i..i + s[i..].find('"')?])
}

/// "2026-10-25T16:32:55.326Z" → secondes Unix (UTC).
fn epoch(ts: &str) -> Option<i64> {
    let n = |r: std::ops::Range<usize>| ts.get(r)?.parse::<i64>().ok();
    let (y, m, d) = (n(0..4)?, n(5..7)?, n(8..10)?);
    // jours depuis 1970 (algorithme « days from civil » de H. Hinnant)
    let (y, m) = if m <= 2 { (y - 1, m + 9) } else { (y, m - 3) };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + (153 * m + 2) / 5 + d - 1;
    let days = era * 146097 + doe - 719468;
    Some(days * 86400 + n(11..13)? * 3600 + n(14..16)? * 60 + n(17..19)?)
}

/// Blocs de 5 h (début arrondi à l'heure, dernier message, total), comme le décompte de Claude Code.
fn blocks(entries: &[(i64, u64)]) -> Vec<(i64, i64, u64)> {
    let mut out: Vec<(i64, i64, u64)> = Vec::new();
    for &(t, n) in entries {
        match out.last_mut() {
            Some(b) if t < b.0 + WINDOW && t - b.1 < WINDOW => {
                b.1 = t;
                b.2 += n;
            }
            _ => out.push((t - t.rem_euclid(3600), t, n)),
        }
    }
    out
}

/// « 2 h 05 », « 14 min », « 0 min ».
pub fn fmt_left(d: Duration) -> String {
    let m = d.as_secs() / 60;
    if m >= 60 { format!("{} h {:02}", m / 60, m % 60) } else { format!("{m} min") }
}

/// « 65 M », « 1,2 M » (« 1.2 M » en anglais), « 840 k », « 512 ».
pub fn fmt_count(n: u64, fr: bool) -> String {
    match n {
        10_000_000.. => format!("{} M", (n as f64 / 1e6).round()),
        1_000_000.. => format!("{:.1} M", n as f64 / 1e6).replace('.', if fr { "," } else { "." }),
        1_000.. => format!("{} k", n / 1000),
        _ => n.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_math_and_format() {
        let u = Usage { used: 750, limit: 1000, reset_at: SystemTime::now() };
        assert_eq!(u.remaining(), 250);
        assert_eq!(u.pct(), 0.75);
        assert_eq!(Usage { used: 5, limit: 0, reset_at: SystemTime::now() }.pct(), 0.0);
        assert_eq!(fmt_left(Duration::from_secs(2 * 3600 + 5 * 60 + 30)), "2 h 05");
        assert_eq!(fmt_left(Duration::from_secs(14 * 60)), "14 min");
        assert_eq!(fmt_count(1_234_567, true), "1,2 M");
        assert_eq!(fmt_count(1_234_567, false), "1.2 M");
        assert_eq!(fmt_count(65_000_000, false), "65 M");
        assert_eq!(fmt_count(840_100, true), "840 k");
        assert_eq!(fmt_count(512, true), "512");
    }

    #[test]
    fn parses_claude_code_lines() {
        assert_eq!(epoch("1970-01-02T00:00:01.000Z"), Some(86401));
        assert_eq!(epoch("2026-10-25T16:32:55.326Z"), Some(1_792_945_975));
        let line = r#"{"message":{"model":"m","id":"msg_1","content":[{"type":"text","text":"\"input_tokens\":999"}],"usage":{"input_tokens":2,"cache_creation_input_tokens":30,"cache_read_input_tokens":40,"output_tokens":5,"output_tokens_details":{"thinking_tokens":1}}},"type":"assistant","timestamp":"1970-01-01T00:01:00.000Z"}"#;
        assert_eq!(parse(line), Some(("msg_1", 60, 77)));
        assert_eq!(parse(&line.replace(r#""type":"assistant""#, r#""type":"user""#)), None);
    }

    #[test]
    fn five_hour_blocks() {
        let h = 3600;
        // bloc 1 : 10h30 → fin 15h00 ; 15h10 ouvre le bloc 2 ; trou de 6 h → bloc 3
        let e = [(10 * h + 1800, 1), (14 * h, 2), (15 * h + 600, 4), (21 * h + 600, 8)];
        assert_eq!(blocks(&e), vec![(10 * h, 14 * h, 3), (15 * h, 15 * h + 600, 4), (21 * h, 21 * h + 600, 8)]);
    }
}
