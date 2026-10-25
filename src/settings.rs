//! Réglages : fichier texte `clé=valeur` dans %APPDATA%\Deskorb, langue, lancement avec Windows.

use crate::win::w;
use std::path::PathBuf;
use windows_sys::Win32::{
    Globalization::GetUserDefaultUILanguage,
    System::Registry::{RegDeleteKeyValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ},
};

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
pub const REFRESH_SECS: [u32; 3] = [5, 15, 60];

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Config {
    pub theme: usize,   // 0 Liquid Glass, 1 sombre, 2 clair
    pub icon: usize,    // 0 Orbite, 1 Jauges, 2 pourcentage
    pub compact: bool,  // panneau sans les lignes de détail
    pub refresh: usize, // index dans REFRESH_SECS
    pub french: bool,
    pub alert: bool, // notification sous 20 % de batterie
    pub startup: bool,
}

impl Default for Config {
    fn default() -> Self {
        // français si Windows l'est (langue principale 0x0C), anglais sinon
        let french = unsafe { GetUserDefaultUILanguage() } & 0x3FF == 0x0C;
        Config { theme: 0, icon: 0, compact: false, refresh: 1, french, alert: true, startup: false }
    }
}

pub fn parse(s: &str) -> Config {
    let mut c = Config::default();
    for line in s.lines() {
        let Some((k, v)) = line.split_once('=') else { continue };
        let (k, v) = (k.trim(), v.trim());
        let n = v.parse::<usize>().ok();
        match k {
            "theme" => c.theme = n.unwrap_or(0).min(2),
            "icon" => c.icon = n.unwrap_or(0).min(2),
            "panel" => c.compact = v == "compact",
            "refresh" => c.refresh = REFRESH_SECS.iter().position(|&s| Some(s as usize) == n).unwrap_or(1),
            "language" => c.french = v == "fr",
            "low_battery_alert" => c.alert = v != "0",
            "startup" => c.startup = v != "0",
            _ => {}
        }
    }
    c
}

fn file() -> Option<PathBuf> {
    Some(PathBuf::from(std::env::var_os("APPDATA")?).join("Deskorb").join("settings.txt"))
}

pub fn load() -> Config {
    file().and_then(|f| std::fs::read_to_string(f).ok()).map_or_else(Config::default, |s| parse(&s))
}

pub fn save(c: &Config) {
    let Some(f) = file() else { return };
    let s = format!("theme={}\nicon={}\npanel={}\nrefresh={}\nlanguage={}\nlow_battery_alert={}\nstartup={}\n",
        c.theme, c.icon, if c.compact { "compact" } else { "detailed" }, REFRESH_SECS[c.refresh],
        if c.french { "fr" } else { "en" }, c.alert as u8, c.startup as u8);
    let _ = std::fs::create_dir_all(f.parent().unwrap()).and_then(|_| std::fs::write(f, s));
}

/// Lancement à l'ouverture de session ; réécrit à chaque démarrage pour suivre l'exe s'il a bougé.
pub unsafe fn set_startup(on: bool) {
    if !on {
        RegDeleteKeyValueW(HKEY_CURRENT_USER, w(RUN_KEY).as_ptr(), w("Deskorb").as_ptr());
        return;
    }
    let Ok(exe) = std::env::current_exe() else { return };
    let cmd = w(&format!("\"{}\"", exe.display()));
    RegSetKeyValueW(HKEY_CURRENT_USER, w(RUN_KEY).as_ptr(), w("Deskorb").as_ptr(), REG_SZ, cmd.as_ptr() as _, (cmd.len() * 2) as u32);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_round_trip() {
        let c = Config { theme: 2, icon: 1, compact: true, refresh: 2, french: true, alert: false, startup: true };
        // une ancienne clé (token_limit) est simplement ignorée
        let s = "theme=2\nicon=1\npanel=compact\ntoken_limit=250\nrefresh=60\nlanguage=fr\nlow_battery_alert=0\nstartup=1\n";
        assert_eq!(parse(s), c);
        // valeurs hors bornes ramenées dans les clous
        let bad = parse("theme=9\nicon=7\nrefresh=42\n");
        assert_eq!((bad.theme, bad.icon, bad.refresh), (2, 2, 1));
    }
}
