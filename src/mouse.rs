//! Souris Pulsar : on trouve ses interfaces HID (souris + dongle) par les API Windows, sans pilote ni dépendance.
//! Batterie : protocole du driver Linux `hid-pulsar` (même famille que Kysona, ATK, VXE). Rapports de 17 octets
//! sur l'interface vendeur dont les rapports entrée/sortie font 17 octets (page FF02 sur le dongle 3710:5406) :
//! octet 0 = 0x08, octet 1 = commande, dernier = 0x55 − somme des autres. La réponse revient en rapport d'entrée.
//! Logitech (dongle Unifying / Bolt / Lightspeed ou câble) : HID++ 2.0, fonctions 0x1004 (Unified Battery) ou 0x1000.
//! Bluetooth, toutes marques : le pourcentage que Windows affiche dans Paramètres (propriété du périphérique).

use std::{mem::zeroed, ptr::null_mut};
use windows_sys::Win32::{
    Devices::{DeviceAndDriverInstallation::*, HumanInterfaceDevice::*, Properties::*},
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
    pub name: String,
    pub battery: Option<u8>, // %
    pub charging: bool,
    pub wired: bool,
    pub asleep: bool, // reliée au dongle mais endormie : elle ne donne pas sa batterie
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

/// État de la souris : Pulsar, puis Logitech, puis Bluetooth ; `None` si aucune n'est trouvée.
// shortcut: la première trouvée gagne ; un choix dans les réglages si quelqu'un a deux souris sans fil
pub fn read() -> Option<Mouse> {
    let devs = list();
    pulsar(&devs).or_else(|| logitech(&devs)).or_else(bluetooth)
}

// shortcut: ni DPI ni polling, le driver Linux ne les lit pas ; à relever par capture USB si besoin
fn pulsar(devs: &[Hid]) -> Option<Mouse> {
    let cmd = devs.iter().find(|d| {
        d.product.to_lowercase().contains("pulsar") && d.usage_page >= 0xFF00 && d.input_len as usize == LEN && d.output_len as usize == LEN
    })?;
    let mut m = Mouse { name: "Pulsar TenZ".into(), wired: !cmd.product.to_lowercase().contains("dongle"), ..Mouse::default() };
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

/// Pulsar : envoie une commande (SET_REPORT, comme le driver) et attend sa réponse, en ignorant les
/// événements spontanés (0x0a).
fn query(path: &[u16], cmd: u8) -> Option<[u8; LEN]> {
    let r = exchange(path, &mut packet(cmd), true, 300, |b| {
        (b.len() == LEN && b[0] == REPORT_ID && b[1] == cmd && b[LEN - 1] == checksum(b.try_into().unwrap())).then_some(true)
    })?;
    r.try_into().ok()
}

/// Envoie `req` (SET_REPORT ou écriture) puis lit les rapports d'entrée jusqu'à ce que `accept` dise
/// oui (`Some(true)`) ou erreur (`Some(false)`), au plus `ms` millisecondes.
fn exchange(path: &[u16], req: &mut [u8], set_report: bool, ms: u32, accept: impl Fn(&[u8]) -> Option<bool>) -> Option<Vec<u8>> {
    unsafe {
        let f = CreateFileW(path.as_ptr(), GENERIC_READ | GENERIC_WRITE, FILE_SHARE_READ | FILE_SHARE_WRITE, null_mut(),
            OPEN_EXISTING, FILE_FLAG_OVERLAPPED, null_mut());
        if f == INVALID_HANDLE_VALUE {
            return None;
        }
        let ev = CreateEventW(null_mut(), 1, 0, null_mut());
        // attend une E/S ; si elle traîne, l'annule et attend l'annulation avant de libérer le tampon
        let wait = |ov: &OVERLAPPED, got: &mut u32| {
            if WaitForSingleObject(ev, ms) != WAIT_OBJECT_0 || GetOverlappedResult(f, ov, got, 0) == 0 {
                CancelIoEx(f, ov);
                GetOverlappedResult(f, ov, got, 1);
                return false;
            }
            true
        };
        let sent = if set_report {
            HidD_SetOutputReport(f, req.as_mut_ptr() as _, req.len() as u32) != 0
        } else {
            let mut ov: OVERLAPPED = zeroed();
            ov.hEvent = ev;
            WriteFile(f, req.as_ptr(), req.len() as u32, null_mut(), &mut ov);
            wait(&ov, &mut 0)
        };
        let mut out = None;
        let start = std::time::Instant::now();
        while sent && start.elapsed().as_millis() < ms as u128 {
            let mut buf = [0u8; 64];
            let mut ov: OVERLAPPED = zeroed();
            ov.hEvent = ev;
            let mut got = 0;
            ReadFile(f, buf.as_mut_ptr(), buf.len() as u32, null_mut(), &mut ov);
            if !wait(&ov, &mut got) {
                break;
            }
            match accept(&buf[..got as usize]) {
                Some(true) => {
                    out = Some(buf[..got as usize].to_vec());
                    break;
                }
                Some(false) => break,
                None => {}
            }
        }
        CloseHandle(ev);
        CloseHandle(f);
        out
    }
}

// ---------- Logitech HID++ 2.0 ----------

const HIDPP_LONG: u8 = 0x11; // rapport de 20 octets, sur la collection vendeur FF00:0002
const SW: u8 = 0x0A; // identifiant logiciel : distingue nos réponses des notifications (0)

/// Requête HID++ longue : appareil `dev`, fonctionnalité d'index `feat`, fonction `func`.
fn hidpp_req(dev: u8, feat: u8, func: u8, params: &[u8]) -> [u8; 20] {
    let mut req = [0u8; 20];
    req[..4].copy_from_slice(&[HIDPP_LONG, dev, feat, func << 4 | SW]);
    req[4..4 + params.len()].copy_from_slice(params);
    req
}

/// Réponse à `req` : `Some(true)` la bonne, `Some(false)` une erreur HID++ 2.0, `None` autre chose.
fn hidpp_match(req: &[u8; 20], b: &[u8]) -> Option<bool> {
    if b.len() < 20 || b[0] != HIDPP_LONG || b[1] != req[1] {
        return None;
    }
    if b[2] == 0xFF && b[3] == req[2] && b[4] == req[3] {
        return Some(false);
    }
    (b[2] == req[2] && b[3] == req[3]).then_some(true)
}

/// Appelle une fonction HID++ et renvoie les 16 octets de paramètres de la réponse.
/// Un appareil absent ou endormi ne répond pas : délai court.
fn hidpp(path: &[u16], dev: u8, feat: u8, func: u8, params: &[u8]) -> Option<[u8; 16]> {
    let mut req = hidpp_req(dev, feat, func, params);
    let want = req;
    let r = exchange(path, &mut req, false, 80, |b| hidpp_match(&want, b))?;
    r[4..20].try_into().ok()
}

/// Index d'une fonctionnalité (fonction 0 de la racine), `None` si l'appareil ne l'a pas.
fn feature(path: &[u16], dev: u8, id: u16) -> Option<u8> {
    hidpp(path, dev, 0, 0, &id.to_be_bytes()).map(|p| p[0]).filter(|&i| i != 0)
}

fn logitech(devs: &[Hid]) -> Option<Mouse> {
    // 0xFF : appareil branché en direct (câble) ; 1..=6 : appareils appairés au dongle
    // shortcut: chaque index vide attend 80 ms ; mémoriser l'index trouvé si le rafraîchissement se sent
    devs.iter()
        .filter(|d| d.vid == 0x046D && d.usage_page == 0xFF00 && d.usage == 0x0002 && d.output_len == 20)
        .find_map(|d| [0xFF, 1, 2, 3, 4, 5, 6].into_iter().find_map(|dev| logitech_device(&d.path, dev)))
}

fn logitech_device(path: &[u16], dev: u8) -> Option<Mouse> {
    let root = hidpp(path, dev, 0, 0, &0x0005u16.to_be_bytes())?; // personne à cet index : on s'arrête là
    // nom et type (0x0005) : un dongle peut aussi porter un clavier
    let mut name = String::from("Logitech");
    if root[0] != 0 {
        let f = root[0];
        if hidpp(path, dev, f, 2, &[]).is_some_and(|p| !(3..=5).contains(&p[0])) {
            return None; // ni souris (3), ni pavé tactile (4), ni trackball (5)
        }
        let len = hidpp(path, dev, f, 0, &[]).map_or(0, |p| p[0] as usize);
        let mut bytes = Vec::new();
        while bytes.len() < len {
            let Some(p) = hidpp(path, dev, f, 1, &[bytes.len() as u8]) else { break };
            bytes.extend_from_slice(&p[..(len - bytes.len()).min(16)]);
        }
        if !bytes.is_empty() {
            name = String::from_utf8_lossy(&bytes).trim().into();
        }
    }
    let mut m = Mouse { name, wired: dev == 0xFF, ..Mouse::default() };
    if let Some(f) = feature(path, dev, 0x1004) {
        // Unified Battery : le pourcentage n'existe que si l'appareil l'annonce (sinon 4 paliers, trop vague)
        let precise = hidpp(path, dev, f, 0, &[]).is_some_and(|c| c[1] & 0x02 != 0);
        let st = hidpp(path, dev, f, 1, &[])?;
        m.battery = Some(st[0]).filter(|&b| precise && b <= 100);
        m.charging = matches!(st[2], 1 | 2);
    } else if let Some(f) = feature(path, dev, 0x1000) {
        let st = hidpp(path, dev, f, 0, &[])?;
        m.battery = Some(st[0]).filter(|&b| (1..=100).contains(&b)); // 0 = inconnu
        m.charging = matches!(st[2], 1 | 2 | 4);
    } else {
        // shortcut: 0x1001 (tension seule) ignorée, le % dépendrait d'une courbe approximative
        return None;
    }
    Some(m)
}

// ---------- Bluetooth ----------

/// Pourcentage de batterie Bluetooth (celui des Paramètres Windows), sur le nœud BTHLE / BTHENUM.
const BT_BATTERY: DEVPROPKEY = DEVPROPKEY { fmtid: windows_sys::core::GUID::from_u128(0x104ea319_6ee2_4701_bd47_8ddbf425bbe5), pid: 2 };

/// Souris Bluetooth : un nœud Bluetooth avec batterie, dans le même conteneur (le même appareil) qu'une souris.
fn bluetooth() -> Option<Mouse> {
    unsafe {
        let mut mice = Vec::new();
        each_dev(Some(&GUID_DEVCLASS_MOUSE), None, |set, d| mice.extend(prop(set, d, &DEVPKEY_Device_ContainerId)));
        let mut out = None;
        for en in ["BTHLE", "BTHENUM"] {
            each_dev(None, Some(en), |set, d| {
                let bat = prop(set, d, &BT_BATTERY).and_then(|b| b.first().copied()).filter(|&b| b <= 100);
                if out.is_none() && bat.is_some() && prop(set, d, &DEVPKEY_Device_ContainerId).is_some_and(|c| mice.contains(&c)) {
                    let name = prop(set, d, &DEVPKEY_Device_FriendlyName).map_or("Bluetooth".into(), |b| {
                        let w: Vec<u16> = b.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).take_while(|&c| c != 0).collect();
                        String::from_utf16_lossy(&w)
                    });
                    out = Some(Mouse { name, battery: bat, ..Mouse::default() });
                }
            });
        }
        out
    }
}

/// Parcourt les périphériques présents d'une classe, ou de toutes les classes d'un énumérateur.
unsafe fn each_dev(class: Option<&windows_sys::core::GUID>, enumerator: Option<&str>, mut f: impl FnMut(HDEVINFO, &SP_DEVINFO_DATA)) {
    let en = enumerator.map(crate::win::w);
    let flags = DIGCF_PRESENT | if class.is_none() { DIGCF_ALLCLASSES } else { 0 };
    let set = SetupDiGetClassDevsW(class.map_or(std::ptr::null(), |g| g), en.as_ref().map_or(std::ptr::null(), |e| e.as_ptr()), null_mut(), flags);
    if set == INVALID_HANDLE_VALUE as _ {
        return;
    }
    let mut d: SP_DEVINFO_DATA = zeroed();
    d.cbSize = size_of::<SP_DEVINFO_DATA>() as u32;
    let mut i = 0;
    while SetupDiEnumDeviceInfo(set, i, &mut d) != 0 {
        f(set, &d);
        i += 1;
    }
    SetupDiDestroyDeviceInfoList(set);
}

unsafe fn prop(set: HDEVINFO, d: &SP_DEVINFO_DATA, key: &DEVPROPKEY) -> Option<Vec<u8>> {
    let (mut ty, mut need) = (0, 0);
    SetupDiGetDevicePropertyW(set, d, key, &mut ty, null_mut(), 0, &mut need, 0);
    if need == 0 {
        return None;
    }
    let mut buf = vec![0u8; need as usize];
    (SetupDiGetDevicePropertyW(set, d, key, &mut ty, buf.as_mut_ptr(), need, &mut need, 0) != 0).then_some(buf)
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
{} : batterie {:?} %, en charge {}, filaire {}, en veille {}", m.name, m.battery, m.charging, m.wired, m.asleep),
        None => println!("
Aucune souris trouvée"),
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

    #[test]
    fn hidpp_answers_are_matched() {
        // getFeature(0x1004) sur l'appareil 1 : 11 01 00 0A 10 04 …
        let req = hidpp_req(1, 0, 0, &0x1004u16.to_be_bytes());
        assert_eq!(req[..6], [0x11, 0x01, 0x00, 0x0A, 0x10, 0x04]);
        let mut ok = req;
        ok[4] = 7; // index de la fonctionnalité
        assert_eq!(hidpp_match(&req, &ok), Some(true));
        let mut err = [0u8; 20];
        err[..5].copy_from_slice(&[0x11, 0x01, 0xFF, 0x00, 0x0A]);
        assert_eq!(hidpp_match(&req, &err), Some(false));
        let mut note = req;
        note[3] = 0x00; // notification de l'appareil (identifiant logiciel 0)
        assert_eq!(hidpp_match(&req, &note), None);
        assert_eq!(hidpp_match(&req, &[0x11, 0x02]), None);
    }
}
