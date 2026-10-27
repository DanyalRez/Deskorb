//! Température exacte du processeur (Intel), lue dans ses registres internes (MSR) par le pilote PawnIO,
//! comme HWiNFO ou Core Temp : température = TjMax − écart lu dans le registre thermique du package.
//! PawnIOLib.dll vient de l'installateur PawnIO ; le module IntelMSR (LGPL) est intégré à l'exe.

use crate::win::w;
use std::ffi::c_void;
use windows_sys::Win32::{
    Foundation::{FreeLibrary, HMODULE},
    System::LibraryLoader::{GetProcAddress, LoadLibraryW},
};

const MODULE: &[u8] = include_bytes!("../assets/pawnio/IntelMSR.bin");
const MSR_TEMPERATURE_TARGET: u64 = 0x1A2; // bits 23:16 = TjMax
const MSR_PACKAGE_THERM_STATUS: u64 = 0x1B1; // bits 22:16 = écart sous TjMax

type Handle = *mut c_void;

/// Pourquoi il n'y a pas de température.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Missing {
    NotIntel,  // seul le module Intel est intégré
    NoPawnIO,  // pilote non installé
    Off,       // pilote présent, mais le service capteurs (administrateur) ne tourne pas : un clic l'active
}

fn dll_path() -> String {
    let dir = std::env::var("ProgramFiles").unwrap_or_else(|_| r"C:\Program Files".into());
    format!(r"{dir}\PawnIO\PawnIOLib.dll")
}

/// Ce qui manque pour lire la température, côté interface (le service capteurs fait la lecture).
pub fn missing() -> Missing {
    if !is_intel() {
        Missing::NotIntel
    } else if !std::path::Path::new(&dll_path()).exists() {
        Missing::NoPawnIO
    } else {
        Missing::Off
    }
}

pub struct CpuTemp {
    lib: HMODULE,
    handle: Handle,
    exec: unsafe extern "system" fn(Handle, *const u8, *const u64, usize, *mut u64, usize, *mut usize) -> i32,
    close: unsafe extern "system" fn(Handle) -> i32,
}

/// TjMax − écart, à partir des deux registres bruts.
pub fn temp_from(target: u64, status: u64) -> f32 {
    ((target >> 16) & 0xFF) as f32 - ((status >> 16) & 0x7F) as f32
}

fn is_intel() -> bool {
    let r = std::arch::x86_64::__cpuid(0);
    let v: Vec<u8> = [r.ebx, r.edx, r.ecx].iter().flat_map(|x| x.to_le_bytes()).collect();
    v == b"GenuineIntel"
}

impl CpuTemp {
    pub fn open() -> Result<CpuTemp, Missing> {
        if !is_intel() {
            return Err(Missing::NotIntel);
        }
        unsafe {
            let lib = LoadLibraryW(w(&dll_path()).as_ptr());
            if lib.is_null() {
                return Err(Missing::NoPawnIO);
            }
            let f = |name: &str| GetProcAddress(lib, format!("{name}\0").as_ptr());
            let ready = (|| {
                let open: unsafe extern "system" fn(*mut Handle) -> i32 = std::mem::transmute(f("pawnio_open")?);
                let load: unsafe extern "system" fn(Handle, *const u8, usize) -> i32 = std::mem::transmute(f("pawnio_load")?);
                let exec = std::mem::transmute(f("pawnio_execute")?);
                let close: unsafe extern "system" fn(Handle) -> i32 = std::mem::transmute(f("pawnio_close")?);
                let mut handle = std::ptr::null_mut();
                if open(&mut handle) != 0 {
                    return None;
                }
                if load(handle, MODULE.as_ptr(), MODULE.len()) != 0 {
                    close(handle);
                    return None;
                }
                Some(CpuTemp { lib, handle, exec, close })
            })();
            ready.ok_or_else(|| {
                FreeLibrary(lib);
                Missing::Off
            })
        }
    }

    fn msr(&self, reg: u64) -> Option<u64> {
        let (input, mut out, mut got) = ([reg], [0u64], 0usize);
        let ok = unsafe { (self.exec)(self.handle, c"ioctl_read_msr".as_ptr().cast(), input.as_ptr(), 1, out.as_mut_ptr(), 1, &mut got) };
        (ok == 0 && got == 1).then_some(out[0])
    }

    /// Température du package (°C).
    pub fn read(&self) -> Option<f32> {
        Some(temp_from(self.msr(MSR_TEMPERATURE_TARGET)?, self.msr(MSR_PACKAGE_THERM_STATUS)?))
    }
}

impl Drop for CpuTemp {
    fn drop(&mut self) {
        unsafe {
            (self.close)(self.handle);
            FreeLibrary(self.lib);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_temperature() {
        // TjMax 100 °C, écart 38 °C → 62 °C ; les autres bits des registres sont ignorés
        assert_eq!(temp_from(0x0064_0000, 0x8826_0000), 62.0);
        assert_eq!(temp_from(0xFF5F_1234, 0x0001_0000), 94.0);
    }
}
