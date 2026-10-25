//! Souris Pulsar : on trouve ses interfaces HID (souris + dongle) par les API Windows, sans pilote ni dépendance.
//! Le protocole propriétaire (batterie, DPI, polling) passe par l'interface « vendeur » (usage page ≥ 0xFF00)
//! en rapports Feature/Output ; il reste à le relever (`cargo run -- --probe`, puis capture USB de l'app Pulsar).

use std::{mem::zeroed, ptr::null_mut};
use windows_sys::Win32::{
    Devices::{DeviceAndDriverInstallation::*, HumanInterfaceDevice::*},
    Foundation::*,
    Storage::FileSystem::*,
};

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
pub fn read() -> Option<Mouse> {
    let devs = list();
    let _vendor = devs.iter().find(|d| d.product.to_lowercase().contains("pulsar") && d.usage_page >= 0xFF00)?;
    // shortcut: connexion détectée mais batterie/DPI/polling vides tant que le protocole n'est pas relevé ;
    // brancher ici HidD_SetFeature/HidD_GetFeature (ou WriteFile/ReadFile) sur `_vendor.path`
    Some(Mouse::default())
}

/// `--probe` : liste les interfaces HID pour repérer celles de la Pulsar et la taille de leurs rapports.
pub fn probe() {
    for d in list() {
        println!("{:04X}:{:04X} page {:04X} usage {:04X} in {:>3} out {:>3} feat {:>3}  {}  {}",
            d.vid, d.pid, d.usage_page, d.usage, d.input_len, d.output_len, d.feature_len, d.product,
            String::from_utf16_lossy(&d.path[..d.path.len() - 1]));
    }
}
