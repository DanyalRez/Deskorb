//! Usage Pro : ce que Claude Code a consommé, lu dans ses transcriptions (~/.claude/projects/**/*.jsonl).
//! Lecture incrémentale (un offset par fichier), 7 jours gardés en mémoire, coût estimé au tarif de l'API.
//! Les % officiels du forfait viennent d'ailleurs (tokens.rs) : ici, le détail par projet et par jour.

use std::{
    collections::HashMap,
    fs::File,
    hash::{DefaultHasher, Hash, Hasher},
    io::{BufRead, BufReader, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};

const KEEP: i64 = 7 * 86400;

/// $/MTok : entrée, sortie, lecture du cache. Écriture du cache : ×1,25 (5 min), ×2 (1 h) l'entrée.
/// Tarifs API publics (référence Anthropic, octobre 2026) ; le plus précis d'abord, car on compare des préfixes.
// shortcut: lecture du cache à 0,1× l'entrée quand le tarif n'est pas publié à part ; Opus 4.0/4.1 (15/75) rangés avec 4.6+
const PRICES: [(&str, f64, f64, f64); 12] = [
    ("claude-fable-5-1", 10.0, 50.0, 0.25),
    ("claude-mythos-5-1", 10.0, 50.0, 0.25),
    ("claude-fable-5", 10.0, 50.0, 1.0),
    ("claude-opus-5-5", 4.0, 20.0, 0.20),
    ("claude-opus-5", 5.0, 25.0, 0.50),
    ("claude-opus-4", 5.0, 25.0, 0.50),
    ("claude-sonnet-5-5", 2.0, 10.0, 0.20),
    ("claude-sonnet-5", 2.0, 10.0, 0.20),
    ("claude-sonnet-4", 3.0, 15.0, 0.30),
    ("claude-haiku-5-5", 0.10, 0.50, 0.01),
    ("claude-haiku-4", 1.0, 5.0, 0.10),
    ("claude-3", 3.0, 15.0, 0.30),
];
const UNKNOWN: u8 = u8::MAX;

/// Une réponse de l'assistant (32 octets).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Entry {
    pub t: i64,
    pub project: u16,
    pub model: u8, // index dans PRICES, UNKNOWN sinon (coût 0, tokens comptés)
    pub input: u32,
    pub output: u32,
    pub cache_5m: u32,
    pub cache_1h: u32,
    pub cache_read: u32,
}

impl Entry {
    pub fn tokens(&self) -> u64 {
        [self.input, self.output, self.cache_5m, self.cache_1h, self.cache_read].iter().map(|&n| n as u64).sum()
    }

    /// Coût équivalent API, en dollars.
    pub fn cost(&self) -> f64 {
        let Some(&(_, i, o, r)) = PRICES.get(self.model as usize) else { return 0.0 };
        (self.input as f64 * i + self.output as f64 * o + self.cache_5m as f64 * i * 1.25
            + self.cache_1h as f64 * i * 2.0 + self.cache_read as f64 * r) / 1e6
    }
}

#[derive(Default)]
struct FileState {
    offset: u64,
    last_id: u64, // Claude Code écrit une ligne par bloc de contenu, toutes avec le même message.id et le même usage
}

#[derive(Default)]
pub struct Reader {
    files: HashMap<PathBuf, FileState>,
    by_cwd: HashMap<String, u16>, // dossier de travail → projet, pour ne chercher le dépôt git qu'une fois
    pub projects: Vec<String>,
    pub entries: Vec<Entry>,
}

/// Partagé : mis à jour dans un fil à part, lu brièvement par l'interface.
pub static READER: Mutex<Option<Reader>> = Mutex::new(None);

pub fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

impl Reader {
    /// Lit ce qui a été ajouté aux transcriptions des 7 derniers jours et oublie le reste.
    pub fn update(&mut self, now: i64) {
        let Some(home) = std::env::var_os("USERPROFILE") else { return };
        let mut files = Vec::new();
        walk(&PathBuf::from(home).join(".claude").join("projects"), now - KEEP, &mut files);
        for f in files {
            self.scan(&f, now - KEEP);
        }
        self.entries.retain(|e| e.t >= now - KEEP);
        self.files.retain(|p, _| p.exists());
    }

    fn scan(&mut self, path: &Path, since: i64) {
        let Ok(mut f) = File::open(path) else { return };
        let len = f.metadata().map_or(0, |m| m.len());
        let st = self.files.entry(path.to_path_buf()).or_default();
        if len < st.offset {
            *st = FileState::default(); // fichier réécrit : on repart du début
        }
        if len == st.offset || f.seek(SeekFrom::Start(st.offset)).is_err() {
            return;
        }
        // ligne par ligne, tampon réutilisé : jamais un fichier entier en mémoire
        let mut r = BufReader::with_capacity(64 * 1024, f);
        let mut buf = Vec::new();
        let (mut offset, mut last_id) = (st.offset, st.last_id);
        loop {
            buf.clear();
            let n = match r.read_until(b'\n', &mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            if buf.last() != Some(&b'\n') {
                break; // ligne en cours d'écriture : relue au prochain passage
            }
            offset += n as u64;
            let Ok(line) = std::str::from_utf8(&buf) else { continue };
            let Some(p) = parse(line) else { continue };
            let id = hash(p.id);
            if id == last_id || p.t < since {
                last_id = id;
                continue;
            }
            last_id = id;
            let project = match self.by_cwd.get(p.cwd) {
                Some(&i) => i,
                None => {
                    let i = intern(&mut self.projects, &project_name(p.cwd));
                    self.by_cwd.insert(p.cwd.to_string(), i);
                    i
                }
            };
            self.entries.push(Entry { t: p.t, project, model: model_index(p.model), input: p.input, output: p.output,
                cache_5m: p.cache_5m, cache_1h: p.cache_1h, cache_read: p.cache_read });
        }
        let st = self.files.get_mut(path).unwrap();
        (st.offset, st.last_id) = (offset, last_id);
    }

    /// Projets classés par coût depuis `since` : (nom, tokens, coût $), les `n` premiers.
    pub fn top_projects(&self, since: i64, n: usize) -> Vec<(&str, u64, f64)> {
        let mut by: HashMap<u16, (u64, f64)> = HashMap::new();
        for e in self.entries.iter().filter(|e| e.t >= since) {
            let s = by.entry(e.project).or_default();
            s.0 += e.tokens();
            s.1 += e.cost();
        }
        let mut v: Vec<_> = by.into_iter().map(|(p, (t, c))| (self.projects[p as usize].as_str(), t, c)).collect();
        v.sort_by(|a, b| b.2.total_cmp(&a.2).then(b.1.cmp(&a.1)));
        v.truncate(n);
        v
    }

    /// Coût équivalent API depuis `since`.
    pub fn cost_since(&self, since: i64) -> f64 {
        self.entries.iter().filter(|e| e.t >= since).map(Entry::cost).sum()
    }

    /// Prévision de la fenêtre de 5 h qui commence à `start` et se réinitialise à `reset`, au % officiel `pct`.
    pub fn forecast(&self, pct: f32, start: i64, reset: i64, now: i64) -> Forecast {
        let recent_from = (now - RATE_WINDOW).max(start);
        forecast(pct, self.cost_since(start), self.cost_since(recent_from), now - recent_from, now, reset)
    }

    /// Totaux des 7 derniers jours locaux (le dernier = aujourd'hui) : (tokens, coût $).
    /// `tz` : décalage de l'heure locale sur UTC, en secondes.
    pub fn daily(&self, now: i64, tz: i64) -> [(u64, f64); 7] {
        let today = (now + tz).div_euclid(86400);
        let mut days = [(0, 0.0); 7];
        for e in &self.entries {
            let back = today - (e.t + tz).div_euclid(86400);
            if (0..7).contains(&back) {
                let d = &mut days[6 - back as usize];
                d.0 += e.tokens();
                d.1 += e.cost();
            }
        }
        days
    }

    /// Début de la fenêtre de 5 h en cours, quand l'API ne le donne pas : premier message après un trou de 5 h.
    pub fn local_block_start(&self, now: i64) -> i64 {
        let mut t: Vec<i64> = self.entries.iter().map(|e| e.t).filter(|&t| t > now - 10 * 3600).collect();
        t.sort_unstable();
        let mut start = now;
        for (i, &x) in t.iter().enumerate() {
            if i == 0 || x - t[i - 1] >= 5 * 3600 || x >= start + 5 * 3600 {
                start = x;
            }
        }
        if now - start < 5 * 3600 { start } else { now }
    }
}

fn walk(dir: &Path, since: i64, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = e.path();
        if p.is_dir() {
            walk(&p, since, out);
        } else if p.extension().is_some_and(|x| x == "jsonl") {
            let fresh = e.metadata().and_then(|m| m.modified()).ok()
                .and_then(|m| m.duration_since(UNIX_EPOCH).ok()).is_some_and(|d| d.as_secs() as i64 >= since);
            if fresh {
                out.push(p);
            }
        }
    }
}

fn hash(s: &str) -> u64 {
    let mut h = DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

fn intern(v: &mut Vec<String>, name: &str) -> u16 {
    match v.iter().position(|p| p == name) {
        Some(i) => i as u16,
        None => {
            v.push(name.to_string());
            (v.len() - 1) as u16
        }
    }
}

/// Sous-dossiers qu'une session visite sans être un projet à part.
const GENERIC: [&str; 14] = ["assets", "docs", "doc", "src", "logo", "logos", "concepts", "renders", "media", "images",
    "target", "dist", "build", "scratchpad"];

/// Nom lisible du projet : le dépôt git qui contient le dossier de travail (une session qui descend dans
/// `DESKORB\assets\logo` reste dans DESKORB). Sans dépôt (dossier sans git, renommé ou supprimé) : on remonte
/// au-dessus des sous-dossiers génériques. Le JSON double les antislashs ; Git Bash écrit `/c/…` pour `C:\…`.
fn project_name(cwd: &str) -> String {
    let mut cwd = cwd.replace(r"\\", r"\");
    if let Some(rest) = cwd.strip_prefix('/').filter(|r| r.as_bytes().get(1) == Some(&b'/')) {
        cwd = format!("{}:{}", &rest[..1], &rest[1..]);
    }
    let path = PathBuf::from(cwd);
    let root = path.ancestors().find(|a| a.join(".git").exists()).unwrap_or_else(|| {
        path.ancestors().find(|a| a.file_name().is_some_and(|n| !GENERIC.contains(&n.to_string_lossy().to_lowercase().as_str())))
            .unwrap_or(&path)
    });
    root.file_name().map_or_else(|| "?".into(), |n| n.to_string_lossy().into_owned())
}

fn model_index(model: &str) -> u8 {
    PRICES.iter().position(|p| model.starts_with(p.0)).map_or(UNKNOWN, |i| i as u8)
}

#[derive(Debug, PartialEq)]
struct Parsed<'a> {
    id: &'a str,
    t: i64,
    model: &'a str,
    cwd: &'a str,
    input: u32,
    output: u32,
    cache_5m: u32,
    cache_1h: u32,
    cache_read: u32,
}

/// Une réponse de l'assistant. Les guillemets des chaînes JSON imbriquées sont échappés, donc chercher
/// `"clé":` ne tombe que sur de vraies clés ; dans `usage`, les compteurs principaux viennent avant `iterations`.
fn parse(line: &str) -> Option<Parsed<'_>> {
    if !line.contains(r#""type":"assistant""#) {
        return None;
    }
    let msg = &line[line.find(r#""message":{"#)?..];
    let usage = &line[line.find(r#""usage":{"#)?..];
    let num = |k: &str| -> u32 {
        usage.find(&format!("\"{k}\":")).map_or(0, |i| {
            let s = &usage[i + k.len() + 3..];
            s[..s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len())].parse().unwrap_or(0)
        })
    };
    // écriture du cache détaillée par durée ; à défaut, tout compte en 5 min
    let (w5, w1) = (num("ephemeral_5m_input_tokens"), num("ephemeral_1h_input_tokens"));
    let (cache_5m, cache_1h) = if w5 + w1 > 0 { (w5, w1) } else { (num("cache_creation_input_tokens"), 0) };
    Some(Parsed {
        id: str_field(msg, "id")?,
        t: epoch(str_field(line, "timestamp")?)?,
        model: str_field(msg, "model").unwrap_or(""),
        cwd: str_field(line, "cwd").unwrap_or("?"),
        input: num("input_tokens"),
        output: num("output_tokens"),
        cache_5m,
        cache_1h,
        cache_read: num("cache_read_input_tokens"),
    })
}

fn str_field<'a>(s: &'a str, k: &str) -> Option<&'a str> {
    let i = s.find(&format!("\"{k}\":\""))? + k.len() + 4;
    Some(&s[i..i + s[i..].find('"')?])
}

/// "2026-10-25T16:32:55.326Z" → secondes Unix (UTC). Claude Code écrit toujours en UTC (Z).
fn epoch(ts: &str) -> Option<i64> {
    let n = |r: std::ops::Range<usize>| ts.get(r)?.parse::<i64>().ok();
    let (y, m, d) = (n(0..4)?, n(5..7)?, n(8..10)?);
    // jours depuis 1970 (algorithme « days from civil » de H. Hinnant)
    let (y, m) = if m <= 2 { (y - 1, m + 9) } else { (y, m - 3) };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + (153 * m + 2) / 5 + d - 1;
    Some((era * 146097 + doe - 719468) * 86400 + n(11..13)? * 3600 + n(14..16)? * 60 + n(17..19)?)
}

/// Le rythme se mesure sur les 30 dernières minutes.
pub const RATE_WINDOW: i64 = 30 * 60;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Forecast {
    Idle,         // rien depuis 30 min : pas de prévision
    Safe,         // à ce rythme, la limite n'est pas atteinte avant la réinitialisation
    LimitAt(i64), // heure (Unix) où la limite serait atteinte
}

/// Le % du forfait ne se compte pas en tokens (Anthropic les pondère) : on étalonne le coût équivalent API sur le %
/// officiel de la fenêtre (x $ = y %), puis on prolonge le rythme des 30 dernières minutes.
pub fn forecast(pct: f32, window_cost: f64, recent_cost: f64, recent_secs: i64, now: i64, reset: i64) -> Forecast {
    if recent_cost <= 0.0 || recent_secs <= 0 || window_cost <= 0.0 || pct <= 0.0 {
        return Forecast::Idle;
    }
    if pct >= 1.0 {
        return Forecast::LimitAt(now);
    }
    let pct_per_sec = recent_cost / recent_secs as f64 * (pct as f64 / window_cost);
    let at = now + ((1.0 - pct as f64) / pct_per_sec).round() as i64;
    if at >= reset { Forecast::Safe } else { Forecast::LimitAt(at) }
}

/// « 21:40 », heure locale.
pub fn clock(t: i64, tz: i64) -> String {
    let m = (t + tz).div_euclid(60);
    format!("{:02}:{:02}", m.div_euclid(60).rem_euclid(24), m.rem_euclid(60))
}

/// Initiale du jour de la semaine (lundi = L / M), `days_ago` jours avant aujourd'hui.
pub fn weekday_letter(now: i64, tz: i64, days_ago: i64, fr: bool) -> &'static str {
    // le 1er janvier 1970 était un jeudi
    let wd = ((now + tz).div_euclid(86400) - days_ago + 3).rem_euclid(7) as usize; // 0 = lundi
    if fr { ["L", "M", "M", "J", "V", "S", "D"][wd] } else { ["M", "T", "W", "T", "F", "S", "S"][wd] }
}

/// « 12,40 $ », « 0,85 $ » (« $12.40 » en anglais).
pub fn fmt_cost(c: f64, fr: bool) -> String {
    if fr { format!("{c:.2} $").replace('.', ",") } else { format!("${c:.2}") }
}

/// « 41 M », « 1,2 M » (« 1.2 M »), « 840 k », « 512 ».
pub fn fmt_tokens(n: u64, fr: bool) -> String {
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

    const LINE: &str = r#"{"cwd":"C:\\Users\\x\\CODE\\DESKORB","message":{"model":"claude-opus-5-5","id":"msg_1","content":[{"type":"text","text":"\"input_tokens\":999"}],"usage":{"input_tokens":2,"cache_creation_input_tokens":300,"cache_read_input_tokens":4000,"output_tokens":50,"cache_creation":{"ephemeral_1h_input_tokens":300,"ephemeral_5m_input_tokens":0},"iterations":[{"input_tokens":7}]}},"type":"assistant","timestamp":"2026-10-25T16:32:55.326Z"}"#;

    #[test]
    fn parses_a_line() {
        let p = parse(LINE).unwrap();
        assert_eq!((p.id, p.model, p.input, p.output, p.cache_5m, p.cache_1h, p.cache_read), ("msg_1", "claude-opus-5-5", 2, 50, 0, 300, 4000));
        assert_eq!(p.t, 1_792_945_975);
        assert_eq!(project_name(p.cwd), "DESKORB");
        // un sous-dossier d'un dépôt git compte pour le dépôt
        let repo = std::env::temp_dir().join(format!("deskorb-repo-{}", std::process::id()));
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::create_dir_all(repo.join("assets").join("logo")).unwrap();
        let sub = repo.join("assets").join("logo").to_string_lossy().replace('\\', r"\\");
        assert_eq!(project_name(&sub), repo.file_name().unwrap().to_string_lossy());
        std::fs::remove_dir_all(&repo).unwrap();
        // dossier disparu, chemins Git Bash : on remonte au-dessus des sous-dossiers génériques
        assert_eq!(project_name(r"C:\\Nulle\\Part\\MUSIC\\assets\\concepts"), "MUSIC");
        assert_eq!(project_name("/c/Nulle/Part/SONAR/assets/logo"), "SONAR");
        assert_eq!(project_name(r"C:\\Nulle\\VSCODE\\ASSISTIVE\\docs\\logo"), "ASSISTIVE");
        assert_eq!(parse(&LINE.replace(r#""type":"assistant""#, r#""type":"user""#)), None);
        // sans détail par durée : l'écriture du cache compte en 5 min
        let line = LINE.replace(r#""cache_creation":{"ephemeral_1h_input_tokens":300,"ephemeral_5m_input_tokens":0},"#, "");
        let old = parse(&line).unwrap();
        assert_eq!((old.cache_5m, old.cache_1h), (300, 0));
    }

    #[test]
    fn prices_and_cost() {
        assert_eq!(PRICES[model_index("claude-opus-5-5") as usize].0, "claude-opus-5-5");
        assert_eq!(PRICES[model_index("claude-opus-5") as usize].0, "claude-opus-5");
        assert_eq!(PRICES[model_index("claude-fable-5-1") as usize].0, "claude-fable-5-1");
        assert_eq!(PRICES[model_index("claude-opus-4-8") as usize].0, "claude-opus-4");
        assert_eq!(model_index("<synthetic>"), UNKNOWN);
        // Opus 5.5 : 1 M entrée 4 $ + 1 M sortie 20 $ + 1 M cache 1 h 8 $ + 1 M lecture 0,20 $
        let e = Entry { t: 0, project: 0, model: model_index("claude-opus-5-5"), input: 1_000_000, output: 1_000_000,
            cache_5m: 0, cache_1h: 1_000_000, cache_read: 1_000_000 };
        assert!((e.cost() - 32.2).abs() < 1e-9);
        assert_eq!(Entry { model: UNKNOWN, ..e }.cost(), 0.0);
    }

    #[test]
    fn top_projects_and_dedup() {
        let dir = std::env::temp_dir().join(format!("deskorb-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("a.jsonl");
        let other = LINE.replace("msg_1", "msg_2").replace("DESKORB", "ASSISTIVE").replace(r#""output_tokens":50"#, r#""output_tokens":5000"#);
        // msg_1 en double (deux blocs de contenu), puis une ligne pas encore terminée
        std::fs::write(&f, format!("{LINE}\n{LINE}\n{other}\n{{\"type\":\"assist")).unwrap();
        let mut r = Reader::default();
        r.scan(&f, 0);
        assert_eq!(r.entries.len(), 2);
        let top = r.top_projects(0, 5);
        assert_eq!(top.iter().map(|p| p.0).collect::<Vec<_>>(), ["ASSISTIVE", "DESKORB"]);
        // la suite de la ligne arrive : seule elle est lue
        std::fs::write(&f, format!("{LINE}\n{LINE}\n{other}\n{}\n", LINE.replace("msg_1", "msg_3"))).unwrap();
        r.scan(&f, 0);
        assert_eq!(r.entries.len(), 3);
        std::fs::remove_dir_all(&dir).unwrap();

        // jours locaux : à UTC+1, 23 h 30 UTC appartient déjà au lendemain
        let now: i64 = 1_792_945_975; // dimanche 25/10/2026 16:32 UTC
        let tz: i64 = 3600;
        let at = |t: i64| Entry { t, project: 0, model: model_index("claude-opus-5-5"), input: 1_000_000, output: 0, cache_5m: 0, cache_1h: 0, cache_read: 0 };
        let midnight = (now + tz).div_euclid(86400) * 86400 - tz; // minuit local aujourd'hui, en UTC
        r.entries = vec![at(now), at(midnight - 1800), at(midnight - 6 * 86400), at(midnight - 7 * 86400)];
        let d = r.daily(now, tz);
        assert_eq!(d.map(|x| x.0), [1_000_000, 0, 0, 0, 0, 1_000_000, 1_000_000]);
        assert!((d[6].1 - 4.0).abs() < 1e-9);
        assert_eq!(weekday_letter(now, tz, 0, false), "S"); // dimanche
        assert_eq!(weekday_letter(now, tz, 1, true), "S"); // samedi
        assert_eq!(weekday_letter(now, tz, 6, true), "L"); // lundi
        assert_eq!(fmt_cost(12.4, true), "12,40 $");
    }

    #[test]
    fn forecast_from_pace() {
        let (now, reset) = (100_000, 100_000 + 3 * 3600);
        // 40 % pour 10 $ dans la fenêtre ; 2 $ ces 30 dernières min → 8 % / 30 min → 100 % dans 3 h 45 : après le reset
        assert_eq!(forecast(0.4, 10.0, 2.0, 1800, now, reset), Forecast::Safe);
        // même chose au double rythme : 16 % / 30 min → limite dans 1 h 52 min 30 s
        assert_eq!(forecast(0.4, 10.0, 4.0, 1800, now, reset), Forecast::LimitAt(now + 6750));
        assert_eq!(forecast(0.4, 10.0, 0.0, 1800, now, reset), Forecast::Idle);
        assert_eq!(forecast(1.0, 10.0, 1.0, 1800, now, reset), Forecast::LimitAt(now));
        assert_eq!(clock(1_792_945_975, 3600), "17:32");
        assert_eq!(fmt_cost(12.4, false), "$12.40");
    }
}
