//! Widget de bureau, façon DESKTOUCH : un carré de verre posé sur le fond d'écran (sous toutes les fenêtres,
//! toujours là après Win + D) avec le logo Orbite en direct. Au survol, un panneau en sort comme une goutte :
//! système (anneaux CPU / RAM / disque), souris, Claude. Un clic sur le carré le garde ouvert (verrou).
//! Fermé, la fenêtre ne fait que la taille du carré : ses images restent petites.

use crate::{
    app, look, lerp,
    render::{smoothstep, Frame, Layer, Pill, Rgb, Rr},
    sys, tr, win, App, Surface, M,
};
use windows_sys::Win32::{
    Foundation::*,
    Graphics::Gdi::*,
    UI::{Input::KeyboardAndMouse::*, WindowsAndMessaging::*},
};

pub const SQ: f32 = 46.0;
const SQ_R: f32 = 15.0;
const GLYPH: f32 = 30.0;
const GAP: f32 = 8.0; // entre le carré et la goutte ouverte
const PW: f32 = 300.0;
const PH: f32 = 322.0;
const PR: f32 = 26.0;
const RING: f32 = 70.0;
const LOCK: (f32, f32, f32) = (PW - 44.0, 8.0, 32.0); // bouton verrou (x, y, côté), relatif à la goutte
const WM_MOUSELEAVE: u32 = 0x02A3;
const T_OPEN: usize = 11;
const T_CLOSE: usize = 12;
const T_SYS: usize = 13;

pub struct Desk {
    pub surf: Surface,
    pub pos: (i32, i32),       // coin haut-gauche du carré, à l'écran
    press: Option<(i32, i32)>, // bouton enfoncé sur le carré : clic, ou début d'un déplacement
    pub locked: bool,          // ouvert par un clic : reste ouvert jusqu'au clic suivant
    tracking: bool,            // la souris est sur le widget
    big: bool,                 // fenêtre agrandie pour la goutte
    right: bool,               // la goutte s'ouvre vers la droite / vers le bas
    down: bool,
    sys: sys::Sys,
    pub stats: sys::Stats,
}

impl Desk {
    pub unsafe fn new(hwnd: HWND, pos: Option<(i32, i32)>, s: f32) -> Self {
        Desk { surf: Surface::new(hwnd), pos: pos.unwrap_or_else(|| default_pos(s)), press: None, locked: false,
            tracking: false, big: false, right: false, down: true, sys: sys::Sys::default(), stats: sys::Stats::default() }
    }

    /// Le carré, dans la fenêtre.
    fn square(&self, s: f32) -> Rr {
        let (m, sq) = (M * s, SQ * s);
        let (ww, wh) = self.surf_size();
        let x = if !self.big || self.right { m } else { ww - m - sq };
        let y = if !self.big || self.down { m } else { wh - m - sq };
        Rr { x, y, w: sq, h: sq, r: SQ_R * s }
    }

    /// La goutte ouverte, dans la fenêtre : elle part du coin du carré, en diagonale vers l'intérieur de l'écran.
    fn drop_rr(&self, s: f32) -> Rr {
        let q = self.square(s);
        let (pw, ph, g) = (PW * s, PH * s, GAP * s);
        Rr { x: if self.right { q.x + q.w + g } else { q.x - g - pw }, y: if self.down { q.y + q.h + g } else { q.y - g - ph },
            w: pw, h: ph, r: PR * s }
    }

    fn surf_size(&self) -> (f32, f32) {
        let (w, h) = self.surf.size();
        (w as f32, h as f32)
    }

    /// Fenêtre réduite au carré (fermé) ou agrandie pour la goutte, du côté où il y a la place.
    unsafe fn layout(&mut self, s: f32, big: bool) {
        let (m, sq) = ((M * s) as i32, (SQ * s) as i32);
        self.big = big;
        if !big {
            let r = RECT { left: self.pos.0 - m, top: self.pos.1 - m, right: self.pos.0 + sq + m, bottom: self.pos.1 + sq + m };
            return self.surf.place(r);
        }
        let mut mi: MONITORINFO = std::mem::zeroed();
        mi.cbSize = size_of::<MONITORINFO>() as u32;
        GetMonitorInfoW(MonitorFromPoint(POINT { x: self.pos.0, y: self.pos.1 }, MONITOR_DEFAULTTONEAREST), &mut mi);
        let wk = mi.rcWork;
        let (pw, ph, g) = ((PW * s) as i32, (PH * s) as i32, (GAP * s) as i32);
        self.right = self.pos.0 + sq + g + pw <= wk.right || self.pos.0 - g - pw < wk.left;
        self.down = self.pos.1 + sq + g + ph <= wk.bottom || self.pos.1 - g - ph < wk.top;
        let (ww, wh) = (sq + g + pw + 2 * m, sq + g + ph + 2 * m);
        let left = if self.right { self.pos.0 - m } else { self.pos.0 + sq + m - ww };
        let top = if self.down { self.pos.1 - m } else { self.pos.1 + sq + m - wh };
        self.surf.place(RECT { left, top, right: left + ww, bottom: top + wh });
    }

    /// La souris est-elle sur le carré ou sur la goutte (coordonnées fenêtre) ?
    fn over(&self, s: f32, x: f32, y: f32) -> bool {
        self.square(s).contains(x, y) || (self.surf.open.x > 0.5 && self.drop_rr(s).contains(x, y))
    }
}

/// Coin haut-droit de l'écran principal : DESKTOUCH occupe déjà le haut-gauche.
fn default_pos(s: f32) -> (i32, i32) {
    let mut wa: RECT = unsafe { std::mem::zeroed() };
    unsafe { SystemParametersInfoW(SPI_GETWORKAREA, 0, &mut wa as *mut RECT as _, 0) };
    (wa.right - ((60.0 + SQ) * s) as i32, wa.top + (120.0 * s) as i32)
}

impl App {
    /// Valeur du logo du widget : la batterie, sinon la session Claude.
    fn desk_value(&self) -> Option<f32> {
        self.battery().map(|b| b as f32 / 100.0).or(self.usage.map(|u| u.session.pct))
    }

    /// Affiche ou retire le widget de bureau selon le réglage.
    pub unsafe fn apply_desk(&mut self) {
        if !self.cfg.desk {
            self.desk.locked = false;
            self.desk.surf.open.snap(0.0);
            KillTimer(self.desk.surf.hwnd, T_SYS);
            return self.desk.surf.release();
        }
        let s = self.scale();
        self.desk.layout(s, false);
        let q = self.desk.square(s);
        self.desk.surf.capture((4.0 * s) as usize, q);
        self.draw_desk();
        if !self.desk.surf.shown {
            ShowWindow(self.desk.surf.hwnd, SW_SHOWNOACTIVATE);
            win::pin_to_desktop(self.desk.surf.hwnd); // après l'affichage, qui sinon la remonte devant les fenêtres
            self.desk.surf.shown = true;
        }
    }

    /// Fin d'un déplacement : `left`, `top` = nouvelle place de la fenêtre (réduite au carré).
    pub unsafe fn desk_moved(&mut self, left: i32, top: i32) {
        let m = (M * self.scale()) as i32;
        self.desk.pos = (left + m, top + m);
        self.cfg.desk_pos = Some(self.desk.pos);
        crate::settings::save(&self.cfg);
        self.apply_desk(); // nouvelle photo du fond à la nouvelle place
    }

    /// Ouvre la goutte : fenêtre agrandie, photo du fond, mesures chaque seconde.
    unsafe fn desk_open(&mut self) {
        if self.desk.big && self.desk.surf.open.target > 0.5 {
            return;
        }
        let s = self.scale();
        if !self.desk.big {
            self.desk.layout(s, true);
            let probe = self.desk.drop_rr(s);
            self.desk.surf.capture((10.0 * s) as usize, probe);
        }
        self.desk.sys.reset();
        self.desk.stats = self.desk.sys.sample();
        SetTimer(self.desk.surf.hwnd, T_SYS, 1000, None);
        self.rebuild_desk_layer();
        self.desk.surf.open.target = 1.0;
        self.kick();
    }

    fn desk_close(&mut self) {
        self.desk.locked = false;
        self.desk.surf.open.target = 0.0;
        unsafe { KillTimer(self.desk.surf.hwnd, T_SYS) };
        self.kick();
    }

    /// Données changées (relevé, souris, Claude) : on redessine ce qui est visible.
    pub unsafe fn desk_refresh(&mut self) {
        if !self.desk.surf.shown {
            return;
        }
        if self.desk.surf.open.target > 0.5 {
            self.rebuild_desk_layer();
        }
        self.draw_desk();
    }

    /// Une image d'animation ; fermée et au repos, la fenêtre redevient un simple carré.
    pub unsafe fn desk_tick(&mut self, dt: f32) -> bool {
        let moving = !self.desk.surf.open.rest();
        self.desk.surf.open.step(dt);
        if moving && self.desk.surf.shown {
            self.draw_desk();
        }
        if self.desk.big && self.desk.surf.open.target == 0.0 && self.desk.surf.open.rest() {
            self.desk.surf.layer = Layer::empty();
            self.apply_desk(); // retour au carré : petite fenêtre, petites images
            crate::trim();
        }
        moving
    }

    /// Textes et anneaux de la goutte, dans un seul calque (aux dimensions de la goutte).
    unsafe fn rebuild_desk_layer(&mut self) {
        let s = self.scale();
        let fr = self.fr();
        let st = self.desk.stats;
        let px = |v: f32| (v * s) as i32;
        let rect = |x: f32, y: f32, w: f32, h: f32| RECT { left: px(x), top: px(y), right: px(x + w), bottom: px(y + h) };
        let t = |txt, r, size: f32, weight, flags, level| win::Text { s: txt, r, size: size * s, weight, flags, level };
        let ram = st.ram_used as f32 / st.ram_total.max(1) as f32;
        let disk = st.disk_used as f32 / st.disk_total.max(1) as f32;
        let gauges: [(f32, &str, String); 3] = [
            (st.cpu, "CPU", format!("{} {}", st.threads, tr(fr, ("threads", "threads")))),
            (ram, "RAM", sys::fmt_gb(st.ram_used, st.ram_total, fr)),
            (disk, tr(fr, ("Disk C:", "Disque C:")), sys::fmt_gb(st.disk_used, st.disk_total, fr)),
        ];
        let values: Vec<String> = gauges.iter().map(|g| format!("{:.0}", g.0 * 100.0)).collect();
        let bat_txt = self.battery().map_or("—".into(), |b| format!("{b} %"));
        let mouse_line = self.mouse_line();
        let (session, weekly) = (self.usage.map(|u| u.session), self.usage.map(|u| u.weekly));
        let (session_pct, weekly_pct) = (Self::pct(session), Self::pct(weekly));
        let session_line = self.session_line();
        let weekly_line = self.window_line(weekly, ("All models", "Tous les modèles"));
        let l = LOCK;
        let mut texts = vec![
            t("DESKORB", rect(20.0, 12.0, 160.0, 20.0), 11.0, 700, DT_LEFT | DT_VCENTER, 0.55),
            // verrou : fermé = la goutte reste ouverte
            t(if self.desk.locked { "\u{E72E}" } else { "\u{E785}" }, rect(l.0, l.1, l.2, l.2), 13.0, 400, DT_CENTER | DT_VCENTER, 0.75),
            t("Pulsar TenZ", rect(20.0, 166.0, 180.0, 20.0), 13.0, 600, DT_LEFT | DT_VCENTER, 1.0),
            t(&bat_txt, rect(180.0, 164.0, 100.0, 24.0), 16.0, 600, DT_RIGHT | DT_VCENTER, 1.0),
            t(&mouse_line, rect(20.0, 186.0, 260.0, 16.0), 11.0, 400, DT_LEFT | DT_VCENTER | DT_END_ELLIPSIS, 0.65),
            t("Claude", rect(20.0, 216.0, 180.0, 20.0), 13.0, 600, DT_LEFT | DT_VCENTER, 1.0),
            t(&session_pct, rect(180.0, 214.0, 100.0, 24.0), 16.0, 600, DT_RIGHT | DT_VCENTER, 1.0),
            t(&session_line, rect(20.0, 248.0, 260.0, 16.0), 11.0, 400, DT_LEFT | DT_VCENTER | DT_END_ELLIPSIS, 0.65),
            t(tr(fr, ("Weekly", "Hebdomadaire")), rect(20.0, 270.0, 180.0, 18.0), 12.0, 600, DT_LEFT | DT_VCENTER, 0.9),
            t(&weekly_pct, rect(180.0, 269.0, 100.0, 20.0), 13.0, 600, DT_RIGHT | DT_VCENTER, 0.9),
            t(&weekly_line, rect(20.0, 298.0, 260.0, 16.0), 11.0, 400, DT_LEFT | DT_VCENTER | DT_END_ELLIPSIS, 0.65),
        ];
        // trois anneaux : valeur au centre, nom et détail dessous
        for (i, (g, v)) in gauges.iter().zip(&values).enumerate() {
            let cx = 15.0 + i as f32 * 90.0;
            texts.push(t(v, rect(cx, 40.0 + RING / 2.0 - 13.0, 90.0, 22.0), 18.0, 600, DT_CENTER | DT_VCENTER, 1.0));
            texts.push(t("%", rect(cx, 40.0 + RING / 2.0 + 7.0, 90.0, 14.0), 10.0, 600, DT_CENTER | DT_VCENTER, 0.55));
            texts.push(t(g.1, rect(cx, 114.0, 90.0, 16.0), 11.5, 600, DT_CENTER | DT_VCENTER, 0.9));
            texts.push(t(&g.2, rect(cx, 130.0, 90.0, 14.0), 10.5, 400, DT_CENTER | DT_VCENTER, 0.6));
        }
        let ink = look(self.cfg.theme, self.desk.surf.light_back).ink;
        let (lw, lh) = ((PW * s) as usize, (PH * s) as usize);
        let mut layer = win::text_layer(lw, lh, &texts, ink);
        let n = (RING * s).round() as usize;
        for (i, g) in gauges.iter().enumerate() {
            let (ox, oy) = (((15.0 + i as f32 * 90.0 + (90.0 - RING) / 2.0) * s) as usize, (40.0 * s) as usize);
            for (k, p) in win::ring(n, g.0, ink).into_iter().enumerate() {
                let (x, y) = (ox + k % n, oy + k / n);
                if x < lw && y < lh {
                    let d = &mut layer.px[y * lw + x];
                    *d = std::array::from_fn(|c| p[c] + d[c] * (1.0 - p[3]));
                }
            }
        }
        self.desk.surf.layer = layer;
    }

    /// Le carré, et la goutte qui en sort pendant l'ouverture (les deux formes fusionnent puis se détachent).
    unsafe fn draw_desk(&self) {
        let d = &self.desk;
        if d.surf.bits.is_null() {
            return;
        }
        let s = self.scale();
        let o = d.surf.open.x.clamp(0.0, 1.2);
        let look = look(self.cfg.theme, d.surf.light_back);
        let q = d.square(s);
        // logo Orbite au centre du carré
        let n = (GLYPH * s).round() as usize;
        let ink = look.ink;
        let glyph = Layer { w: n, h: n, px: win::logo_alpha(0, n, self.desk_value(), None).into_iter().map(|a| [ink[0] * a, ink[1] * a, ink[2] * a, a]).collect() };
        let at = ((q.x + (q.w - n as f32) / 2.0) as i32, (q.y + (q.h - n as f32) / 2.0) as i32);
        let mut layers = vec![(&glyph, at, 1.0)];
        let mut pills = Vec::new();
        let drop = (d.big && o > 0.002).then(|| Rr::lerp(q, d.drop_rr(s), o));
        let content = smoothstep(0.55, 1.0, o);
        if let (Some(_), true) = (drop, !d.surf.layer.px.is_empty()) {
            let p = d.drop_rr(s);
            layers.push((&d.surf.layer, (p.x as i32, p.y as i32), content));
            let bar = |y: f32, v: f32, color: Rgb, th: f32| {
                let rail = Rr { x: p.x + 20.0 * s, y: p.y + y * s, w: 260.0 * s, h: th * s, r: th * s / 2.0 };
                let fill = Rr { w: (rail.w * v.clamp(0.0, 1.0)).max(rail.h), ..rail };
                [Pill { rr: rail, color: ink, amount: 0.12 * content, rim: 0.0 },
                    Pill { rr: fill, color, amount: if v > 0.0 { 0.9 * content } else { 0.0 }, rim: 0.4 * content }]
            };
            let bat = self.battery().map_or(0.0, |b| b as f32 / 100.0);
            let (session, weekly) = (self.usage.map_or(0.0, |u| u.session.pct), self.usage.map_or(0.0, |u| u.weekly.pct));
            pills.extend(bar(205.0, bat, [0.35, 0.85, 0.45], 5.0));
            pills.extend(bar(241.0, session, crate::gauge_color(session, [0.45, 0.6, 1.0]), 5.0));
            pills.extend(bar(291.0, weekly, crate::gauge_color(weekly, [0.7, 0.55, 1.0]), 4.0));
            let l = LOCK;
            pills.push(Pill { rr: Rr { x: p.x + l.0 * s, y: p.y + l.1 * s, w: l.2 * s, h: l.2 * s, r: l.2 * s / 2.0 },
                color: crate::accent(&look), amount: if d.locked { 0.3 } else { 0.1 } * content, rim: 0.3 * content });
        }
        d.surf.present(&Frame {
            back: &d.surf.back,
            layers: &layers,
            a: q,
            b: drop,
            // fusion forte au départ (la goutte s'étire), puis les deux formes se détachent
            smooth: 18.0 * s * smoothstep(0.0, 0.12, o) * lerp(1.0, 0.3, o.clamp(0.0, 1.0)),
            refr_height: 22.0 * s,
            refr_amount: 36.0 * s,
            glyph: false,
            pills: &pills,
            alpha: 1.0,
            scale: s,
            look,
        });
    }
}

pub unsafe extern "system" fn proc(h: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let xy = || ((lp & 0xFFFF) as i16 as i32, ((lp >> 16) & 0xFFFF) as i16 as i32);
    let track = || {
        let mut tme = TRACKMOUSEEVENT { cbSize: size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: h, dwHoverTime: 0 };
        TrackMouseEvent(&mut tme);
    };
    match msg {
        // ne jamais voler le focus ni passer devant les autres fenêtres
        WM_MOUSEACTIVATE => MA_NOACTIVATE as isize,
        WM_MOUSEMOVE => {
            let (x, y) = xy();
            // bouton enfoncé sur le carré fermé et la souris bouge : on le déplace (Windows suit la souris)
            let drag = app(|a| {
                let p = a.desk.press.filter(|_| !a.desk.big)?; // goutte ouverte : un clic, pas un déplacement
                ((p.0 - x).abs() > 4 || (p.1 - y).abs() > 4).then(|| a.desk.press = None)
            }).flatten().is_some();
            if drag {
                KillTimer(h, T_OPEN);
                ReleaseCapture();
                SendMessageW(h, WM_NCLBUTTONDOWN, HTCAPTION as usize, 0);
                return 0;
            }
            // arrivée sur le widget : petite attente avant d'ouvrir (on peut le traverser ou l'attraper)
            if app(|a| !std::mem::replace(&mut a.desk.tracking, true)).unwrap_or(false) {
                track();
                KillTimer(h, T_CLOSE);
                if app(|a| a.desk.surf.open.target < 0.5).unwrap_or(false) {
                    SetTimer(h, T_OPEN, 140, None);
                }
            }
            0
        }
        WM_MOUSELEAVE => {
            // la goutte s'agrandit sous la souris : fausse sortie, on vérifie où elle est vraiment
            let mut c: POINT = std::mem::zeroed();
            GetCursorPos(&mut c);
            let inside = app(|a| {
                let s = a.scale();
                a.desk.press.is_some() || a.desk.over(s, (c.x - a.desk.surf.win.left) as f32, (c.y - a.desk.surf.win.top) as f32)
            }).unwrap_or(false);
            if inside {
                track();
                return 0;
            }
            app(|a| a.desk.tracking = false);
            KillTimer(h, T_OPEN);
            if !app(|a| a.desk.locked).unwrap_or(false) {
                SetTimer(h, T_CLOSE, 300, None); // pardonne un écart de souris
            }
            0
        }
        WM_TIMER => {
            match wp {
                T_OPEN => {
                    KillTimer(h, T_OPEN);
                    app(|a| a.desk_open());
                }
                T_CLOSE => {
                    KillTimer(h, T_CLOSE);
                    app(|a| if !a.desk.locked { a.desk_close() });
                }
                T_SYS => {
                    app(|a| {
                        a.desk.stats = a.desk.sys.sample();
                        a.rebuild_desk_layer();
                        a.draw_desk();
                    });
                }
                _ => {}
            }
            0
        }
        WM_LBUTTONDOWN => {
            let (x, y) = xy();
            let target = app(|a| {
                let s = a.scale();
                if a.desk.square(s).contains(x as f32, y as f32) {
                    a.desk.press = Some((x, y));
                    return 1;
                }
                let p = a.desk.drop_rr(s);
                let l = LOCK;
                let (lx, ly) = ((x as f32 - p.x) / s, (y as f32 - p.y) / s);
                if a.desk.surf.open.x > 0.5 && (l.0..l.0 + l.2).contains(&lx) && (l.1..l.1 + l.2).contains(&ly) {
                    return 2;
                }
                0
            }).unwrap_or(0);
            match target {
                1 => {
                    KillTimer(h, T_OPEN);
                    SetCapture(h);
                }
                // bouton verrou de la goutte
                2 => {
                    app(|a| {
                        a.desk.locked = !a.desk.locked;
                        a.rebuild_desk_layer();
                        a.draw_desk();
                    });
                }
                _ => {}
            }
            0
        }
        WM_LBUTTONUP => {
            ReleaseCapture();
            // clic sur le carré : ouvre et verrouille ; déjà verrouillé : referme
            app(|a| {
                if a.desk.press.take().is_some() {
                    if a.desk.locked {
                        a.desk_close();
                    } else {
                        a.desk.locked = true;
                        a.desk_open();
                        a.rebuild_desk_layer();
                    }
                }
            });
            0
        }
        // fin d'un déplacement : nouvelle place, gardée dans les réglages
        WM_EXITSIZEMOVE => {
            let mut r: RECT = std::mem::zeroed();
            GetWindowRect(h, &mut r);
            app(|a| a.desk_moved(r.left, r.top));
            0
        }
        WM_RBUTTONUP => {
            app(|a| a.show_prefs());
            0
        }
        _ => DefWindowProcW(h, msg, wp, lp),
    }
}
