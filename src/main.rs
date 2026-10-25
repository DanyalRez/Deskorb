//! DESKORB : batterie de la Pulsar TenZ et quota de tokens, dans la zone de notification.
//! L'icône affiche le % en direct (mini-widget) ; un clic ouvre un panneau Liquid Glass au-dessus de la barre.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod mouse;
#[allow(dead_code)] // copié de DESKTOUCH, tout n'est pas encore utilisé
mod render;
mod tokens;
mod win;

use render::{Back, Frame, Look, Pill, Rgb, Rr};
use std::{
    cell::RefCell,
    mem::zeroed,
    ptr::{null, null_mut},
    time::Instant,
};
use windows_sys::Win32::{
    Foundation::*,
    Graphics::Gdi::*,
    System::LibraryLoader::GetModuleHandleW,
    UI::{HiDpi::*, Shell::*, WindowsAndMessaging::*},
};

const WM_TRAY: u32 = WM_APP + 1;
const T_POLL: usize = 1;
const POLL_MS: u32 = 15_000;
const ID_QUIT: usize = 1;
const ID_STYLE: usize = 10; // + 0 Orbite, 1 Jauges, 2 Pourcentage
const STYLES: [&str; 3] = ["Icône : Orbite", "Icône : Jauges", "Icône : Pourcentage"];
// panneau, en pixels logiques (96 dpi) ; la marge laisse la place à l'ombre
const W: f32 = 320.0;
const H: f32 = 196.0;
const R: f32 = 26.0;
const M: f32 = 30.0;
const GAP: f32 = 4.0; // entre le panneau et la barre des tâches

struct App {
    hwnd: HWND,
    dc: HDC,
    bits: *mut u32,
    win: RECT,
    back: Back,
    mouse: Option<mouse::Mouse>,
    usage: Option<tokens::Usage>,
    icon: HICON,
    style: usize,
    shown: bool,
    hidden_at: Instant,
    taskbar_created: u32,
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

/// `None` aussi quand l'état est déjà emprunté : ShowWindow, SetForegroundWindow ou un menu renvoient des
/// messages (WM_ACTIVATE…) pendant qu'on le modifie ; on les ignore au lieu de paniquer.
fn app<T>(f: impl FnOnce(&mut App) -> T) -> Option<T> {
    APP.with(|a| a.try_borrow_mut().ok()?.as_mut().map(f))
}

/// Liquid Glass de DESKTOUCH ; le texte passe en foncé sur un fond clair.
fn look(light_back: bool) -> Look {
    if light_back {
        Look { refraction: 1.0, tint: [1.0; 3], tint_amount: 0.22, ink: [0.08, 0.08, 0.10] }
    } else {
        Look { refraction: 1.0, tint: [1.0; 3], tint_amount: 0.06, ink: [1.0; 3] }
    }
}

impl App {
    /// Texte de l'icône : % de batterie, sinon % de tokens, sinon un tiret.
    fn label(&self) -> String {
        match (self.mouse.as_ref().and_then(|m| m.battery), &self.usage) {
            (Some(b), _) => b.to_string(),
            (None, Some(u)) => format!("{:.0}", u.pct() * 100.0),
            _ => "–".into(),
        }
    }

    fn mouse_line(&self) -> String {
        let Some(m) = &self.mouse else { return "Non détectée".into() };
        let mut parts = vec![if m.wired { "Filaire" } else { "Sans fil" }.to_string()];
        if m.asleep {
            parts.push("en veille".into());
        }
        if m.charging {
            parts.push("en charge".into());
        }
        if let Some(d) = m.dpi {
            parts.push(format!("{d} DPI"));
        }
        if let Some(p) = m.polling_hz {
            parts.push(format!("{p} Hz"));
        }
        parts.join(" · ")
    }

    fn token_lines(&self) -> (String, String) {
        let Some(u) = &self.usage else { return ("—".into(), "Source non configurée".into()) };
        (
            format!("{:.0} %", u.pct() * 100.0),
            format!("{} / {} · reste {} · reset {}", tokens::fmt_count(u.used), tokens::fmt_count(u.limit),
                tokens::fmt_count(u.remaining()), tokens::fmt_left(u.left())),
        )
    }

    fn tip(&self) -> String {
        let bat = self.mouse.as_ref().and_then(|m| m.battery).map_or("—".into(), |b| format!("{b} %"));
        format!("Pulsar : {bat}\nTokens : {}", self.token_lines().0)
    }

    unsafe fn refresh(&mut self) {
        let last = self.mouse.as_ref().and_then(|m| m.battery);
        self.mouse = mouse::read();
        if let Some(m) = self.mouse.as_mut().filter(|m| m.battery.is_none()) {
            m.battery = last; // en veille : on garde la dernière valeur lue
        }
        self.usage = tokens::read();
        let old = self.icon;
        let bat = self.mouse.as_ref().and_then(|m| m.battery).map(|b| b as f32 / 100.0);
        let tok = self.usage.as_ref().map(|u| u.pct());
        self.icon = match self.style {
            0 => win::logo_icon(0, bat.or(tok), None), // une seule jauge : la batterie, sinon les tokens
            1 => win::logo_icon(1, bat, tok),
            _ => win::text_icon(&self.label()),
        };
        win::tray(self.hwnd, WM_TRAY, NIM_MODIFY, self.icon, &self.tip());
        DestroyIcon(old);
        if self.shown {
            self.draw();
        }
    }

    fn scale(&self) -> f32 {
        unsafe { GetDpiForWindow(self.hwnd) as f32 / 96.0 }
    }

    /// Ouvre le panneau en bas à droite de la zone de travail de l'écran où se trouve le curseur.
    // shortcut: suppose la barre des tâches en bas ; lire son bord (SHAppBarMessage) si elle est ailleurs
    unsafe fn show(&mut self) {
        let mut p: POINT = zeroed();
        GetCursorPos(&mut p);
        let mut mi: MONITORINFO = zeroed();
        mi.cbSize = size_of::<MONITORINFO>() as u32;
        GetMonitorInfoW(MonitorFromPoint(p, MONITOR_DEFAULTTONEAREST), &mut mi);
        let s = self.scale();
        let (ww, wh) = (((W + 2.0 * M) * s) as i32, ((H + 2.0 * M) * s) as i32);
        let right = mi.rcWork.right - ((GAP + 8.0 - M) * s) as i32;
        let bottom = mi.rcWork.bottom - ((GAP - M) * s) as i32;
        self.win = RECT { left: right - ww, top: bottom - wh, right, bottom };
        let (bmp, bits) = win::dib(ww, wh);
        DeleteObject(SelectObject(self.dc, bmp));
        self.bits = bits;
        self.back = win::capture(self.hwnd, self.win.left, self.win.top, ww as usize, wh as usize);
        self.back.frost((16.0 * s) as usize);
        self.shown = true;
        self.draw();
        ShowWindow(self.hwnd, SW_SHOW);
        SetForegroundWindow(self.hwnd); // pour se fermer quand on clique ailleurs (WM_ACTIVATE)
    }

    unsafe fn hide(&mut self) {
        self.shown = false;
        self.hidden_at = Instant::now();
        ShowWindow(self.hwnd, SW_HIDE);
    }

    unsafe fn draw(&self) {
        let s = self.scale();
        let (ww, wh) = ((self.win.right - self.win.left) as usize, (self.win.bottom - self.win.top) as usize);
        let px = |v: f32| (v * s) as i32;
        let (x0, y0) = (M, M);
        let probe = (px(x0) as usize, px(y0) as usize, px(W) as usize, px(H) as usize);
        let look = look(self.back.luminance(probe.0, probe.1, probe.2, probe.3) > 0.6);

        let bat = self.mouse.as_ref().and_then(|m| m.battery);
        let (tok_pct, tok_detail) = self.token_lines();
        let mouse_line = self.mouse_line();
        let bat_txt = bat.map_or("—".into(), |b| format!("{b} %"));
        let rect = |x: f32, y: f32, w: f32, h: f32| RECT { left: px(x0 + x), top: px(y0 + y), right: px(x0 + x + w), bottom: px(y0 + y + h) };
        let t = |s, r, size: f32, weight, flags, level| win::Text { s, r, size: size * px(1.0).max(1) as f32, weight, flags, level };
        let texts = [
            t("Pulsar TenZ", rect(20.0, 16.0, 200.0, 22.0), 14.0, 600, DT_LEFT | DT_VCENTER, 1.0),
            t(&bat_txt, rect(180.0, 12.0, 120.0, 30.0), 22.0, 600, DT_RIGHT | DT_VCENTER, 1.0),
            t(&mouse_line, rect(20.0, 40.0, 280.0, 18.0), 12.0, 400, DT_LEFT | DT_VCENTER, 0.7),
            t("Tokens", rect(20.0, 104.0, 200.0, 22.0), 14.0, 600, DT_LEFT | DT_VCENTER, 1.0),
            t(&tok_pct, rect(180.0, 100.0, 120.0, 30.0), 22.0, 600, DT_RIGHT | DT_VCENTER, 1.0),
            t(&tok_detail, rect(20.0, 160.0, 280.0, 18.0), 12.0, 400, DT_LEFT | DT_VCENTER, 0.7),
        ];
        let layer = win::text_layer(ww, wh, &texts, look.ink);

        // barres de progression : rail + remplissage, posées sur le verre
        let bar = |y: f32, v: f32, color: Rgb| {
            let rail = Rr { x: (x0 + 20.0) * s, y: (y0 + y) * s, w: 280.0 * s, h: 8.0 * s, r: 4.0 * s };
            let fill = Rr { w: (rail.w * v.clamp(0.0, 1.0)).max(rail.h), ..rail };
            [Pill { rr: rail, color: look.ink, amount: 0.12, rim: 0.0 }, Pill { rr: fill, color, amount: if v > 0.0 { 0.9 } else { 0.0 }, rim: 0.4 }]
        };
        let tok = self.usage.as_ref().map_or(0.0, |u| u.pct());
        let pills: Vec<Pill> = bar(66.0, bat.map_or(0.0, |b| b as f32 / 100.0), [0.35, 0.85, 0.45])
            .into_iter().chain(bar(140.0, tok, [0.45, 0.6, 1.0])).collect();

        let f = Frame {
            back: &self.back,
            layers: &[(&layer, (0, 0), 1.0)],
            a: Rr { x: x0 * s, y: y0 * s, w: W * s, h: H * s, r: R * s },
            b: None,
            smooth: 0.0,
            refr_height: 22.0 * s,
            refr_amount: 36.0 * s,
            glyph: false,
            pills: &pills,
            alpha: 1.0,
            scale: s,
            look,
        };
        render::render(&f, std::slice::from_raw_parts_mut(self.bits, ww * wh), ww);
        GdiFlush();
        let pos = POINT { x: self.win.left, y: self.win.top };
        let size = SIZE { cx: ww as i32, cy: wh as i32 };
        let src = POINT { x: 0, y: 0 };
        let blend = BLENDFUNCTION { BlendOp: AC_SRC_OVER as u8, BlendFlags: 0, SourceConstantAlpha: 255, AlphaFormat: AC_SRC_ALPHA as u8 };
        UpdateLayeredWindow(self.hwnd, null_mut(), &pos, &size, self.dc, &src, 0, &blend, ULW_ALPHA);
    }
}

unsafe extern "system" fn proc(h: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_TRAY => match lp as u32 {
            WM_LBUTTONUP => {
                app(|a| {
                    // le clic sur l'icône a déjà fermé le panneau (perte du focus) : ne pas le rouvrir aussitôt
                    if !a.shown && a.hidden_at.elapsed().as_millis() > 250 {
                        a.show();
                    }
                });
            }
            WM_RBUTTONUP => {
                let style = app(|a| a.style).unwrap_or(0);
                let mut items: Vec<_> = STYLES.iter().enumerate().map(|(i, s)| (ID_STYLE + i, *s, i == style)).collect();
                items.extend([(0, "", false), (ID_QUIT, "Quitter", false)]);
                match win::menu(h, &items) {
                    ID_QUIT => {
                        DestroyWindow(h);
                    }
                    id if (ID_STYLE..ID_STYLE + STYLES.len()).contains(&id) => {
                        app(|a| {
                            a.style = id - ID_STYLE;
                            save_style(a.style);
                            a.refresh();
                        });
                    }
                    _ => {}
                }
            }
            _ => {}
        },
        WM_TIMER if wp == T_POLL => {
            app(|a| a.refresh());
        }
        WM_ACTIVATE if wp & 0xFFFF == WA_INACTIVE as usize => {
            app(|a| if a.shown { a.hide() });
        }
        WM_DESTROY => {
            win::tray(h, WM_TRAY, NIM_DELETE, null_mut(), "");
            PostQuitMessage(0);
        }
        // Explorer redémarré : l'icône a disparu, on la remet
        _ if app(|a| a.taskbar_created == msg).unwrap_or(false) => {
            app(|a| win::tray(h, WM_TRAY, NIM_ADD, a.icon, &a.tip()));
        }
        _ => return DefWindowProcW(h, msg, wp, lp),
    }
    0
}

/// Choix d'icône, gardé dans %APPDATA%\Deskorb\icone.txt.
fn style_file() -> Option<std::path::PathBuf> {
    Some(std::path::PathBuf::from(std::env::var_os("APPDATA")?).join("Deskorb").join("icone.txt"))
}

fn load_style() -> usize {
    style_file().and_then(|f| std::fs::read_to_string(f).ok()).and_then(|s| s.trim().parse().ok()).filter(|&s| s < STYLES.len()).unwrap_or(0)
}

fn save_style(style: usize) {
    if let Some(f) = style_file() {
        let _ = std::fs::create_dir_all(f.parent().unwrap()).and_then(|_| std::fs::write(f, style.to_string()));
    }
}

fn main() {
    if std::env::args().any(|a| a == "--probe") {
        return mouse::probe();
    }
    if std::env::args().any(|a| a == "--tokens") {
        let t = Instant::now();
        return match tokens::read() {
            Some(u) => println!("{} / {} ({:.0} %), reset dans {} — lu en {:?}", tokens::fmt_count(u.used),
                tokens::fmt_count(u.limit), u.pct() * 100.0, tokens::fmt_left(u.left()), t.elapsed()),
            None => println!("aucune donnée Claude Code"),
        };
    }
    unsafe {
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let inst = GetModuleHandleW(null());
        let class = win::w("DESKORB");
        let wc = WNDCLASSW { lpfnWndProc: Some(proc), hInstance: inst, lpszClassName: class.as_ptr(),
            hCursor: LoadCursorW(null_mut(), IDC_ARROW), ..zeroed() };
        RegisterClassW(&wc);
        let h = CreateWindowExW(WS_EX_TOOLWINDOW | WS_EX_LAYERED | WS_EX_TOPMOST,
            class.as_ptr(), class.as_ptr(), WS_POPUP, 0, 0, 1, 1, null_mut(), null_mut(), inst, null());
        let icon = win::text_icon("–");
        win::tray(h, WM_TRAY, NIM_ADD, icon, "DESKORB");
        APP.with_borrow_mut(|a| *a = Some(App {
            hwnd: h, dc: CreateCompatibleDC(null_mut()), bits: null_mut(), win: zeroed(), back: Back::empty(),
            mouse: None, usage: None, icon, style: load_style(), shown: false, hidden_at: Instant::now(),
            taskbar_created: RegisterWindowMessageW(win::w("TaskbarCreated").as_ptr()),
        }));
        app(|a| a.refresh());
        SetTimer(h, T_POLL, POLL_MS, None);

        let mut msg: MSG = zeroed();
        while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}
