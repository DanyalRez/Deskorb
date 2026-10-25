//! Ce qui parle à Windows hors rendu : texte, capture du fond, icône de notification dynamique, menu.
//! `w`, `dib`, `text_layer`, `capture`, `menu` viennent tels quels de DESKTOUCH (shell.rs).

use crate::render::{Back, Layer, Rgb, Rgba};
use std::{
    ffi::c_void,
    mem::zeroed,
    ptr::{null, null_mut},
};
use windows_sys::Win32::{
    Foundation::*,
    Graphics::{Dwm::DwmFlush, Gdi::*},
    UI::{Shell::*, WindowsAndMessaging::*},
};

pub fn w(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

/// DIB 32 bits haut-en-bas : (bitmap, pointeur vers les pixels).
pub unsafe fn dib(w: i32, h: i32) -> (HBITMAP, *mut u32) {
    let mut bmi: BITMAPINFO = zeroed();
    bmi.bmiHeader = BITMAPINFOHEADER {
        biSize: size_of::<BITMAPINFOHEADER>() as u32, biWidth: w, biHeight: -h,
        biPlanes: 1, biBitCount: 32, biCompression: BI_RGB, ..zeroed()
    };
    let mut bits: *mut c_void = null_mut();
    let bmp = CreateDIBSection(null_mut(), &bmi, DIB_RGB_COLORS, &mut bits, null_mut(), 0);
    (bmp, bits as *mut u32)
}

fn unpack(p: u32) -> Rgba {
    [(p >> 16 & 255) as f32 / 255.0, (p >> 8 & 255) as f32 / 255.0, (p & 255) as f32 / 255.0, (p >> 24) as f32 / 255.0]
}

/// Texte à poser sur un calque.
pub struct Text<'a> {
    pub s: &'a str,
    pub r: RECT,
    pub size: f32,
    pub weight: i32,
    pub flags: DRAW_TEXT_FORMAT,
    pub level: f32, // 1 = plein, < 1 = texte secondaire
}

/// Calque de textes : GDI dessine en blanc sur noir, la luminosité devient le canal alpha (bords nets).
pub unsafe fn text_layer(pw: usize, ph: usize, texts: &[Text], ink: Rgb) -> Layer {
    let (bmp, bits) = dib(pw as i32, ph as i32);
    let dc = CreateCompatibleDC(null_mut());
    let old = SelectObject(dc, bmp);
    SetBkMode(dc, TRANSPARENT as i32);
    for t in texts {
        // caractères de la zone privée Unicode : pictogrammes de la police d'icônes de Windows 11 (roue dentée = E713)
        let glyphs = !t.s.is_empty() && t.s.chars().all(|c| ('\u{E000}'..='\u{F8FF}').contains(&c));
        let face = if glyphs { "Segoe Fluent Icons" } else { "Segoe UI Variable Text" };
        let font = CreateFontW(-t.size.round() as i32, 0, 0, 0, t.weight, 0, 0, 0, DEFAULT_CHARSET as u32, 0, 0,
            ANTIALIASED_QUALITY as u32, 0, w(face).as_ptr());
        let of = SelectObject(dc, font);
        let g = (255.0 * t.level).round() as u32;
        SetTextColor(dc, g | g << 8 | g << 16);
        let txt: Vec<u16> = t.s.encode_utf16().collect();
        let mut r = t.r;
        DrawTextW(dc, txt.as_ptr(), txt.len() as i32, &mut r, t.flags | DT_NOPREFIX | DT_SINGLELINE);
        SelectObject(dc, of);
        DeleteObject(font);
    }
    GdiFlush();
    let px = std::slice::from_raw_parts(bits, pw * ph).iter().map(|&p| {
        let a = ((p >> 16 & 255).max(p >> 8 & 255).max(p & 255)) as f32 / 255.0;
        [ink[0] * a, ink[1] * a, ink[2] * a, a]
    }).collect();
    SelectObject(dc, old);
    DeleteDC(dc);
    DeleteObject(bmp);
    Layer { w: pw, h: ph, px }
}

/// Copie de l'écran sous la fenêtre, sans la fenêtre elle-même (exclue de la capture le temps de la copie).
pub unsafe fn capture(hwnd: HWND, x: i32, y: i32, cw: usize, ch: usize) -> Back {
    SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE);
    DwmFlush();
    let screen = GetDC(null_mut());
    let (bmp, bits) = dib(cw as i32, ch as i32);
    let dc = CreateCompatibleDC(screen);
    let old = SelectObject(dc, bmp);
    BitBlt(dc, 0, 0, cw as i32, ch as i32, screen, x, y, SRCCOPY);
    GdiFlush();
    let px = std::slice::from_raw_parts(bits, cw * ch).iter().map(|&p| {
        let c = unpack(p);
        [c[0], c[1], c[2]]
    }).collect();
    SelectObject(dc, old);
    DeleteDC(dc);
    DeleteObject(bmp);
    ReleaseDC(null_mut(), screen);
    SetWindowDisplayAffinity(hwnd, WDA_NONE);
    Back { w: cw, h: ch, px }
}

// ---------- zone de notification ----------

/// Icône carrée générée à la volée : un texte court (ex. « 87 ») — c'est le « mini-widget » visible sans clic.
pub unsafe fn text_icon(label: &str) -> HICON {
    let n = GetSystemMetrics(SM_CXSMICON).max(16) as usize * 2; // 2× puis Windows réduit : bords plus nets
    let size = if label.chars().count() > 2 { n as f32 * 0.62 } else { n as f32 * 0.86 };
    let r = RECT { left: 0, top: 0, right: n as i32, bottom: n as i32 };
    let layer = text_layer(n, n, &[Text { s: label, r, size, weight: 700, flags: DT_CENTER | DT_VCENTER, level: 1.0 }], [1.0; 3]);
    alpha_icon(n, &layer.px.iter().map(|p| p[3]).collect::<Vec<_>>())
}

/// Arc à bouts ronds, dessiné dans le sens horaire depuis midi : distance signée au trait.
/// `c` centre, `r` rayon, `w` épaisseur, `sweep` angle en radians (0 à 2π).
pub fn sd_arc(p: (f32, f32), c: (f32, f32), r: f32, w: f32, sweep: f32) -> f32 {
    let (x, y) = (p.0 - c.0, p.1 - c.1);
    let a = x.atan2(-y).rem_euclid(std::f32::consts::TAU);
    if a <= sweep {
        return (x.hypot(y) - r).abs() - w / 2.0;
    }
    let end = (r * sweep.sin(), -r * sweep.cos());
    x.hypot(y + r).min((x - end.0).hypot(y - end.1)) - w / 2.0
}

/// Logo dessiné en direct, en blanc : les arcs se remplissent selon `a` (et `b`), sur un rail atténué.
/// style 0 = Orbite (une jauge + l'orbe), 1 = Jauges (souris dehors, tokens dedans). Géométrie des
/// versions « petites tailles » (assets/*-small.svg), en unités du carré de 256.
pub unsafe fn logo_icon(style: usize, a: Option<f32>, b: Option<f32>) -> HICON {
    let n = GetSystemMetrics(SM_CXSMICON).max(16) as usize;
    alpha_icon(n, &logo_alpha(style, n, a, b))
}

/// Opacité de chaque pixel du logo vivant, sur `n`×`n`.
pub fn logo_alpha(style: usize, n: usize, a: Option<f32>, b: Option<f32>) -> Vec<f32> {
    use std::f32::consts::PI;
    let (lo, span) = if style == 0 { (50.0, 156.0) } else { (48.0, 160.0) }; // cadrage serré sur le pictogramme
    let k = span / n as f32;
    let fill = |v: Option<f32>, max: f32| v.map_or(0.0, |v| v.clamp(0.0, 1.0) * max);
    let c = (128.0, 128.0);
    let mut px = Vec::with_capacity(n * n);
    for j in 0..n {
        for i in 0..n {
            let p = (lo + (i as f32 + 0.5) * k, lo + (j as f32 + 0.5) * k);
            let cov = |d: f32| (0.5 - d / k).clamp(0.0, 1.0);
            // jauge vide : rien, sinon le bout rond laisserait un point à midi
            let arc = |r: f32, w: f32, s: f32| if s > 0.0 { cov(sd_arc(p, c, r, w, s)) } else { 0.0 };
            let (track, full) = if style == 0 {
                let orb = cov((p.0 - 88.4).hypot(p.1 - 88.4) - 16.0);
                (arc(56.0, 30.0, 1.5 * PI), arc(56.0, 30.0, fill(a, 1.5 * PI)).max(orb))
            } else {
                let t = arc(66.0, 20.0, 1.5 * PI).max(arc(32.0, 20.0, PI));
                (t, arc(66.0, 20.0, fill(a, 1.5 * PI)).max(arc(32.0, 20.0, fill(b, PI))))
            };
            px.push(full.max(0.35 * track));
        }
    }
    px
}

/// Icône blanche dont seule l'opacité varie, pixel par pixel.
// shortcut: encre blanche fixe, illisible sur barre des tâches claire ; lire SystemUsesLightTheme au besoin
unsafe fn alpha_icon(n: usize, alpha: &[f32]) -> HICON {
    let (color, bits) = dib(n as i32, n as i32);
    for (d, &p) in std::slice::from_raw_parts_mut(bits, n * n).iter_mut().zip(alpha) {
        let a = (p * 255.0).round() as u32;
        *d = a << 24 | if a > 0 { 0xFF_FFFF } else { 0 }; // alpha droit (non prémultiplié) pour les icônes
    }
    let mask = CreateBitmap(n as i32, n as i32, 1, 1, null());
    let info = ICONINFO { fIcon: 1, xHotspot: 0, yHotspot: 0, hbmMask: mask, hbmColor: color };
    let icon = CreateIconIndirect(&info);
    DeleteObject(mask);
    DeleteObject(color);
    icon
}

/// Ajoute, met à jour ou retire l'icône ; l'ancienne icône est libérée par l'appelant.
pub unsafe fn tray(hwnd: HWND, msg: u32, op: NOTIFY_ICON_MESSAGE, icon: HICON, tip: &str) {
    let mut nid: NOTIFYICONDATAW = zeroed();
    nid.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
    nid.hWnd = hwnd;
    nid.uID = 1;
    nid.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
    nid.uCallbackMessage = msg;
    nid.hIcon = icon;
    for (d, c) in nid.szTip.iter_mut().take(127).zip(tip.encode_utf16()) {
        *d = c;
    }
    Shell_NotifyIconW(op, &nid);
}

/// Bulle de notification Windows attachée à l'icône (ex. batterie faible).
pub unsafe fn notify(hwnd: HWND, title: &str, text: &str) {
    let mut nid: NOTIFYICONDATAW = zeroed();
    nid.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
    nid.hWnd = hwnd;
    nid.uID = 1;
    nid.uFlags = NIF_INFO;
    nid.dwInfoFlags = NIIF_WARNING;
    for (d, c) in nid.szInfoTitle.iter_mut().take(63).zip(title.encode_utf16()) {
        *d = c;
    }
    for (d, c) in nid.szInfo.iter_mut().take(255).zip(text.encode_utf16()) {
        *d = c;
    }
    Shell_NotifyIconW(NIM_MODIFY, &nid);
}

/// Requête HTTPS par WinHTTP (déjà dans Windows) : (statut, corps). `headers` : lignes terminées par \r\n.
pub fn https(method: &str, host: &str, path: &str, headers: &str, body: Option<&[u8]>) -> Option<(u32, String)> {
    use windows_sys::Win32::Networking::WinHttp::*;
    unsafe {
        // proxy du système sans détection automatique (WPAD) : celle-ci laisse des threads tourner en permanence
        let session = WinHttpOpen(w("Deskorb").as_ptr(), WINHTTP_ACCESS_TYPE_DEFAULT_PROXY, null(), null(), 0);
        if session.is_null() {
            return None;
        }
        WinHttpSetTimeouts(session, 5000, 5000, 5000, 10_000);
        let conn = WinHttpConnect(session, w(host).as_ptr(), INTERNET_DEFAULT_HTTPS_PORT as u16, 0);
        let req = if conn.is_null() { null_mut() } else {
            WinHttpOpenRequest(conn, w(method).as_ptr(), w(path).as_ptr(), null(), null(), null(), WINHTTP_FLAG_SECURE)
        };
        let mut out = None;
        let h = w(headers);
        let (data, len) = body.map_or((null(), 0), |b| (b.as_ptr() as *const _, b.len() as u32));
        if !req.is_null() && WinHttpSendRequest(req, h.as_ptr(), u32::MAX, data, len, len, 0) != 0
            && WinHttpReceiveResponse(req, null_mut()) != 0
        {
            let (mut status, mut size) = (0u32, 4u32);
            WinHttpQueryHeaders(req, WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER, null(),
                &mut status as *mut u32 as _, &mut size, null_mut());
            let mut buf = Vec::new();
            loop {
                let mut avail = 0;
                if WinHttpQueryDataAvailable(req, &mut avail) == 0 || avail == 0 {
                    break;
                }
                let start = buf.len();
                buf.resize(start + avail as usize, 0);
                let mut got = 0;
                WinHttpReadData(req, buf[start..].as_mut_ptr() as _, avail, &mut got);
                buf.truncate(start + got as usize);
            }
            out = Some((status, String::from_utf8_lossy(&buf).into_owned()));
        }
        for handle in [req, conn, session] {
            if !handle.is_null() {
                WinHttpCloseHandle(handle);
            }
        }
        out
    }
}

/// Menu contextuel natif (id 0 = séparateur, booléen = coché) ; renvoie l'identifiant choisi (0 si rien).
pub unsafe fn menu(hwnd: HWND, entries: &[(usize, &str, bool)]) -> usize {
    let m = CreatePopupMenu();
    for &(id, label, checked) in entries {
        if id == 0 {
            AppendMenuW(m, MF_SEPARATOR, 0, null());
        } else {
            AppendMenuW(m, MF_STRING | if checked { MF_CHECKED } else { MF_UNCHECKED }, id, w(label).as_ptr());
        }
    }
    let mut p: POINT = zeroed();
    GetCursorPos(&mut p);
    SetForegroundWindow(hwnd); // sans ça, le menu ne se ferme pas en cliquant ailleurs
    let id = TrackPopupMenu(m, TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_BOTTOMALIGN, p.x, p.y, 0, hwnd, null());
    DestroyMenu(m);
    id as usize
}

#[cfg(test)]
mod tests {
    use super::sd_arc;
    use std::f32::consts::PI;

    #[test]
    fn arc_distance() {
        let c = (0.0, 0.0);
        // sur le trait, à midi puis à 3 h, pour un quart de tour
        assert!(sd_arc((0.0, -10.0), c, 10.0, 2.0, PI / 2.0) < 0.0);
        assert!(sd_arc((10.0, 0.0), c, 10.0, 2.0, PI / 2.0) < 0.0);
        // à 6 h : hors du quart de tour, mais dedans pour un demi-tour
        assert!(sd_arc((0.0, 10.0), c, 10.0, 2.0, PI / 2.0) > 5.0);
        assert!(sd_arc((0.0, 10.0), c, 10.0, 2.0, PI) < 0.0);
        // jauge vide : il reste le bout rond à midi
        assert!(sd_arc((0.0, -10.0), c, 10.0, 2.0, 0.0) < 0.0);
        assert!((sd_arc((0.0, 0.0), c, 10.0, 2.0, 1.0) - 9.0).abs() < 1e-4);
    }
}
