//! Souris Pulsar : on trouve ses interfaces HID (souris + dongle) par les API Windows, sans pilote ni dépendance.
//! Batterie : protocole du driver Linux `hid-pulsar` (même famille que Kysona, ATK, VXE). Rapports de 17 octets
//! sur l'interface vendeur dont les rapports entrée/sortie font 17 octets (page FF02 sur le dongle 3710:5406) :
//! octet 0 = 0x08, octet 1 = commande, dernier = 0x55 − somme des autres. La réponse revient en rapport d'entrée.

use std::{mem::zeroed, ptr::null_mut};
use windows_sys::Win32::{
    Devices::{DeviceAndDriverInstallation::*, HumanInterfaceDevice::*},
    Foundation::*,
    Storage::FileSystem::*,
    System::{
        Threading::{CreateEventW, WaitForSingleObject},
        IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED},
    },
};

const LEN: usize = 17;
const REPORT_ID: u8 = 0x08;
const CMD_STATUS: u8 = 0x03; // réponse[6] : 1 = souris reliée au dongle
const CMD_POWER: u8 = 0x04; // réponse[6] = %, [7] = câble branché (charge), [8..10] = tension en mV

pub struct Hid {
    pub path: Vec<u16>, // terminé par 0, prêt pour CreateFileW
    pub vid: u16,
    pub pid: u16,
    pub product: String,
    pub usage_page: u16,
    pub usage: u16,
    pub input_len: u16,
    pub output_len: u16,
    pub feature_len: u16,
}

#[derive(Default)]
pub struct Mouse {
    pub battery: Option<u8>, // %
    pub charging: bool,
    pub wired: bool,
    pub asleep: bool, // reliée au dongle mais endormie : elle ne donne pas sa batterie
    pub dpi: Option<u32>,
    pub polling_hz: Option<u32>,
}

/// Toutes les interfaces HID présentes.
pub fn list() -> Vec<Hid> {
    let mut out = Vec::new();
    unsafe {
        let mut guid = zeroed();
        HidD_GetHidGuid(&mut guid);
        let set = SetupDiGetClassDevsW(&guid, null_mut(), null_mut(), DIGCF_PRESENT | DIGCF_DEVICEINTERFACE);
        if set == INVALID_HANDLE_VALUE as _ {
            return out;
        }
        let mut i = 0;
        loop {
            let mut itf: SP_DEVICE_INTERFACE_DATA = zeroed();
            itf.cbSize = size_of::<SP_DEVICE_INTERFACE_DATA>() as u32;
            if SetupDiEnumDeviceInterfaces(set, null_mut(), &guid, i, &mut itf) == 0 {
                break;
            }
            i += 1;
            let mut need = 0;
            SetupDiGetDeviceInterfaceDetailW(set, &itf, null_mut(), 0, &mut need, null_mut());
            let mut buf = vec![0u32; need as usize / 4 + 1]; // aligné, cbSize compris
            let detail = buf.as_mut_ptr() as *mut SP_DEVICE_INTERFACE_DETAIL_DATA_W;
            (*detail).cbSize = size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32;
            if SetupDiGetDeviceInterfaceDetailW(set, &itf, detail, need, null_mut(), null_mut()) == 0 {
                continue;
            }
            let p = (*detail).DevicePath.as_ptr();
            let len = (0..).take_while(|&k| *p.add(k) != 0).count();
            let path: Vec<u16> = std::slice::from_raw_parts(p, len).iter().copied().chain(Some(0)).collect();
            if let Some(h) = describe(path) {
                out.push(h);
            }
        }
        SetupDiDestroyDeviceInfoList(set);
    }
    out
}

/// Ouverture sans droits de lecture/écriture : suffit pour les attributs, et marche même sur la souris système.
unsafe fn describe(path: Vec<u16>) -> Option<Hid> {
    let f = CreateFileW(path.as_ptr(), 0, FILE_SHARE_READ | FILE_SHARE_WRITE, null_mut(), OPEN_EXISTING, 0, null_mut());
    if f == INVALID_HANDLE_VALUE {
        return None;
    }
    let mut attr: HIDD_ATTRIBUTES = zeroed();
    attr.Size = size_of::<HIDD_ATTRIBUTES>() as u32;
    HidD_GetAttributes(f, &mut attr);
    let mut name = [0u16; 128];
    HidD_GetProductString(f, name.as_mut_ptr() as _, 256);
    let mut caps: HIDP_CAPS = zeroed();
    let mut pre = 0;
    if HidD_GetPreparsedData(f, &mut pre) != 0 {
        HidP_GetCaps(pre, &mut caps);
        HidD_FreePreparsedData(pre);
    }
    CloseHandle(f);
    let end = name.iter().position(|&c| c == 0).unwrap_or(name.len());
    Some(Hid {
        path, vid: attr.VendorID, pid: attr.ProductID, product: String::from_utf16_lossy(&name[..end]),
        usage_page: caps.UsagePage, usage: caps.Usage,
        input_len: caps.InputReportByteLength, output_len: caps.OutputReportByteLength, feature_len: caps.FeatureReportByteLength,
    })
}

/// État de la souris, `None` si aucune Pulsar n'est branchée (ni filaire ni dongle).
// shortcut: DPI et polling restent vides, le driver Linux ne les lit pas ; à relever par capture USB si besoin
pub fn read() -> Option<Mouse> {
    let devs = list();
    let cmd = devs.iter().find(|d| {
        d.product.to_lowercase().contains("pulsar") && d.usage_page >= 0xFF00 && d.input_len as usize == LEN && d.output_len as usize == LEN
    })?;
    let mut m = Mouse { wired: !cmd.product.to_lowercase().contains("dongle"), ..Mouse::default() };
    // la souris s'endort vite quand on ne la bouge pas : le dongle répond seul, sans batterie
    if !m.wired && query(&cmd.path, CMD_STATUS).is_none_or(|r| r[6] != 1) {
        m.asleep = true;
        return Some(m);
    }
    if let Some(r) = query(&cmd.path, CMD_POWER).filter(|r| r[6] <= 100 && r[7] <= 1) {
        m.battery = Some(r[6]);
        m.charging = r[7] == 1;
    }
    Some(m)
}

fn packet(cmd: u8) -> [u8; LEN] {
    let mut p = [0; LEN];
    p[0] = REPORT_ID;
    p[1] = cmd;
    p[LEN - 1] = checksum(&p);
    p
}

fn checksum(p: &[u8; LEN]) -> u8 {
    0x55u8.wrapping_sub(p[..LEN - 1].iter().fold(0u8, |a, &b| a.wrapping_add(b)))
}

/// Envoie une commande (SET_REPORT, comme le driver) et attend sa réponse ; ignore les autres rapports
/// (événements spontanés 0x0a) et abandonne après ~300 ms.
fn query(path: &[u16], cmd: u8) -> Option<[u8; LEN]> {
    unsafe {
        let f = CreateFileW(path.as_ptr(), GENERIC_READ | GENERIC_WRITE, FILE_SHARE_READ | FILE_SHARE_WRITE, null_mut(),
            OPEN_EXISTING, FILE_FLAG_OVERLAPPED, null_mut());
        if f == INVALID_HANDLE_VALUE {
            return None;
        }
        let ev = CreateEventW(null_mut(), 1, 0, null_mut());
        let mut req = packet(cmd);
        let mut out = None;
        if HidD_SetOutputReport(f, req.as_mut_ptr() as _, LEN as u32) != 0 {
            let start = std::time::Instant::now();
            while out.is_none() && start.elapsed().as_millis() < 300 {
                let mut buf = [0u8; LEN];
                let mut ov: OVERLAPPED = zeroed();
                ov.hEvent = ev;
                let mut got = 0;
                ReadFile(f, buf.as_mut_ptr(), LEN as u32, null_mut(), &mut ov);
                if WaitForSingleObject(ev, 300) != WAIT_OBJECT_0 || GetOverlappedResult(f, &ov, &mut got, 0) == 0 {
                    CancelIoEx(f, &ov);
                    GetOverlappedResult(f, &ov, &mut got, 1); // attendre l'annulation avant de libérer `buf`
                    break;
                }
                if got as usize == LEN && buf[0] == REPORT_ID && buf[1] == cmd && buf[LEN - 1] == checksum(&buf) {
                    out = Some(buf);
                }
            }
        }
        CloseHandle(ev);
        CloseHandle(f);
        out
    }
}

/// `--probe` : liste les interfaces HID pour repérer celles de la Pulsar et la taille de leurs rapports.
pub fn probe() {
    for d in list() {
        println!("{:04X}:{:04X} page {:04X} usage {:04X} in {:>3} out {:>3} feat {:>3}  {}  {}",
            d.vid, d.pid, d.usage_page, d.usage, d.input_len, d.output_len, d.feature_len, d.product,
            String::from_utf16_lossy(&d.path[..d.path.len() - 1]));
    }
    match read() {
        Some(m) => println!("
Souris : batterie {:?} %, en charge {}, filaire {}, en veille {}", m.battery, m.charging, m.wired, m.asleep),
        None => println!("
Aucune Pulsar trouvée"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packets_match_the_driver() {
        // requête batterie connue : 08 04 00… 49
        let p = packet(CMD_POWER);
        assert_eq!((p[0], p[1], p[LEN - 1]), (0x08, 0x04, 0x49));
        assert!(p[2..LEN - 1].iter().all(|&b| b == 0));
        assert_eq!(packet(CMD_STATUS)[LEN - 1], 0x4A);
        let mut r = [0u8; LEN];
        r[..10].copy_from_slice(&[0x08, 0x04, 0, 0, 0, 0, 87, 1, 0x0F, 0xA0]);
        r[LEN - 1] = checksum(&r);
        assert_eq!(r[LEN - 1], 0x42); // 0x55 − (275 mod 256)
    }
}
