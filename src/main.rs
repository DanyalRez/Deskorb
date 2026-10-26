//! DESKORB : batterie de la Pulsar TenZ et limites d'usage Claude (session, semaine), dans la zone de notification.
//! L'icône affiche l'état en direct (mini-widget) ; un clic ouvre un panneau Liquid Glass au-dessus de la barre,
//! le clic droit ouvre les réglages. Deux fenêtres en couches dessinées par le moteur de DESKTOUCH.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod desk;
mod mouse;
#[allow(dead_code)] // copié de DESKTOUCH, tout n'est pas utilisé
mod render;
mod settings;
mod tokens;
mod usage;
mod win;

use render::{smoothstep, Back, Frame, Layer, Look, Pill, Rgb, Rr, Spring};
use settings::Config;
use std::{
    cell::RefCell,
    mem::zeroed,
    ptr::{null, null_mut},
    sync::Mutex,
    time::Instant,
};
use windows_sys::Win32::{
    Foundation::*,
    Graphics::{Dwm::DwmFlush, Gdi::*},
    System::{LibraryLoader::GetModuleHandleW, Threading::{GetCurrentProcess, SetProcessWorkingSetSize}},
    UI::{HiDpi::*, Input::KeyboardAndMouse::*, Shell::*, WindowsAndMessaging::*},
};

const WM_TRAY: u32 = WM_APP + 1;
const WM_USAGE: u32 = WM_APP + 2; // réponse d'Anthropic arrivée (fil réseau → fil de l'interface)
const WM_LOCAL: u32 = WM_APP + 3; // transcriptions locales relues (fil de lecture → fil de l'interface)
const T_POLL: usize = 1;
const ID_SETTINGS: usize = 1;
const ID_QUIT: usize = 2;
const ID_NOTIFY: usize = 3;
// pixels logiques (96 dpi) ; la marge autour des formes laisse la place à l'ombre
const M: f32 = 30.0;
const GAP: f32 = 4.0; // entre une fenêtre et la barre des tâches
const PANEL_W: f32 = 320.0;
const PANEL_R: f32 = 26.0;
const PREFS_W: f32 = 340.0;
const PREFS_TOP: f32 = 54.0; // sous le titre
const LOW_BATTERY: u8 = 20;
// Anthropic limite la fréquence (429 vu à 15 s) : un relevé toutes les 2 min, espacé jusqu'à 30 min si refusé
const USAGE_EVERY: u64 = 120;
const USAGE_MAX_BACKOFF: u64 = 1800;
const HEADER: f32 = 30.0; // en-tête du panneau : nom de l'app et bouton des réglages
const GEAR: (f32, f32, f32) = (PANEL_W - 44.0, 8.0, 32.0); // x, y, côté (relatif au panneau)
const BACK: (f32, f32, f32) = (12.0, 8.0, 32.0); // retour de la vue Usage Pro
const PRO_ROWS: usize = 5;
const PRO_H: f32 = 344.0; // vue Usage Pro, sous l'en-tête
const CHART: (f32, f32) = (238.0, 54.0); // graphique 7 jours : haut des barres, hauteur max (relatifs à l'en-tête)

/// Rend à Windows les pages de mémoire inutilisées (pile HTTPS après un relevé, images libérées).
fn trim() {
    unsafe { SetProcessWorkingSetSize(GetCurrentProcess(), usize::MAX, usize::MAX) };
}

/// Texte anglais ou français.
type Tr = (&'static str, &'static str);

fn tr(fr: bool, t: Tr) -> &'static str {
    if fr { t.1 } else { t.0 }
}

/// Contrôle d'une ligne des réglages.
enum Ctl {
    Seg(&'static [Tr]),
    Toggle,
}

const ROWS: [(Tr, Ctl); 9] = [
    (("Theme", "Thème"), Ctl::Seg(&[("Liquid Glass", "Liquid Glass"), ("Dark", "Sombre"), ("Light", "Clair")])),
    (("Tray icon", "Icône"), Ctl::Seg(&[("Orbit", "Orbite"), ("Gauges", "Jauges"), ("Percent", "Pourcentage")])),
    (("Panel", "Panneau"), Ctl::Seg(&[("Detailed", "Détaillé"), ("Compact", "Compact")])),
    (("Refresh every", "Actualiser toutes les"), Ctl::Seg(&[("5 s", "5 s"), ("15 s", "15 s"), ("1 min", "1 min")])),
    (("Language", "Langue"), Ctl::Seg(&[("English", "English"), ("Français", "Français")])),
    (("Low battery alert", "Alerte batterie faible"), Ctl::Toggle),
    (("Claude notifications", "Notifications Claude"), Ctl::Toggle),
    (("Desktop widget", "Widget de bureau"), Ctl::Toggle),
    (("Launch at startup", "Lancer au démarrage"), Ctl::Toggle),
];

fn row_height(c: &Ctl) -> f32 {
    match c {
        Ctl::Seg(_) => 66.0,
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

/// Jauge Claude : bleu, orange au-dessus de 75 %, rouge au-dessus de 90 %.
fn gauge_color(pct: f32, normal: Rgb) -> Rgb {
    if pct >= 0.9 { [1.0, 0.36, 0.36] } else if pct >= 0.75 { [1.0, 0.62, 0.25] } else { normal }
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
        self.place(RECT { left: right - ww, top: bottom - wh, right, bottom });
    }

    /// Fenêtre à cette place ; la bitmap est refaite seulement si la taille change.
    unsafe fn place(&mut self, r: RECT) {
        let old = self.size();
        self.win = r;
        let (ww, wh) = (r.right - r.left, r.bottom - r.top);
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

    /// Fenêtre rangée : on rend ses images (fond flouté, calque, bitmap), soit ~10 Mo pour les réglages.
    unsafe fn release(&mut self) {
        self.shown = false;
        ShowWindow(self.hwnd, SW_HIDE);
        DeleteDC(self.dc); // libère d'abord la bitmap sélectionnée dedans
        self.dc = CreateCompatibleDC(null_mut());
        if !self.dib.is_null() {
            DeleteObject(self.dib);
        }
        (self.dib, self.bits, self.win) = (null_mut(), null_mut(), zeroed());
        (self.back, self.layer) = (Back::empty(), Layer::empty());
        trim();
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
    desk: desk::Desk,
    p_rows: Vec<Spring>, // une par ligne : case choisie ou interrupteur
    mouse: Option<mouse::Mouse>,
    usage: Option<tokens::Usage>,     // dernières valeurs reçues, gardées si le réseau tombe
    usage_err: Option<tokens::Error>, // dernier échec, affiché sous les valeurs
    fetching: bool,
    fetched_at: Option<Instant>, // dernière demande à Anthropic
    usage_every: u64,            // secondes entre deux demandes, doublé à chaque 429
    reading: bool,               // relecture des transcriptions locales en cours
    pro: bool,                   // le panneau montre la vue Usage Pro
    pro_all: Vec<(String, u64, f64)>, // projets de la fenêtre, du plus coûteux au moins : nom, tokens, coût $
    pro_days: [(u64, f64); 7],        // 7 derniers jours locaux (le dernier = aujourd'hui) : tokens, coût $
    forecast: usage::Forecast,        // prévision de la session, recalculée quand les données changent
    alerted_pct: (Option<std::time::SystemTime>, u8), // fenêtre (heure de reset) et dernier seuil notifié : 1 = 75 %, 2 = 90 %
    notified_waits: Vec<u64>,         // tours déjà signalés « attend ta réponse »
    alerted: bool,
    icon: HICON,
    taskbar_created: u32,
    animating: bool,
    last: Instant,
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}
static USAGE: Mutex<Option<Result<tokens::Usage, tokens::Error>>> = Mutex::new(None);

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

    fn pct(w: Option<tokens::Window>) -> String {
        w.map_or("—".into(), |w| format!("{:.0} %", w.pct * 100.0))
    }

    /// Prévision de la session : « Limite vers 21:40 · reset 22:32 » quand elle tomberait avant le reset,
    /// sinon la ligne habituelle, complétée de « rythme OK » s'il y a de l'activité.
    fn session_line(&mut self) -> String {
        let fr = self.fr();
        let session = self.usage.map(|u| u.session);
        let (now, tz) = (usage::now(), win::utc_offset());
        let reset = session.and_then(|w| w.reset_at).and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs() as i64);
        if let (Some(w), Some(reset)) = (session, reset) {
            if let Some(f) = usage::READER.try_lock().ok().and_then(|g| g.as_ref().map(|r| r.forecast(w.pct, reset - 5 * 3600, reset, now))) {
                self.forecast = f;
            }
            if let usage::Forecast::LimitAt(t) = self.forecast {
                return format!("{} {} · reset {}", tr(fr, ("Limit around", "Limite vers")), usage::clock(t, tz), usage::clock(reset, tz));
            }
        }
        let line = self.window_line(session, ("Session", "Session"));
        if self.forecast == usage::Forecast::Safe && self.usage_err.is_none() {
            format!("{line} · {}", tr(fr, ("pace OK", "rythme OK")))
        } else {
            line
        }
    }

    /// « Session · reset dans 3 h 03 », ou pourquoi il n'y a pas de chiffres.
    fn window_line(&self, w: Option<tokens::Window>, name: Tr) -> String {
        let fr = self.fr();
        match (self.usage_err, w) {
            (Some(tokens::Error::SignedOut), _) => tr(fr, ("Sign in: claude auth login", "Connecte-toi : claude auth login")).into(),
            (Some(tokens::Error::Offline), None) => tr(fr, ("Offline", "Hors ligne")).into(),
            (Some(tokens::Error::RateLimited), None) => tr(fr, ("Too many requests, retrying soon", "Trop de demandes, nouvel essai bientôt")).into(),
            (_, None) => "…".into(),
            (err, Some(w)) => {
                let reset = w.left().map_or(String::new(), |d| format!(" · {} {}", tr(fr, ("resets in", "reset dans")), tokens::fmt_left(d, fr)));
                // 429 : les valeurs ont au plus quelques minutes, inutile d'alarmer
                let offline = if err == Some(tokens::Error::Offline) { tr(fr, (" · offline", " · hors ligne")) } else { "" };
                format!("{}{reset}{offline}", tr(fr, name))
            }
        }
    }

    fn tip(&self) -> String {
        let bat = self.battery().map_or("—".into(), |b| format!("{b} %"));
        let (s, w) = (Self::pct(self.usage.map(|u| u.session)), Self::pct(self.usage.map(|u| u.weekly)));
        format!("Pulsar : {bat}\nClaude : {s} · {} {w}", tr(self.fr(), ("week", "semaine")))
    }

    unsafe fn refresh(&mut self) {
        let last = self.battery();
        self.mouse = mouse::read();
        if let Some(m) = self.mouse.as_mut().filter(|m| m.battery.is_none()) {
            m.battery = last; // en veille : on garde la dernière valeur lue
        }
        self.fetch_usage(self.usage_every);
        self.read_local();
        self.check_low_battery();
        self.refresh_icon();
        if self.panel.shown {
            self.rebuild_panel_layer();
            self.kick();
        }
    }

    /// Demande l'usage à Anthropic dans un fil à part, si la dernière demande date d'au moins `min_age` secondes
    /// (ou de l'espacement imposé après un 429, qui prime).
    fn fetch_usage(&mut self, min_age: u64) {
        let wait = if self.usage_every > USAGE_EVERY { self.usage_every } else { min_age };
        if self.fetching || self.fetched_at.is_some_and(|t| t.elapsed().as_secs() < wait) {
            return;
        }
        self.fetching = true;
        self.fetched_at = Some(Instant::now());
        let h = self.panel.hwnd as isize;
        std::thread::spawn(move || {
            *USAGE.lock().unwrap() = Some(tokens::read());
            unsafe { PostMessageW(h as HWND, WM_USAGE, 0, 0) };
        });
    }

    /// Relit les transcriptions de Claude Code dans un fil à part (seuls les nouveaux octets).
    fn read_local(&mut self) {
        if self.reading {
            return;
        }
        self.reading = true;
        let h = self.panel.hwnd as isize;
        std::thread::spawn(move || {
            usage::READER.lock().unwrap().get_or_insert_with(Default::default).update(usage::now());
            unsafe { PostMessageW(h as HWND, WM_LOCAL, 0, 0) };
        });
    }

    unsafe fn local_arrived(&mut self) {
        self.reading = false;
        self.check_waiting();
        if self.panel.shown {
            self.rebuild_panel_layer();
            self.kick();
        } else if !self.prefs.shown {
            trim();
        }
    }

    /// Début de la fenêtre de 5 h : celui d'Anthropic (reset − 5 h), sinon déduit des transcriptions.
    fn window_start(&self, reader: &usage::Reader) -> i64 {
        let api = self.usage.and_then(|u| u.session.reset_at)
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs() as i64 - 5 * 3600);
        api.unwrap_or_else(|| reader.local_block_start(usage::now()))
    }

    /// Bascule entre le panneau et la vue Usage Pro (le panneau change de taille : on le rouvre).
    unsafe fn toggle_pro(&mut self) {
        self.pro = !self.pro;
        self.panel.open.snap(0.0);
        self.panel.release();
        self.show_panel();
    }

    /// Réponse d'Anthropic : nouvelles valeurs, ou on garde les anciennes avec la raison de l'échec.
    unsafe fn usage_arrived(&mut self) {
        self.fetching = false;
        match USAGE.lock().unwrap().take() {
            Some(Ok(u)) => {
                (self.usage, self.usage_err, self.usage_every) = (Some(u), None, USAGE_EVERY);
                self.check_thresholds(u.session);
            }
            Some(Err(e)) => {
                if e == tokens::Error::RateLimited {
                    self.usage_every = (self.usage_every * 2).min(USAGE_MAX_BACKOFF);
                }
                self.usage_err = Some(e);
            }
            None => return,
        }
        if !self.panel.shown && !self.prefs.shown {
            trim(); // pas pendant qu'une fenêtre est ouverte : ses images resteraient à recharger
        }
        self.refresh_icon();
        if self.panel.shown && !self.pro {
            self.rebuild_panel_layer();
            self.kick();
        }
    }

    /// Une notification au passage de 75 % puis de 90 % de la session, une seule fois par fenêtre de 5 h.
    unsafe fn check_thresholds(&mut self, session: tokens::Window) {
        if self.alerted_pct.0 != session.reset_at {
            self.alerted_pct = (session.reset_at, 0);
        }
        let level = if session.pct >= 0.9 { 2 } else if session.pct >= 0.75 { 1 } else { 0 };
        if level > self.alerted_pct.1 {
            self.alerted_pct.1 = level;
            if self.cfg.notify {
                let fr = self.fr();
                let reset = session.left().map_or(String::new(), |d| format!(" · {} {}", tr(fr, ("resets in", "reset dans")), tokens::fmt_left(d, fr)));
                win::notify(self.panel.hwnd, &format!("Claude : {:.0} %", session.pct * 100.0),
                    &format!("{}{reset}", tr(fr, ("of your 5-hour session used", "de ta session de 5 h utilisés"))));
            }
        }
    }

    /// « Claude attend ta réponse » : une notification par tour terminé, après `wait_secs` sans réponse.
    unsafe fn check_waiting(&mut self) {
        let waits = usage::READER.try_lock().ok().and_then(|g| g.as_ref().map(|r| {
            r.waiting(usage::now(), self.cfg.wait_secs as i64).into_iter().map(|w| (w.key, r.projects[w.project as usize].clone())).collect::<Vec<_>>()
        }));
        for (key, project) in waits.unwrap_or_default() {
            if self.notified_waits.contains(&key) {
                continue;
            }
            if self.notified_waits.len() > 64 {
                self.notified_waits.remove(0);
            }
            self.notified_waits.push(key);
            if self.cfg.notify {
                let fr = self.fr();
                win::notify(self.panel.hwnd, tr(fr, ("Claude is waiting for you", "Claude attend ta réponse")), &project);
            }
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
        let tok = self.usage.map(|u| u.session.pct);
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
        if self.desk.surf.shown {
            self.desk.draw(self.scale(), self.cfg.theme, self.desk_value());
        }
    }

    /// Valeur du logo du widget : la batterie, sinon la session Claude.
    fn desk_value(&self) -> Option<f32> {
        self.battery().map(|b| b as f32 / 100.0).or(self.usage.map(|u| u.session.pct))
    }

    /// Affiche ou retire le widget de bureau selon le réglage.
    unsafe fn apply_desk(&mut self) {
        if self.cfg.desk {
            let (s, theme, v) = (self.scale(), self.cfg.theme, self.desk_value());
            self.desk.show(s, theme, v);
        } else {
            self.desk.hide();
        }
    }

    /// Fin d'un déplacement du widget : `left`, `top` = nouvelle place de sa fenêtre.
    unsafe fn desk_moved(&mut self, left: i32, top: i32) {
        let m = (M * self.scale()) as i32;
        self.desk.pos = (left + m, top + m);
        self.cfg.desk_pos = Some(self.desk.pos);
        settings::save(&self.cfg);
        self.apply_desk(); // nouvelle photo du fond à la nouvelle place
    }

    // ---------- panneau ----------

    fn panel_h(&self) -> f32 {
        HEADER + if self.pro { PRO_H } else if self.cfg.compact { 116.0 } else { 236.0 }
    }

    fn panel_rr(&self) -> Rr {
        let s = self.scale();
        Rr { x: M * s, y: M * s, w: PANEL_W * s, h: self.panel_h() * s, r: PANEL_R * s }
    }

    unsafe fn show_panel(&mut self) {
        self.fetch_usage(30); // chiffres frais à l'ouverture, sauf si Anthropic demande d'espacer
        let s = self.scale();
        self.panel.place_corner(PANEL_W, self.panel_h(), s);
        self.panel.capture((16.0 * s) as usize, self.panel_rr());
        self.rebuild_panel_layer();
        self.panel.open.snap(0.0);
        self.draw_panel();
        self.panel.show();
        self.kick();
    }

    /// Vue Usage Pro : les projets qui ont coûté le plus dans la fenêtre de 5 h en cours.
    unsafe fn rebuild_pro_layer(&mut self) {
        let s = self.scale();
        let p = self.panel_rr();
        let fr = self.fr();
        let px = |v: f32| (v * s) as i32;
        let rect = |x: f32, y: f32, w: f32, h: f32| RECT { left: px(x), top: px(y), right: px(x + w), bottom: px(y + h) };
        let t = |txt, r, size: f32, weight, flags, level| win::Text { s: txt, r, size: size * s, weight, flags, level };
        // try_lock : pendant la première lecture (~1 s), on garde les derniers chiffres au lieu de figer l'interface
        let (now, tz) = (usage::now(), win::utc_offset());
        let fresh = usage::READER.try_lock().ok().and_then(|g| g.as_ref().map(|r| {
            let all = r.top_projects(self.window_start(r), usize::MAX).into_iter().map(|(n, tk, c)| (n.to_string(), tk, c)).collect();
            (all, r.daily(now, tz))
        }));
        let loaded = fresh.is_some() || !self.pro_all.is_empty();
        if let Some((all, days)) = fresh {
            (self.pro_all, self.pro_days) = (all, days);
        }
        let (week_tokens, week_cost) = self.pro_days.iter().fold((0, 0.0), |a, d| (a.0 + d.0, a.1 + d.1));
        let week_total = format!("{} · {}", usage::fmt_tokens(week_tokens, fr), usage::fmt_cost(week_cost, fr));
        let all = &self.pro_all;
        let (tokens, cost) = all.iter().fold((0, 0.0), |a, r| (a.0 + r.1, a.1 + r.2));
        let h = HEADER;
        let total = usage::fmt_cost(cost, fr);
        let footer = match (loaded, all.len()) {
            (false, _) => tr(fr, ("Reading Claude Code history…", "Lecture de l'historique Claude Code…")).to_string(),
            (true, 0) => tr(fr, ("No Claude Code activity in this window", "Aucune activité Claude Code dans cette fenêtre")).to_string(),
            (true, n) => format!("{n} {} · {} tokens · {}", tr(fr, ("projects", "projets")), usage::fmt_tokens(tokens, fr),
                tr(fr, ("API-equivalent cost", "coût équivalent API"))),
        };
        let rows = &self.pro_all[..self.pro_all.len().min(PRO_ROWS)];
        let cells: Vec<(String, String)> = rows.iter().map(|r| (usage::fmt_tokens(r.1, fr), usage::fmt_cost(r.2, fr))).collect();
        let (b, g) = (BACK, GEAR);
        let mut texts = vec![
            t("\u{E76B}", rect(b.0, b.1, b.2, b.2), 12.0, 400, DT_CENTER | DT_VCENTER, 0.75),
            t("USAGE PRO", rect(50.0, 12.0, 200.0, 20.0), 11.0, 700, DT_LEFT | DT_VCENTER, 0.55),
            t("\u{E713}", rect(g.0, g.1, g.2, g.2), 14.0, 400, DT_CENTER | DT_VCENTER, 0.75),
            t(tr(fr, ("Projects · this 5 h window", "Projets · fenêtre de 5 h")), rect(20.0, h + 12.0, 200.0, 22.0), 13.0, 600, DT_LEFT | DT_VCENTER, 1.0),
            t(&total, rect(200.0, h + 8.0, 100.0, 28.0), 18.0, 600, DT_RIGHT | DT_VCENTER, 1.0),
            t(&footer, rect(20.0, h + PRO_H - 30.0, 280.0, 18.0), 11.5, 400, DT_LEFT | DT_VCENTER | DT_END_ELLIPSIS, 0.6),
            t(tr(fr, ("Last 7 days", "7 derniers jours")), rect(20.0, h + 206.0, 140.0, 22.0), 13.0, 600, DT_LEFT | DT_VCENTER, 1.0),
            t(&week_total, rect(140.0, h + 206.0, 160.0, 22.0), 12.5, 600, DT_RIGHT | DT_VCENTER, 0.85),
        ];
        // initiale du jour sous chaque barre ; aujourd'hui plus marqué
        for i in 0..7 {
            let x = 20.0 + i as f32 * 40.0;
            let level = if i == 6 { 0.95 } else { 0.5 };
            texts.push(t(usage::weekday_letter(now, tz, 6 - i as i64, fr), rect(x, h + CHART.0 + CHART.1 + 4.0, 40.0, 16.0), 11.0, 600, DT_CENTER | DT_VCENTER, level));
        }
        for (i, (row, (tk, c))) in rows.iter().zip(&cells).enumerate() {
            let y = h + 44.0 + i as f32 * 30.0;
            texts.push(t(&row.0, rect(20.0, y, 150.0, 20.0), 13.0, 600, DT_LEFT | DT_VCENTER | DT_END_ELLIPSIS, 0.95));
            texts.push(t(tk, rect(170.0, y, 60.0, 20.0), 11.5, 400, DT_RIGHT | DT_VCENTER, 0.6));
            texts.push(t(c, rect(232.0, y, 68.0, 20.0), 13.0, 600, DT_RIGHT | DT_VCENTER, 0.95));
        }
        let ink = look(self.cfg.theme, self.panel.light_back).ink;
        self.panel.layer = win::text_layer(p.w as usize, p.h as usize, &texts, ink);
    }

    /// Lignes du panneau : souris, puis Claude (session et, en mode détaillé, semaine).
    unsafe fn rebuild_panel_layer(&mut self) {
        if self.pro {
            return self.rebuild_pro_layer();
        }
        let s = self.scale();
        let p = self.panel_rr();
        let px = |v: f32| (v * s) as i32;
        let rect = |x: f32, y: f32, w: f32, h: f32| RECT { left: px(x), top: px(y), right: px(x + w), bottom: px(y + h) };
        let t = |txt, r, size: f32, weight, flags, level| win::Text { s: txt, r, size: size * s, weight, flags, level };
        let bat_txt = self.battery().map_or("—".into(), |b| format!("{b} %"));
        let fr = self.fr();
        let (session, weekly) = (self.usage.map(|u| u.session), self.usage.map(|u| u.weekly));
        let (session_pct, weekly_pct) = (Self::pct(session), Self::pct(weekly));
        let session_line = self.session_line();
        let weekly_line = self.window_line(weekly, ("All models", "Tous les modèles"));
        let mouse_line = self.mouse_line();
        let (h, g) = (HEADER, GEAR);
        let tokens_y = h + if self.cfg.compact { 66.0 } else { 104.0 };
        let mut texts = vec![
            t("DESKORB", rect(20.0, 12.0, 200.0, 20.0), 11.0, 700, DT_LEFT | DT_VCENTER, 0.55),
            t("\u{E713}", rect(g.0, g.1, g.2, g.2), 14.0, 400, DT_CENTER | DT_VCENTER, 0.75),
            t("Pulsar TenZ", rect(20.0, h + 16.0, 200.0, 22.0), 14.0, 600, DT_LEFT | DT_VCENTER, 1.0),
            t(&bat_txt, rect(180.0, h + 12.0, 120.0, 30.0), 22.0, 600, DT_RIGHT | DT_VCENTER, 1.0),
            t("Claude", rect(20.0, tokens_y, 200.0, 22.0), 14.0, 600, DT_LEFT | DT_VCENTER, 1.0),
            // chevron : la section s'ouvre sur la vue Usage Pro
            t("\u{E76C}", rect(74.0, tokens_y + 1.0, 16.0, 20.0), 10.0, 400, DT_CENTER | DT_VCENTER, 0.5),
            t(&session_pct, rect(180.0, tokens_y - 4.0, 120.0, 30.0), 22.0, 600, DT_RIGHT | DT_VCENTER, 1.0),
        ];
        if !self.cfg.compact {
            texts.push(t(&mouse_line, rect(20.0, h + 40.0, 280.0, 18.0), 12.0, 400, DT_LEFT | DT_VCENTER, 0.7));
            texts.push(t(&session_line, rect(20.0, h + 146.0, 280.0, 18.0), 12.0, 400, DT_LEFT | DT_VCENTER, 0.7));
            texts.push(t(tr(fr, ("Weekly", "Hebdomadaire")), rect(20.0, h + 172.0, 200.0, 20.0), 12.5, 600, DT_LEFT | DT_VCENTER, 0.9));
            texts.push(t(&weekly_pct, rect(180.0, h + 170.0, 120.0, 24.0), 15.0, 600, DT_RIGHT | DT_VCENTER, 0.9));
            texts.push(t(&weekly_line, rect(20.0, h + 208.0, 280.0, 18.0), 12.0, 400, DT_LEFT | DT_VCENTER, 0.7));
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
        let (bar_bat, bar_tok) = if self.cfg.compact { (HEADER + 46.0, HEADER + 94.0) } else { (HEADER + 66.0, HEADER + 132.0) };
        let bar = |y: f32, v: f32, color: Rgb, th: f32| {
            let rail = Rr { x: p.x + 20.0 * s, y: p.y + y * s, w: 280.0 * s, h: th * s, r: th * s / 2.0 };
            let fill = Rr { w: (rail.w * v.clamp(0.0, 1.0)).max(rail.h), ..rail };
            [Pill { rr: rail, color: look.ink, amount: 0.12 * content, rim: 0.0 },
                Pill { rr: fill, color, amount: if v > 0.0 { 0.9 * content } else { 0.0 }, rim: 0.4 * content }]
        };
        let mut pills: Vec<Pill> = Vec::new();
        let button = |(x, y, w): (f32, f32, f32)| Pill { rr: Rr { x: p.x + x * s, y: p.y + y * s, w: w * s, h: w * s, r: w * s / 2.0 },
            color: accent(&look), amount: 0.1 * content, rim: 0.3 * content };
        if self.pro {
            // une barre fine par projet, à l'échelle du plus coûteux
            let top = self.pro_all.first().map_or(0.0, |r| r.2).max(1e-9);
            for (i, r) in self.pro_all.iter().take(PRO_ROWS).enumerate() {
                pills.extend(bar(HEADER + 64.0 + i as f32 * 30.0, (r.2 / top) as f32, [0.45, 0.6, 1.0], 3.0));
            }
            // 7 jours : une barre par jour à l'échelle du plus cher, aujourd'hui en plein
            let max = self.pro_days.iter().map(|d| d.1).fold(0.0, f64::max).max(1e-9);
            for (i, d) in self.pro_days.iter().enumerate() {
                let hgt = if d.0 == 0 { 3.0 } else { (CHART.1 * (d.1 / max) as f32).max(5.0) };
                let rr = Rr { x: p.x + (20.0 + i as f32 * 40.0 + 9.0) * s, y: p.y + (HEADER + CHART.0 + CHART.1 - hgt) * s,
                    w: 22.0 * s, h: hgt * s, r: 5.0_f32.min(hgt / 2.0) * s };
                let amount = if i == 6 { 0.9 } else if d.0 == 0 { 0.12 } else { 0.55 };
                pills.push(Pill { rr, color: [0.45, 0.6, 1.0], amount: amount * content, rim: 0.35 * content });
            }
            pills.push(button(BACK));
        } else {
            let bat = self.battery().map_or(0.0, |b| b as f32 / 100.0);
            // batterie : vert, orange sous 20 %
            let bat_color = if bat > 0.0 && bat <= LOW_BATTERY as f32 / 100.0 { [1.0, 0.62, 0.25] } else { [0.35, 0.85, 0.45] };
            let (session, weekly) = (self.usage.map_or(0.0, |u| u.session.pct), self.usage.map_or(0.0, |u| u.weekly.pct));
            pills.extend(bar(bar_bat, bat, bat_color, 8.0).into_iter().chain(bar(bar_tok, session, gauge_color(session, [0.45, 0.6, 1.0]), 8.0)));
            if !self.cfg.compact {
                pills.extend(bar(HEADER + 196.0, weekly, gauge_color(weekly, [0.7, 0.55, 1.0]), 6.0));
            }
        }
        // bouton des réglages : une pastille discrète sous la roue dentée
        pills.push(button(GEAR));
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

    /// Valeur affichée par la ligne `i` : case choisie ou interrupteur (0 / 1).
    fn row_value(&self, i: usize) -> f32 {
        let c = &self.cfg;
        match i {
            0 => c.theme as f32,
            1 => c.icon as f32,
            2 => c.compact as u8 as f32,
            3 => c.refresh as f32,
            4 => c.french as u8 as f32,
            5 => c.alert as u8 as f32,
            6 => c.notify as u8 as f32,
            7 => c.desk as u8 as f32,
            _ => c.startup as u8 as f32,
        }
    }

    unsafe fn show_prefs(&mut self) {
        if self.prefs.shown {
            return;
        }
        if self.panel.shown {
            // tout de suite, sans animation : sinon il apparaîtrait dans la photo du fond des réglages
            self.panel.open.snap(0.0);
            self.panel.release();
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

    unsafe fn rebuild_prefs_layer(&mut self) {
        let p = self.prefs_rr();
        let s = self.scale();
        let fr = self.fr();
        let r = |x: f32, y: f32, w: f32, h: f32| RECT {
            left: (x * s) as i32, top: (y * s) as i32, right: ((x + w) * s) as i32, bottom: ((y + h) * s) as i32,
        };
        let label = |t: &'static str, rect: RECT, level: f32, flags| win::Text { s: t, r: rect, size: 12.5 * s, weight: 500, flags, level };
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
                Ctl::Toggle => {
                    let mut t = label(tr(fr, *name), r(22.0, y, 240.0, 40.0), 1.0, DT_LEFT | DT_VCENTER);
                    t.size = 13.5 * s;
                    texts.push(t);
                }
            }
        }
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
                    3 => {
                        self.cfg.refresh = k;
                        SetTimer(self.panel.hwnd, T_POLL, settings::REFRESH_SECS[k] * 1000, None);
                    }
                    _ => self.cfg.french = k == 1,
                }
            }
            Ctl::Toggle if i == 5 => self.cfg.alert = !self.cfg.alert,
            Ctl::Toggle if i == 6 => self.cfg.notify = !self.cfg.notify,
            Ctl::Toggle if i == 7 => {
                self.cfg.desk = !self.cfg.desk;
                self.apply_desk();
            }
            Ctl::Toggle => {
                self.cfg.startup = !self.cfg.startup;
                settings::set_startup(self.cfg.startup);
            }
        }
        self.p_rows[i].target = self.row_value(i);
        settings::save(&self.cfg);
        match i {
            0 => {
                self.rebuild_prefs_layer(); // nouvelles couleurs de texte
                if self.desk.surf.shown {
                    self.desk.draw(self.scale(), self.cfg.theme, self.desk_value());
                }
            }
            4 => self.rebuild_prefs_layer(), // langue : tous les textes changent
            1 => self.refresh_icon(),
            _ => {}
        }
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
                w.release();
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
                let notify = app(|a| a.cfg.notify).unwrap_or(true);
                let items = [(ID_SETTINGS, tr(fr, ("Settings…", "Réglages…")), false),
                    (ID_NOTIFY, tr(fr, ("Claude notifications", "Notifications Claude")), notify),
                    (0, "", false), (ID_QUIT, tr(fr, ("Quit", "Quitter")), false)];
                match win::menu(h, &items) {
                    ID_SETTINGS => {
                        app(|a| a.show_prefs());
                    }
                    ID_NOTIFY => {
                        app(|a| {
                            a.cfg.notify = !a.cfg.notify;
                            settings::save(&a.cfg);
                        });
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
        WM_USAGE => {
            app(|a| a.usage_arrived());
        }
        WM_LOCAL => {
            app(|a| a.local_arrived());
        }
        // roue dentée : réglages ; section Claude : vue Usage Pro ; en-tête de la vue : retour
        WM_LBUTTONDOWN => {
            app(|a| {
                let (s, p) = (a.scale(), a.panel_rr());
                let (x, y) = (((lp & 0xFFFF) as i16 as f32 - p.x) / s, (((lp >> 16) & 0xFFFF) as i16 as f32 - p.y) / s);
                let inside = |(bx, by, bw): (f32, f32, f32)| (bx..bx + bw).contains(&x) && (by..by + bw).contains(&y);
                let claude_top = HEADER + if a.cfg.compact { 56.0 } else { 94.0 };
                if inside(GEAR) {
                    a.show_prefs();
                } else if (a.pro && y < HEADER + 4.0) || (!a.pro && y >= claude_top) {
                    a.toggle_pro();
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
            // ligne touchée : la zone des segments, ou toute la ligne pour un interrupteur
            let (tops, _) = row_tops();
            let row = ROWS.iter().enumerate().position(|(i, (_, c))| {
                let t = tops[i];
                match c {
                    Ctl::Seg(_) => (t + 22.0..t + 56.0).contains(&y) && (20.0..320.0).contains(&x),
                    Ctl::Toggle => (t..t + 40.0).contains(&y) && (0.0..PREFS_W).contains(&x),
                }
            });
            if let Some(i) = row {
                app(|a| a.prefs_click(i, x));
            }
            0
        }
        WM_KEYDOWN if wp == VK_ESCAPE as usize => {
            app(|a| a.hide(true));
            0
        }
        // clic ailleurs : les réglages se rangent
        WM_ACTIVATE if wp & 0xFFFF == WA_INACTIVE as usize => {
            app(|a| a.hide(true));
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
        let t = Instant::now();
        let show = |w: tokens::Window| format!("{:.0} % (resets in {})", w.pct * 100.0,
            w.left().map_or("—".into(), |d| tokens::fmt_left(d, false)));
        return match tokens::read() {
            Ok(u) => println!("session {} · weekly {} — {:?}", show(u.session), show(u.weekly), t.elapsed()),
            Err(e) => println!("{e:?}"),
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
        // widget de bureau : jamais au premier plan, jamais activé
        let desk_class = win::w("DESKORB_DESK");
        class(&desk_class, Some(desk::proc), inst);
        let dh = CreateWindowExW(WS_EX_TOOLWINDOW | WS_EX_LAYERED | WS_EX_NOACTIVATE,
            desk_class.as_ptr(), desk_class.as_ptr(), WS_POPUP, 0, 0, 1, 1, null_mut(), null_mut(), inst, null());
        let cfg = settings::load();
        if cfg.startup {
            settings::set_startup(true);
        }
        let icon = win::text_icon("–");
        win::tray(h, WM_TRAY, NIM_ADD, icon, "Deskorb");
        APP.with_borrow_mut(|a| *a = Some(App {
            cfg, panel: Surface::new(h), prefs: Surface::new(ph), desk: desk::Desk::new(dh, cfg.desk_pos, GetDpiForWindow(h) as f32 / 96.0),
            p_rows: ROWS.iter().map(|(_, c)| match c {
                Ctl::Seg(_) => Spring::new(0.0, 420.0, 0.78),
                Ctl::Toggle => Spring::new(0.0, 520.0, 0.75),
            }).collect(),
            mouse: None, usage: None, usage_err: None, fetching: false, fetched_at: None, usage_every: USAGE_EVERY, reading: false, pro: false, pro_all: Vec::new(), pro_days: [(0, 0.0); 7], forecast: usage::Forecast::Idle, alerted_pct: (None, 0), notified_waits: Vec::new(), alerted: false, icon,
            taskbar_created: RegisterWindowMessageW(win::w("TaskbarCreated").as_ptr()),
            animating: false, last: Instant::now(),
        }));
        app(|a| {
            a.refresh();
            a.apply_desk();
        });
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
