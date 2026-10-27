//! Mini-service capteurs : `deskorb.exe --sensors`, lancé en administrateur par une tâche planifiée à
//! l'ouverture de session. Sans fenêtre ni réseau, il ne fait que lire la température CPU (PawnIO, qui exige
//! les droits admin) une fois par seconde et la déposer en mémoire partagée, où le Deskorb normal la lit.
//! Ainsi l'interface (et sa connexion à Anthropic) ne tourne jamais en administrateur.

use crate::{cputemp, win::w};
use std::{
    ptr::{null, null_mut},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use windows_sys::Win32::{
    Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, INVALID_HANDLE_VALUE},
    Security::{Authorization::ConvertStringSecurityDescriptorToSecurityDescriptorW, SECURITY_ATTRIBUTES},
    System::Memory::{CreateFileMappingW, MapViewOfFile, OpenFileMappingW, UnmapViewOfFile, FILE_MAP_READ, FILE_MAP_WRITE, PAGE_READWRITE},
    UI::{Shell::ShellExecuteW, WindowsAndMessaging::SW_HIDE},
};

const MAP: &str = r"Local\Deskorb.Sensors";
const TASK: &str = "Deskorb Sensors";

/// Ce que le service publie (16 octets).
#[repr(C)]
#[derive(Clone, Copy)]
struct Shared {
    stamp_ms: u64,   // heure du dernier relevé : un service arrêté laisse une valeur périmée
    temp_centi: i32, // °C × 100
    _pad: i32,
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

/// Boucle du service : ne revient pas. Quitte si le pilote est inaccessible ou si un service tourne déjà.
pub fn run() {
    let Ok(t) = cputemp::CpuTemp::open() else { return };
    unsafe {
        // lecture pour les utilisateurs de la session (le Deskorb normal n'est pas administrateur)
        let mut sd = null_mut();
        if ConvertStringSecurityDescriptorToSecurityDescriptorW(w("D:(A;;GA;;;BA)(A;;GA;;;SY)(A;;GR;;;IU)").as_ptr(), 1, &mut sd, null_mut()) == 0 {
            return;
        }
        let sa = SECURITY_ATTRIBUTES { nLength: size_of::<SECURITY_ATTRIBUTES>() as u32, lpSecurityDescriptor: sd, bInheritHandle: 0 };
        let map = CreateFileMappingW(INVALID_HANDLE_VALUE, &sa, PAGE_READWRITE, 0, size_of::<Shared>() as u32, w(MAP).as_ptr());
        if map.is_null() || GetLastError() == ERROR_ALREADY_EXISTS {
            return;
        }
        let view = MapViewOfFile(map, FILE_MAP_WRITE, 0, 0, size_of::<Shared>());
        if view.Value.is_null() {
            return;
        }
        let shared = view.Value as *mut Shared;
        loop {
            if let Some(c) = t.read() {
                shared.write_volatile(Shared { stamp_ms: now_ms(), temp_centi: (c * 100.0) as i32, _pad: 0 });
            }
            std::thread::sleep(Duration::from_secs(1));
        }
    }
}

/// Dernière température publiée par le service, si elle a moins de 5 s.
pub fn read() -> Option<f32> {
    unsafe {
        let map = OpenFileMappingW(FILE_MAP_READ, 0, w(MAP).as_ptr());
        if map.is_null() {
            return None;
        }
        let view = MapViewOfFile(map, FILE_MAP_READ, 0, 0, size_of::<Shared>());
        let s = (!view.Value.is_null()).then(|| (view.Value as *const Shared).read_volatile());
        if !view.Value.is_null() {
            UnmapViewOfFile(view);
        }
        CloseHandle(map);
        s.filter(|s| now_ms().saturating_sub(s.stamp_ms) < 5000).map(|s| s.temp_centi as f32 / 100.0)
    }
}

/// Crée la tâche planifiée (au démarrage de session, droits les plus élevés, sans limite de durée) et la lance.
/// Une seule demande administrateur, celle de Windows. Le service tourne depuis sa propre copie de l'exe
/// (%LOCALAPPDATA%\Deskorb) : sinon il verrouillerait l'exe de Deskorb et empêcherait toute mise à jour.
pub fn setup() {
    let (Ok(src), Some(local)) = (std::env::current_exe(), std::env::var_os("LOCALAPPDATA")) else { return };
    let exe = std::path::PathBuf::from(local).join("Deskorb").join("deskorb-sensors.exe");
    if std::fs::create_dir_all(exe.parent().unwrap()).and_then(|_| std::fs::copy(&src, &exe)).is_err() {
        return;
    }
    let user = format!("{}\\{}", std::env::var("USERDOMAIN").unwrap_or_default(), std::env::var("USERNAME").unwrap_or_default());
    let xml = format!(r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo><Description>Deskorb : température CPU (pilote PawnIO)</Description></RegistrationInfo>
  <Triggers><LogonTrigger><Enabled>true</Enabled><UserId>{user}</UserId></LogonTrigger></Triggers>
  <Principals><Principal id="Author"><UserId>{user}</UserId><LogonType>InteractiveToken</LogonType><RunLevel>HighestAvailable</RunLevel></Principal></Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <Priority>7</Priority>
  </Settings>
  <Actions Context="Author"><Exec><Command>"{}"</Command><Arguments>--sensors</Arguments></Exec></Actions>
</Task>"#, exe.display());
    let file = std::env::temp_dir().join("deskorb-sensors-task.xml");
    // schtasks lit le XML en UTF-16 (avec BOM)
    let bytes: Vec<u8> = [0xFEFFu16].into_iter().chain(xml.encode_utf16()).flat_map(|c| c.to_le_bytes()).collect();
    if std::fs::write(&file, bytes).is_err() {
        return;
    }
    let args = format!(r#"/c schtasks /Create /TN "{TASK}" /XML "{}" /F && schtasks /Run /TN "{TASK}""#, file.display());
    unsafe {
        ShellExecuteW(null_mut(), w("runas").as_ptr(), w("cmd.exe").as_ptr(), w(&args).as_ptr(), null(), SW_HIDE);
    }
}
