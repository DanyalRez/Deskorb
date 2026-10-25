//! DESKORB : batterie de la Pulsar TenZ et quota de tokens, dans la zone de notification.
//! L'icône affiche l'état en direct (mini-widget) ; un clic ouvre un panneau Liquid Glass au-dessus de la barre,
//! le clic droit ouvre les réglages. Deux fenêtres en couches dessinées par le moteur de DESKTOUCH.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod mouse;
#[allow(dead_code)] // copié de DESKTOUCH, tout n'est pas utilisé
mod render;
mod settings;
mod tokens;
mod win;

use render::{smoothstep, Back, Frame, Layer, Look, Pill, Rgb, Rr, Spring};
use settings::Config;
use std::{
    cell::RefCell,
    mem::zeroed,
    ptr::{null, null_mut},
    time::Instant,
};
use windows_sys::Win32::{
    Foundation::*,
    Graphics::{Dwm::DwmFlush, Gdi::*},
    System::LibraryLoader::GetModuleHandleW,
    UI::{HiDpi::*, Input::KeyboardAndMouse::*, Shell::*, WindowsAndMessaging::*},
};

const WM_TRAY: u32 = WM_APP + 1;
const T_POLL: usize = 1;
const ID_SETTINGS: usize = 1;
const ID_QUIT: usize = 2;
// pixels logiques (96 dpi) ; la marge autour des formes laisse la place à l'ombre
const M: f32 = 30.0;
const GAP: f32 = 4.0; // entre une fenêtre et la barre des tâches
const PANEL_W: f32 = 320.0;
const PANEL_R: f32 = 26.0;
const PREFS_W: f32 = 340.0;
const PREFS_TOP: f32 = 54.0; // sous le titre
const LOW_BATTERY: u8 = 20;
const HEADER: f32 = 30.0; // en-tête du panneau : nom de l'app et bouton des réglages
const GEAR: (f32, f32, f32) = (PANEL_W - 44.0, 8.0, 32.0); // x, y, côté (relatif au panneau)

/// Texte anglais ou français.
type Tr = (&'static str, &'static str);

fn tr(fr: bool, t: Tr) -> &'static str {
    if fr { t.1 } else { t.0 }
}

/// Contrôle d'une ligne des réglages.
enum Ctl {
    Seg(&'static [Tr]),
    Slider,
    Toggle,
}

const ROWS: [(Tr, Ctl); 8] = [
    (("Theme", "Thème"), Ctl::Seg(&[("Liquid Glass", "Liquid Glass"), ("Dark", "Sombre"), ("Light", "Clair")])),
    (("Tray icon", "Icône"), Ctl::Seg(&[("Orbit", "Orbite"), ("Gauges", "Jauges"), ("Percent", "Pourcentage")])),
    (("Panel", "Panneau"), Ctl::Seg(&[("Detailed", "Détaillé"), ("Compact", "Compact")])),
    (("Token limit", "Limite de tokens"), Ctl::Slider),
    (("Refresh every", "Actualiser toutes les"), Ctl::Seg(&[("5 s", "5 s"), ("15 s", "15 s"), ("1 min", "1 min")])),
    (("Language", "Langue"), Ctl::Seg(&[("English", "English"), ("Français", "Français")])),
    (("Low battery alert", "Alerte batterie faible"), Ctl::Toggle),
    (("Launch at startup", "Lancer au démarrage"), Ctl::Toggle),
];
const ROW_LIMIT: usize = 3;

fn row_height(c: &Ctl) -> f32 {
    match c {
        Ctl::Seg(_) => 66.0,
        Ctl::Slider => 58.0,
        Ctl::Toggle => 42.0,
    }
}

/// Haut de chaque ligne (relatif au panneau des réglages) et hauteur totale.
fn row_tops() -> ([f32; ROWS.len()], f32) {
    let mut tops = [0.0; ROWS.len()];
    let mut y = PREFS_TOP;
    for (t, (_, c)) in tops.iter_mut().zip(&ROWS) {
        *t = y;
        y += row_height(c);
    }
    (tops, y + 10.0)
}

/// Apparence selon le thème ; en Liquid Glass, le texte s'adapte au fond.
fn look(theme: usize, light_back: bool) -> Look {
    const DARK_INK: Rgb = [0.08, 0.08, 0.10];
    match theme {
        1 => Look { refraction: 0.0, tint: [0.07, 0.07, 0.09], tint_amount: 0.62, ink: [1.0; 3] },
        2 => Look { refraction: 0.0, tint: [0.97, 0.97, 0.98], tint_amount: 0.68, ink: DARK_INK },
        _ if light_back => Look { refraction: 1.0, tint: [1.0; 3], tint_amount: 0.22, ink: DARK_INK },
        _ => Look { refraction: 1.0, tint: [1.0; 3], tint_amount: 0.06, ink: [1.0; 3] },
    }
}

/// Couleur des éléments posés sur le verre : blanc sur verre sombre, noir sur verre clair.
fn accent(l: &Look) -> Rgb {
    if l.ink[0] > 0.5 { [1.0; 3] } else { [0.0; 3] }
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

/// Fenêtre en couches dessinée par notre moteur (pixels à nous, alpha par pixel).
struct Surface {
    hwnd: HWND,
    win: RECT,
    dc: HDC,
    dib: HBITMAP,
    bits: *mut u32,
    back: Back,
    layer: Layer,
    light_back: bool,
    shown: bool,
    open: Spring,
    hidden_at: Instant,
}

impl Surface {
    unsafe fn new(hwnd: HWND) -> Self {
        Surface { hwnd, win: zeroed(), dc: CreateCompatibleDC(null_mut()), dib: null_mut(), bits: null_mut(),
            back: Back::empty(), layer: Layer::empty(), light_back: false, shown: false,
            open: Spring::new(0.0, 320.0, 0.82), hidden_at: Instant::now() }
    }

    fn size(&self) -> (usize, usize) {
        ((self.win.right - self.win.left) as usize, (self.win.bottom - self.win.top) as usize)
    }

    /// Place la fenêtre en bas à droite de l'écran du curseur, au-dessus de la barre des tâches.
    // shortcut: suppose la barre des tâches en bas ; lire son bord (SHAppBarMessage) si elle est ailleurs
    unsafe fn place_corner(&mut self, w: f32, h: f32, s: f32) {
        let mut p: POINT = zeroed();
        GetCursorPos(&mut p);
        let mut mi: MONITORINFO = zeroed();
        mi.cbSize = size_of::<MONITORINFO>() as u32;
        GetMonitorInfoW(MonitorFromPoint(p, MONITOR_DEFAULTTONEAREST), &mut mi);
        let (ww, wh) = (((w + 2.0 * M) * s) as i32, ((h + 2.0 * M) * s) as i32);
        let right = mi.rcWork.right - ((GAP + 8.0 - M) * s) as i32;
        let bottom = mi.rcWork.bottom - ((GAP - M) * s) as i32;
        let old = self.size();
        self.win = RECT { left: right - ww, top: bottom - wh, right, bottom };
        if old != self.size() || self.dib.is_null() {
            if !self.dib.is_null() {
                DeleteObject(self.dib);
            }
            let (bmp, bits) = win::dib(ww, wh);
            (self.dib, self.bits) = (bmp, bits);
            SelectObject(self.dc, bmp);
        }
    }

    /// Nouvelle photo du fond, floutée ; la zone `probe` décide du texte clair ou foncé.
    unsafe fn capture(&mut self, blur: usize, probe: Rr) {
        let (ww, wh) = self.size();
        self.back = win::capture(self.hwnd, self.win.left, self.win.top, ww, wh);
        self.back.frost(blur);
        self.light_back = self.back.luminance(probe.x as usize, probe.y as usize, probe.w as usize, probe.h as usize) > 0.6;
    }

    unsafe fn present(&self, f: &Frame) {
        let (ww, wh) = self.size();
        render::render(f, std::slice::from_raw_parts_mut(self.bits, ww * wh), ww);
        GdiFlush();
        let pos = POINT { x: self.win.left, y: self.win.top };
        let size = SIZE { cx: ww as i32, cy: wh as i32 };
        let src = POINT { x: 0, y: 0 };
        let blend = BLENDFUNCTION { BlendOp: AC_SRC_OVER as u8, BlendFlags: 0, SourceConstantAlpha: 255, AlphaFormat: AC_SRC_ALPHA as u8 };
        UpdateLayeredWindow(self.hwnd, null_mut(), &pos, &size, self.dc, &src, 0, &blend, ULW_ALPHA);
    }

    /// Affiche sans activer puis demande le premier plan, pour se ranger quand on clique ailleurs (WM_ACTIVATE).
    unsafe fn show(&mut self) {
        ShowWindow(self.hwnd, SW_SHOWNA);
        SetForegroundWindow(self.hwnd);
        self.shown = true;
        self.open.target = 1.0;
    }
}

struct App {
    cfg: Config,
    panel: Surface,
    prefs: Surface,
    p_rows: Vec<Spring>, // une par ligne : case choisie, curseur ou interrupteur
    dragging: bool,
    mouse: Option<mouse::Mouse>,
    usage: Option<tokens::Usage>,
    alerted: bool,
    icon: HICON,
    taskbar_created: u32,
    animating: bool,
    last: Instant,
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

/// `None` aussi quand l'état est déjà emprunté : ShowWindow, SetForegroundWindow ou un menu renvoient des
/// messages (WM_ACTIVATE…) pendant qu'on le modifie ; on les ignore au lieu de paniquer.
fn app<T>(f: impl FnOnce(&mut App) -> T) -> Option<T> {
    APP.with(|a| a.try_borrow_mut().ok()?.as_mut().map(f))
}

impl App {
    fn fr(&self) -> bool {
        self.cfg.french
    }

    fn scale(&self) -> f32 {
        unsafe { GetDpiForWindow(self.panel.hwnd) as f32 / 96.0 }
    }

    fn kick(&mut self) {
        if !self.animating {
            self.animating = true;
            self.last = Instant::now();
        }
    }

    // ---------- données ----------

    fn battery(&self) -> Option<u8> {
        self.mouse.as_ref().and_then(|m| m.battery)
    }

    fn mouse_line(&self) -> String {
        let fr = self.fr();
        let Some(m) = &self.mouse else { return tr(fr, ("Not detected", "Non détectée")).into() };
        let mut parts = vec![tr(fr, if m.wired { ("Wired", "Filaire") } else { ("Wireless", "Sans fil") })];
        if m.asleep {
            parts.push(tr(fr, ("asleep", "en veille")));
        }
        if m.charging {
            parts.push(tr(fr, ("charging", "en charge")));
        }
        parts.join(" · ")
    }

    fn token_lines(&self) -> (String, String) {
        let fr = self.fr();
        let Some(u) = &self.usage else { return ("—".into(), tr(fr, ("No Claude Code data", "Aucune donnée Claude Code")).into()) };
        (
            format!("{:.0} %", u.pct() * 100.0),
            format!("{} / {} · {} {} · reset {}", tokens::fmt_count(u.used, fr), tokens::fmt_count(u.limit, fr),
                tr(fr, ("left", "reste")), tokens::fmt_count(u.remaining(), fr), tokens::fmt_left(u.left())),
        )
    }

    fn tip(&self) -> String {
        let bat = self.battery().map_or("—".into(), |b| format!("{b} %"));
        format!("Pulsar : {bat}\nTokens : {}", self.token_lines().0)
    }

    unsafe fn refresh(&mut self) {
        let last = self.battery();
        self.mouse = mouse::read();
        if let Some(m) = self.mouse.as_mut().filter(|m| m.battery.is_none()) {
            m.battery = last; // en veille : on garde la dernière valeur lue
        }
        let limit = (self.cfg.limit_m > 0).then(|| self.cfg.limit_m as u64 * 1_000_000);
        self.usage = tokens::read(limit);
        self.check_low_battery();
        self.refresh_icon();
        if self.panel.shown {
            self.rebuild_panel_layer();
            self.kick();
        }
    }

    /// Une notification quand la batterie passe sous le seuil ; réarmée à 25 % ou en charge.
    unsafe fn check_low_battery(&mut self) {
        let Some(m) = &self.mouse else { return };
        match m.battery {
            Some(b) if m.charging || b >= LOW_BATTERY + 5 => self.alerted = false,
            Some(b) if b <= LOW_BATTERY && !self.alerted && self.cfg.alert => {
                self.alerted = true;
                let fr = self.fr();
                win::notify(self.panel.hwnd, tr(fr, ("Low mouse battery", "Batterie de la souris faible")),
                    &format!("Pulsar TenZ : {b} %. {}", tr(fr, ("Time to charge it.", "Pense à la recharger."))));
            }
            _ => {}
        }
    }

    unsafe fn refresh_icon(&mut self) {
        let old = self.icon;
        let bat = self.battery().map(|b| b as f32 / 100.0);
        let tok = self.usage.as_ref().map(|u| u.pct());
        self.icon = match self.cfg.icon {
            0 => win::logo_icon(0, bat.or(tok), None), // une seule jauge : la batterie, sinon les tokens
            1 => win::logo_icon(1, bat, tok),
            _ => win::text_icon(&match (bat, tok) {
                (Some(b), _) | (None, Some(b)) => format!("{:.0}", b * 100.0),
                _ => "–".into(),
            }),
        };
        win::tray(self.panel.hwnd, WM_TRAY, NIM_MODIFY, self.icon, &self.tip());
        DestroyIcon(old);
    }

    // ---------- panneau ----------

    fn panel_h(&self) -> f32 {
        HEADER + if self.cfg.compact { 116.0 } else { 196.0 }
    }

    fn panel_rr(&self) -> Rr {
        let s = self.scale();
        Rr { x: M * s, y: M * s, w: PANEL_W * s, h: self.panel_h() * s, r: PANEL_R * s }
    }

    unsafe fn show_panel(&mut self) {
        let s = self.scale();
        self.panel.place_corner(PANEL_W, self.panel_h(), s);
        self.panel.capture((16.0 * s) as usize, self.panel_rr());
        self.rebuild_panel_layer();
        self.panel.open.snap(0.0);
        self.draw_panel();
        self.panel.show();
        self.kick();
    }

    /// Lignes du panneau : (titre, valeur, détail) ×2, aux hauteurs du mode choisi.
    unsafe fn rebuild_panel_layer(&mut self) {
        let s = self.scale();
        let p = self.panel_rr();
        let px = |v: f32| (v * s) as i32;
        let rect = |x: f32, y: f32, w: f32, h: f32| RECT { left: px(x), top: px(y), right: px(x + w), bottom: px(y + h) };
        let t = |txt, r, size: f32, weight, flags, level| win::Text { s: txt, r, size: size * s, weight, flags, level };
        let bat_txt = self.battery().map_or("—".into(), |b| format!("{b} %"));
        let (tok_pct, tok_detail) = self.token_lines();
        let mouse_line = self.mouse_line();
        let (h, g) = (HEADER, GEAR);
        let tokens_y = h + if self.cfg.compact { 66.0 } else { 104.0 };
        let mut texts = vec![
            t("DESKORB", rect(20.0, 12.0, 200.0, 20.0), 11.0, 700, DT_LEFT | DT_VCENTER, 0.55),
            t("\u{E713}", rect(g.0, g.1, g.2, g.2), 14.0, 400, DT_CENTER | DT_VCENTER, 0.75),
            t("Pulsar TenZ", rect(20.0, h + 16.0, 200.0, 22.0), 14.0, 600, DT_LEFT | DT_VCENTER, 1.0),
            t(&bat_txt, rect(180.0, h + 12.0, 120.0, 30.0), 22.0, 600, DT_RIGHT | DT_VCENTER, 1.0),
            t("Tokens", rect(20.0, tokens_y, 200.0, 22.0), 14.0, 600, DT_LEFT | DT_VCENTER, 1.0),
            t(&tok_pct, rect(180.0, tokens_y - 4.0, 120.0, 30.0), 22.0, 600, DT_RIGHT | DT_VCENTER, 1.0),
        ];
        if !self.cfg.compact {
            texts.push(t(&mouse_line, rect(20.0, h + 40.0, 280.0, 18.0), 12.0, 400, DT_LEFT | DT_VCENTER, 0.7));
            texts.push(t(&tok_detail, rect(20.0, h + 160.0, 280.0, 18.0), 12.0, 400, DT_LEFT | DT_VCENTER, 0.7));
        }
        let ink = look(self.cfg.theme, self.panel.light_back).ink;
        self.panel.layer = win::text_layer(p.w as usize, p.h as usize, &texts, ink);
    }

    unsafe fn draw_panel(&self) {
        let o = self.panel.open.x;
        let p = self.panel_rr();
        let s = self.scale();
        // entre en grandissant un peu depuis le coin bas-droit, côté zone de notification
        let k = lerp(0.94, 1.0, o.clamp(0.0, 1.0));
        let shape = Rr { x: p.x + p.w * (1.0 - k), y: p.y + p.h * (1.0 - k), w: p.w * k, h: p.h * k, r: p.r };
        let content = smoothstep(0.4, 1.0, o);
        let look = look(self.cfg.theme, self.panel.light_back);
        let (bar_bat, bar_tok) = if self.cfg.compact { (HEADER + 46.0, HEADER + 94.0) } else { (HEADER + 66.0, HEADER + 140.0) };
        let bar = |y: f32, v: f32, color: Rgb| {
            let rail = Rr { x: p.x + 20.0 * s, y: p.y + y * s, w: 280.0 * s, h: 8.0 * s, r: 4.0 * s };
            let fill = Rr { w: (rail.w * v.clamp(0.0, 1.0)).max(rail.h), ..rail };
            [Pill { rr: rail, color: look.ink, amount: 0.12 * content, rim: 0.0 },
                Pill { rr: fill, color, amount: if v > 0.0 { 0.9 * content } else { 0.0 }, rim: 0.4 * content }]
        };
        let bat = self.battery().map_or(0.0, |b| b as f32 / 100.0);
        // batterie : vert, orange sous 20 %
        let bat_color = if bat > 0.0 && bat <= LOW_BATTERY as f32 / 100.0 { [1.0, 0.62, 0.25] } else { [0.35, 0.85, 0.45] };
        let tok = self.usage.as_ref().map_or(0.0, |u| u.pct());
        let mut pills: Vec<Pill> = bar(bar_bat, bat, bat_color).into_iter().chain(bar(bar_tok, tok, [0.45, 0.6, 1.0])).collect();
        // bouton des réglages : une pastille discrète sous la roue dentée
        let g = GEAR;
        pills.push(Pill { rr: Rr { x: p.x + g.0 * s, y: p.y + g.1 * s, w: g.2 * s, h: g.2 * s, r: g.2 * s / 2.0 },
            color: accent(&look), amount: 0.1 * content, rim: 0.3 * content });
        let f = Frame {
            back: &self.panel.back,
            layers: &[(&self.panel.layer, (p.x as i32, p.y as i32), content)],
            a: shape,
            b: None,
            smooth: 0.0,
            refr_height: 22.0 * s,
            refr_amount: 36.0 * s,
            glyph: false,
            pills: &pills,
            alpha: o.clamp(0.0, 1.0),
            scale: s,
            look,
        };
        self.panel.present(&f);
    }

    // ---------- réglages ----------

    fn prefs_rr(&self) -> Rr {
        let s = self.scale();
        Rr { x: M * s, y: M * s, w: PREFS_W * s, h: row_tops().1 * s, r: PANEL_R * s }
    }

    /// Valeur affichée par la ligne `i` : case choisie, position du curseur ou interrupteur (0 / 1).
    fn row_value(&self, i: usize) -> f32 {
        let c = &self.cfg;
        match i {
            0 => c.theme as f32,
            1 => c.icon as f32,
            2 => c.compact as u8 as f32,
            3 => settings::limit_pos(c.limit_m),
            4 => c.refresh as f32,
            5 => c.french as u8 as f32,
            6 => c.alert as u8 as f32,
            _ => c.startup as u8 as f32,
        }
    }

    unsafe fn show_prefs(&mut self) {
        if self.prefs.shown {
            return;
        }
        if self.panel.shown {
            // tout de suite, sans animation : sinon il apparaîtrait dans la photo du fond des réglages
            self.panel.shown = false;
            self.panel.open.snap(0.0);
            ShowWindow(self.panel.hwnd, SW_HIDE);
        }
        let s = self.scale();
        self.prefs.place_corner(PREFS_W, row_tops().1, s);
        self.prefs.capture((10.0 * s) as usize, self.prefs_rr());
        for i in 0..ROWS.len() {
            let v = self.row_value(i);
            self.p_rows[i].snap(v);
        }
        self.prefs.open.snap(0.0);
        self.rebuild_prefs_layer();
        self.draw_prefs();
        self.prefs.show();
        self.kick();
    }

    /// Range le panneau (`true`) ou les réglages (`false`) ; la fenêtre se cache à la fin de l'animation.
    fn hide(&mut self, prefs: bool) {
        let w = if prefs { &mut self.prefs } else { &mut self.panel };
        if w.shown {
            w.open.target = 0.0;
            w.hidden_at = Instant::now();
        }
        self.kick();
    }

    fn limit_label(&self) -> String {
        match self.cfg.limit_m {
            0 => "Auto".into(),
            m => tokens::fmt_count(m as u64 * 1_000_000, self.fr()),
        }
    }

    unsafe fn rebuild_prefs_layer(&mut self) {
        let p = self.prefs_rr();
        let s = self.scale();
        let fr = self.fr();
        let r = |x: f32, y: f32, w: f32, h: f32| RECT {
            left: (x * s) as i32, top: (y * s) as i32, right: ((x + w) * s) as i32, bottom: ((y + h) * s) as i32,
        };
        let label = |t: &'static str, rect: RECT, level: f32, flags| win::Text { s: t, r: rect, size: 12.5 * s, weight: 500, flags, level };
        let limit = self.limit_label();
        let (tops, _) = row_tops();
        let mut texts = vec![win::Text { s: tr(fr, ("Settings", "Réglages")), r: r(22.0, 16.0, 296.0, 26.0), size: 17.0 * s,
            weight: 600, flags: DT_LEFT | DT_VCENTER, level: 1.0 }];
        for (i, (name, ctl)) in ROWS.iter().enumerate() {
            let y = tops[i];
            match ctl {
                Ctl::Seg(opts) => {
                    texts.push(label(tr(fr, *name), r(22.0, y, 296.0, 18.0), 0.62, DT_LEFT | DT_VCENTER));
                    let w = 300.0 / opts.len() as f32;
                    for (k, o) in opts.iter().enumerate() {
                        let mut t = label(tr(fr, *o), r(20.0 + k as f32 * w, y + 22.0, w, 34.0), 1.0, DT_CENTER | DT_VCENTER);
                        t.weight = 600;
                        texts.push(t);
                    }
                }
                Ctl::Slider => texts.push(label(tr(fr, *name), r(22.0, y, 150.0, 18.0), 0.62, DT_LEFT | DT_VCENTER)),
                Ctl::Toggle => {
                    let mut t = label(tr(fr, *name), r(22.0, y, 240.0, 40.0), 1.0, DT_LEFT | DT_VCENTER);
                    t.size = 13.5 * s;
                    texts.push(t);
                }
            }
        }
        texts.push(win::Text { s: &limit, r: r(150.0, tops[ROW_LIMIT], 170.0, 18.0), size: 12.5 * s, weight: 600,
            flags: DT_RIGHT | DT_VCENTER | DT_END_ELLIPSIS, level: 0.9 });
        let ink = look(self.cfg.theme, self.prefs.light_back).ink;
        self.prefs.layer = win::text_layer(p.w as usize, p.h as usize, &texts, ink);
    }

    unsafe fn draw_prefs(&self) {
        let o = self.prefs.open.x;
        let p = self.prefs_rr();
        let k = lerp(0.94, 1.0, o.clamp(0.0, 1.0));
        let shape = Rr { x: p.x + p.w * (1.0 - k), y: p.y + p.h * (1.0 - k), w: p.w * k, h: p.h * k, r: p.r };
        let content = smoothstep(0.4, 1.0, o);
        let look = look(self.cfg.theme, self.prefs.light_back);
        let ac = accent(&look);
        let s = self.scale();
        let at = |x: f32, y: f32, w: f32, h: f32, r: f32| Rr { x: p.x + x * s, y: p.y + y * s, w: w * s, h: h * s, r: r * s };
        let pill = |rr: Rr, color: Rgb, amount: f32, rim: f32| Pill { rr, color, amount: amount * content, rim: rim * content };
        let (tops, _) = row_tops();
        let mut pills = Vec::new();
        for (i, (_, ctl)) in ROWS.iter().enumerate() {
            let (y, v) = (tops[i], self.p_rows[i].x);
            match ctl {
                Ctl::Seg(opts) => {
                    let w = 300.0 / opts.len() as f32;
                    pills.push(pill(at(20.0, y + 22.0, 300.0, 34.0, 17.0), ac, 0.08, 0.0));
                    pills.push(pill(at(23.0 + v * w, y + 25.0, w - 6.0, 28.0, 14.0), ac, 0.2, 0.7));
                }
                Ctl::Slider => {
                    let knob = 20.0 + v.clamp(0.0, 1.0) * (300.0 - 22.0);
                    pills.push(pill(at(20.0, y + 30.0, 300.0, 6.0, 3.0), ac, 0.16, 0.0));
                    pills.push(pill(at(20.0, y + 30.0, knob - 20.0 + 11.0, 6.0, 3.0), ac, 0.6, 0.0));
                    pills.push(pill(at(knob, y + 22.0, 22.0, 22.0, 11.0), [1.0; 3], 1.0, 0.6));
                }
                Ctl::Toggle => {
                    pills.push(pill(at(272.0, y + 6.0, 48.0, 28.0, 14.0), ac, lerp(0.14, 0.6, v), 0.3));
                    pills.push(pill(at(275.0 + v * 20.0, y + 9.0, 22.0, 22.0, 11.0), [1.0; 3], 1.0, 0.6));
                }
            }
        }
        let f = Frame {
            back: &self.prefs.back,
            layers: &[(&self.prefs.layer, (p.x as i32, p.y as i32), content)],
            a: shape,
            b: None,
            smooth: 0.0,
            refr_height: 22.0 * s,
            refr_amount: 36.0 * s,
            glyph: false,
            pills: &pills,
            alpha: o.clamp(0.0, 1.0),
            scale: s,
            look,
        };
        self.prefs.present(&f);
    }

    /// Clic sur la ligne `i` à l'abscisse logique `x` (relative au panneau des réglages).
    unsafe fn prefs_click(&mut self, i: usize, x: f32) {
        match ROWS[i].1 {
            Ctl::Seg(opts) => {
                let k = (((x - 20.0) / (300.0 / opts.len() as f32)).max(0.0) as usize).min(opts.len() - 1);
                match i {
                    0 => self.cfg.theme = k,
                    1 => self.cfg.icon = k,
                    2 => self.cfg.compact = k == 1,
                    4 => {
                        self.cfg.refresh = k;
                        SetTimer(self.panel.hwnd, T_POLL, settings::REFRESH_SECS[k] * 1000, None);
                    }
                    _ => self.cfg.french = k == 1,
                }
            }
            Ctl::Slider => {
                self.dragging = true;
                return self.set_limit_from(x);
            }
            Ctl::Toggle if i == 6 => self.cfg.alert = !self.cfg.alert,
            Ctl::Toggle => {
                self.cfg.startup = !self.cfg.startup;
                settings::set_startup(self.cfg.startup);
            }
        }
        self.p_rows[i].target = self.row_value(i);
        settings::save(&self.cfg);
        match i {
            0 | 5 => self.rebuild_prefs_layer(), // couleurs ou langue : tous les textes changent
            1 => self.refresh_icon(),
            _ => {}
        }
        self.prefs_dirty();
    }

    unsafe fn set_limit_from(&mut self, lx: f32) {
        let t = ((lx - 31.0) / 278.0).clamp(0.0, 1.0);
        self.cfg.limit_m = settings::limit_from(t);
        self.p_rows[ROW_LIMIT].target = t;
        self.rebuild_prefs_layer();
        self.prefs_dirty();
    }

    fn prefs_dirty(&mut self) {
        // un ressort à peine déplacé suffit à relancer une image
        self.p_rows[0].v += 1e-3;
        self.kick();
    }

    // ---------- animation ----------

    /// Une image pour chaque fenêtre qui bouge ; s'arrête quand tout est au repos.
    unsafe fn tick(&mut self) {
        let now = Instant::now();
        let dt = (now - self.last).as_secs_f32().min(1.0 / 30.0);
        self.last = now;
        let panel_moving = !self.panel.open.rest();
        let prefs_moving = !self.prefs.open.rest() || !self.p_rows.iter().all(|s| s.rest());
        self.panel.open.step(dt);
        self.prefs.open.step(dt);
        self.p_rows.iter_mut().for_each(|s| s.step(dt));
        if self.panel.shown && panel_moving {
            self.draw_panel();
        }
        if self.prefs.shown && prefs_moving {
            self.draw_prefs();
        }
        for w in [&mut self.panel, &mut self.prefs] {
            if w.shown && w.open.target == 0.0 && w.open.rest() {
                w.shown = false;
                ShowWindow(w.hwnd, SW_HIDE);
            }
        }
        self.animating = panel_moving || prefs_moving;
    }
}

unsafe extern "system" fn panel_proc(h: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_TRAY => match lp as u32 {
            WM_LBUTTONUP => {
                app(|a| {
                    // le clic sur l'icône a déjà rangé le panneau (perte du focus) : ne pas le rouvrir aussitôt
                    if !a.panel.shown && a.panel.hidden_at.elapsed().as_millis() > 250 {
                        a.show_panel();
                    }
                });
            }
            WM_RBUTTONUP => {
                let fr = app(|a| a.fr()).unwrap_or(false);
                let items = [(ID_SETTINGS, tr(fr, ("Settings…", "Réglages…")), false), (0, "", false), (ID_QUIT, tr(fr, ("Quit", "Quitter")), false)];
                match win::menu(h, &items) {
                    ID_SETTINGS => {
                        app(|a| a.show_prefs());
                    }
                    ID_QUIT => {
                        DestroyWindow(h);
                    }
                    _ => {}
                }
            }
            _ => {}
        },
        WM_TIMER if wp == T_POLL => {
            app(|a| a.refresh());
        }
        // clic sur la roue dentée du panneau : les réglages
        WM_LBUTTONDOWN => {
            app(|a| {
                let (s, p) = (a.scale(), a.panel_rr());
                let (x, y) = (((lp & 0xFFFF) as i16 as f32 - p.x) / s, (((lp >> 16) & 0xFFFF) as i16 as f32 - p.y) / s);
                let g = GEAR;
                if (g.0..g.0 + g.2).contains(&x) && (g.1..g.1 + g.2).contains(&y) {
                    a.show_prefs();
                }
            });
        }
        WM_ACTIVATE if wp & 0xFFFF == WA_INACTIVE as usize => {
            app(|a| a.hide(false));
        }
        WM_KEYDOWN if wp == VK_ESCAPE as usize => {
            app(|a| a.hide(false));
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

unsafe extern "system" fn prefs_proc(h: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    // coordonnées logiques relatives au panneau des réglages
    let local = |lp: LPARAM| app(|a| {
        let (x, y) = ((lp & 0xFFFF) as i16 as f32, ((lp >> 16) & 0xFFFF) as i16 as f32);
        let (p, s) = (a.prefs_rr(), a.scale());
        ((x - p.x) / s, (y - p.y) / s)
    });
    match msg {
        WM_LBUTTONDOWN => {
            let Some((x, y)) = local(lp) else { return 0 };
            // ligne touchée : la zone du contrôle (segments, curseur) ou toute la ligne (interrupteurs)
            let (tops, _) = row_tops();
            let row = ROWS.iter().enumerate().position(|(i, (_, c))| {
                let t = tops[i];
                match c {
                    Ctl::Seg(_) => (t + 22.0..t + 56.0).contains(&y) && (20.0..320.0).contains(&x),
                    Ctl::Slider => (t + 14.0..t + 50.0).contains(&y) && (8.0..332.0).contains(&x),
                    Ctl::Toggle => (t..t + 40.0).contains(&y) && (0.0..PREFS_W).contains(&x),
                }
            });
            if let Some(i) = row {
                if matches!(ROWS[i].1, Ctl::Slider) {
                    SetCapture(h);
                }
                app(|a| a.prefs_click(i, x));
            }
            0
        }
        WM_MOUSEMOVE => {
            if let Some((x, _)) = local(lp) {
                app(|a| if a.dragging { a.set_limit_from(x) });
            }
            0
        }
        WM_LBUTTONUP => {
            if app(|a| std::mem::replace(&mut a.dragging, false)) == Some(true) {
                ReleaseCapture();
                app(|a| {
                    settings::save(&a.cfg);
                    a.refresh(); // nouvelle limite : nouveau %
                });
            }
            0
        }
        WM_KEYDOWN if wp == VK_ESCAPE as usize => {
            app(|a| a.hide(true));
            0
        }
        // clic ailleurs : les réglages se rangent
        WM_ACTIVATE if wp & 0xFFFF == WA_INACTIVE as usize => {
            app(|a| if !a.dragging { a.hide(true) });
            0
        }
        _ => DefWindowProcW(h, msg, wp, lp),
    }
}

unsafe fn class(name: &[u16], proc: WNDPROC, inst: HINSTANCE) {
    let wc = WNDCLASSW { lpfnWndProc: proc, hInstance: inst, lpszClassName: name.as_ptr(),
        hCursor: LoadCursorW(null_mut(), IDC_ARROW), ..zeroed() };
    RegisterClassW(&wc);
}

fn main() {
    if std::env::args().any(|a| a == "--probe") {
        return mouse::probe();
    }
    if std::env::args().any(|a| a == "--tokens") {
        let (t, fr) = (Instant::now(), false);
        return match tokens::read(None) {
            Some(u) => println!("{} / {} ({:.0} %), reset in {} — read in {:?}", tokens::fmt_count(u.used, fr),
                tokens::fmt_count(u.limit, fr), u.pct() * 100.0, tokens::fmt_left(u.left()), t.elapsed()),
            None => println!("no Claude Code data"),
        };
    }
    unsafe {
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let inst = GetModuleHandleW(null());
        let (panel_class, prefs_class) = (win::w("DESKORB"), win::w("DESKORB_SETTINGS"));
        class(&panel_class, Some(panel_proc), inst);
        class(&prefs_class, Some(prefs_proc), inst);
        let new_window = |c: &[u16]| CreateWindowExW(WS_EX_TOOLWINDOW | WS_EX_LAYERED | WS_EX_TOPMOST,
            c.as_ptr(), c.as_ptr(), WS_POPUP, 0, 0, 1, 1, null_mut(), null_mut(), inst, null());
        let (h, ph) = (new_window(&panel_class), new_window(&prefs_class));
        let cfg = settings::load();
        if cfg.startup {
            settings::set_startup(true);
        }
        let icon = win::text_icon("–");
        win::tray(h, WM_TRAY, NIM_ADD, icon, "Deskorb");
        APP.with_borrow_mut(|a| *a = Some(App {
            cfg, panel: Surface::new(h), prefs: Surface::new(ph),
            p_rows: ROWS.iter().map(|(_, c)| match c {
                Ctl::Seg(_) => Spring::new(0.0, 420.0, 0.78),
                Ctl::Slider => Spring::new(0.0, 900.0, 1.0),
                Ctl::Toggle => Spring::new(0.0, 520.0, 0.75),
            }).collect(),
            dragging: false, mouse: None, usage: None, alerted: false, icon,
            taskbar_created: RegisterWindowMessageW(win::w("TaskbarCreated").as_ptr()),
            animating: false, last: Instant::now(),
        }));
        app(|a| a.refresh());
        SetTimer(h, T_POLL, settings::REFRESH_SECS[cfg.refresh] * 1000, None);

        let mut msg: MSG = zeroed();
        loop {
            if app(|a| a.animating).unwrap_or(false) {
                // animation : une image par rafraîchissement de l'écran, sans jamais bloquer les messages
                while PeekMessageW(&mut msg, null_mut(), 0, 0, PM_REMOVE) != 0 {
                    if msg.message == WM_QUIT {
                        return;
                    }
                    TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
                app(|a| a.tick());
                DwmFlush();
            } else if GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            } else {
                return;
            }
        }
    }
}
