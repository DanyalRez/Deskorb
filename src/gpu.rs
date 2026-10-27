//! Carte graphique NVIDIA : charge, mémoire vidéo et température, lues par NVML (`nvml.dll`, livrée avec le
//! pilote NVIDIA, sans droits admin). Chargée seulement quand la goutte s'ouvre, libérée quand elle se ferme.

use crate::win::w;
use std::ffi::c_void;
use windows_sys::Win32::{
    Foundation::{FreeLibrary, HMODULE},
    System::LibraryLoader::{GetProcAddress, LoadLibraryW},
};

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Stats {
    pub load: f32, // 0 à 1
    pub vram_used: u64,
    pub vram_total: u64,
    pub temp: f32, // °C
}

#[repr(C)]
#[derive(Default)]
struct Utilization {
    gpu: u32,
    memory: u32,
}

#[repr(C)]
#[derive(Default)]
struct Memory {
    total: u64,
    free: u64,
    used: u64,
}

type Dev = *mut c_void;

pub struct Gpu {
    lib: HMODULE,
    dev: Dev,
    util: unsafe extern "C" fn(Dev, *mut Utilization) -> i32,
    mem: unsafe extern "C" fn(Dev, *mut Memory) -> i32,
    temp: unsafe extern "C" fn(Dev, u32, *mut u32) -> i32,
    shutdown: unsafe extern "C" fn() -> i32,
}

impl Gpu {
    /// Première carte NVIDIA, ou `None` (pas de pilote NVIDIA, ou pas de carte).
    pub fn open() -> Option<Gpu> {
        unsafe {
            let lib = LoadLibraryW(w("nvml.dll").as_ptr());
            if lib.is_null() {
                return None;
            }
            let f = |name: &str| GetProcAddress(lib, format!("{name}\0").as_ptr());
            let gpu = (|| {
                let init: unsafe extern "C" fn() -> i32 = std::mem::transmute(f("nvmlInit_v2")?);
                let by_index: unsafe extern "C" fn(u32, *mut Dev) -> i32 = std::mem::transmute(f("nvmlDeviceGetHandleByIndex_v2")?);
                let shutdown: unsafe extern "C" fn() -> i32 = std::mem::transmute(f("nvmlShutdown")?);
                let (util, mem, temp) = (f("nvmlDeviceGetUtilizationRates")?, f("nvmlDeviceGetMemoryInfo")?, f("nvmlDeviceGetTemperature")?);
                if init() != 0 {
                    return None;
                }
                let mut dev = std::ptr::null_mut();
                if by_index(0, &mut dev) != 0 {
                    shutdown();
                    return None;
                }
                // construit en une fois : un Gpu temporaire, en disparaissant, fermerait NVML (Drop)
                Some(Gpu { lib, dev, util: std::mem::transmute(util), mem: std::mem::transmute(mem), temp: std::mem::transmute(temp), shutdown })
            })();
            if gpu.is_none() {
                FreeLibrary(lib);
            }
            gpu
        }
    }

    pub fn sample(&self) -> Stats {
        let (mut u, mut m, mut t) = (Utilization::default(), Memory::default(), 0u32);
        unsafe {
            (self.util)(self.dev, &mut u);
            (self.mem)(self.dev, &mut m);
            (self.temp)(self.dev, 0, &mut t); // 0 = capteur du GPU
        }
        Stats { load: u.gpu.min(100) as f32 / 100.0, vram_used: m.used, vram_total: m.total, temp: t as f32 }
    }
}

impl Drop for Gpu {
    fn drop(&mut self) {
        unsafe {
            (self.shutdown)();
            FreeLibrary(self.lib);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sur une machine NVIDIA : valeurs plausibles. Ailleurs, `open` doit simplement renvoyer `None`.
    #[test]
    fn reads_nvidia_or_nothing() {
        if let Some(g) = Gpu::open() {
            let s = g.sample();
            assert!(s.vram_total > 0 && s.vram_used <= s.vram_total);
            assert!((0.0..=1.0).contains(&s.load) && (1.0..120.0).contains(&s.temp));
        }
    }
}
