//! Mesures système du widget : CPU, RAM, disque C:. API natives de Windows, échantillonnées seulement
//! quand la goutte est ouverte (une fois par seconde).

use std::mem::zeroed;
use windows_sys::Win32::{
    Foundation::FILETIME,
    Storage::FileSystem::GetDiskFreeSpaceExW,
    System::{
        SystemInformation::{GetSystemInfo, GlobalMemoryStatusEx, MEMORYSTATUSEX, SYSTEM_INFO},
        Threading::GetSystemTimes,
    },
};

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Stats {
    pub cpu: f32, // 0 à 1, toutes les unités confondues
    pub threads: u32,
    pub ram_used: u64, // octets
    pub ram_total: u64,
    pub disk_used: u64,
    pub disk_total: u64,
}

#[derive(Default)]
pub struct Sys {
    prev: Option<(u64, u64)>, // (inactif, total) du relevé précédent : le CPU se mesure entre deux relevés
}

fn ft(t: FILETIME) -> u64 {
    (t.dwHighDateTime as u64) << 32 | t.dwLowDateTime as u64
}

/// Part d'occupation entre deux relevés (inactif, total) de GetSystemTimes.
pub fn busy(prev: (u64, u64), now: (u64, u64)) -> f32 {
    let (idle, total) = (now.0.saturating_sub(prev.0), now.1.saturating_sub(prev.1));
    if total == 0 { 0.0 } else { (1.0 - idle as f32 / total as f32).clamp(0.0, 1.0) }
}

impl Sys {
    /// Oublie le relevé précédent : à la réouverture, le premier CPU mesuré couvre la dernière seconde, pas la pause.
    pub fn reset(&mut self) {
        self.prev = None;
    }

    pub fn sample(&mut self) -> Stats {
        let mut s = Stats::default();
        unsafe {
            let (mut idle, mut kernel, mut user) = (zeroed(), zeroed(), zeroed());
            if GetSystemTimes(&mut idle, &mut kernel, &mut user) != 0 {
                // le temps noyau inclut le temps inactif
                let now = (ft(idle), ft(kernel) + ft(user));
                s.cpu = self.prev.map_or(0.0, |p| busy(p, now));
                self.prev = Some(now);
            }
            let mut info: SYSTEM_INFO = zeroed();
            GetSystemInfo(&mut info);
            s.threads = info.dwNumberOfProcessors;
            let mut mem: MEMORYSTATUSEX = zeroed();
            mem.dwLength = size_of::<MEMORYSTATUSEX>() as u32;
            if GlobalMemoryStatusEx(&mut mem) != 0 {
                (s.ram_total, s.ram_used) = (mem.ullTotalPhys, mem.ullTotalPhys - mem.ullAvailPhys);
            }
            let (mut free, mut total) = (0u64, 0u64);
            let c: Vec<u16> = "C:\\".encode_utf16().chain(Some(0)).collect();
            if GetDiskFreeSpaceExW(c.as_ptr(), std::ptr::null_mut(), &mut total, &mut free) != 0 {
                (s.disk_total, s.disk_used) = (total, total - free);
            }
        }
        s
    }
}

/// « 9,8 / 16 Go » (« 9.8 / 16 GB »).
pub fn fmt_gb(used: u64, total: u64, fr: bool) -> String {
    let g = |b: u64| b as f64 / (1u64 << 30) as f64;
    let n = |v: f64| if v >= 100.0 { format!("{v:.0}") } else { format!("{v:.1}").replace('.', if fr { "," } else { "." }) };
    format!("{} / {:.0} {}", n(g(used)), g(total), if fr { "Go" } else { "GB" })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_and_format() {
        assert_eq!(busy((100, 1000), (150, 1100)), 0.5); // 50 inactifs sur 100
        assert_eq!(busy((100, 1000), (100, 1000)), 0.0); // aucun temps écoulé
        assert_eq!(fmt_gb(10_522_669_875, 17_179_869_184, true), "9,8 / 16 Go");
        assert_eq!(fmt_gb(343_597_383_680, 536_870_912_000, false), "320 / 500 GB");
        let s = Sys::default().sample();
        assert!(s.ram_total > 0 && s.ram_used <= s.ram_total && s.threads > 0);
    }
}
