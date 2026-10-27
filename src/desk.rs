//! Widget de bureau, façon DESKTOUCH : un carré de verre posé sur le fond d'écran (sous toutes les fenêtres,
//! toujours là après Win + D) avec le logo Orbite en direct. Au survol, un panneau en sort comme une goutte :
//! système (anneaux CPU / température CPU / RAM, GPU / mémoire vidéo / température GPU, réseau), souris, Claude. Un clic sur le carré le garde ouvert (verrou).
//! Fermé, la fenêtre ne fait que la taille du carré : ses images restent petites.

use crate::{
    app, look, lerp,
    render::{smoothstep, Frame, Layer, Pill, Rgb, Rr, Spring},
    cputemp, gpu, sensors, settings, sys, tr, win, App, Surface, M,
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
const PR: f32 = 26.0;
const RING: f32 = 70.0;
const LOCK: (f32, f32, f32) = (PW - 44.0, 8.0, 32.0); // bouton verrou (x, y, côté), relatif à la goutte
const WM_MOUSELEAVE: u32 = 0x02A3;
const T_OPEN: usize = 11;
const T_CLOSE: usize = 12;
const T_SYS: usize = 13;

/// Place de chaque section dans la goutte (haut, relatif à la goutte), selon celles qu'on affiche.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Lay {
    pub sys: Option<f32>,
    pub gpu: Option<f32>,
    pub net: Option<f32>,
    pub mouse: Option<f32>,
    pub claude: Option<f32>,
    pub h: f32, // hauteur de la goutte
}

pub fn lay(sections: u8) -> Lay {
    let mut y = 40.0; // sous l'en-tête
    let mut take = |bit: u8, h: f32| (sections & bit != 0).then(|| {
        y += h;
        y - h
    });
    let (sys, gpu, net) = (take(settings::SEC_SYS, 112.0), take(settings::SEC_GPU, 112.0), take(settings::SEC_NET, 32.0));
    let (mouse, claude) = (take(settings::SEC_MOUSE, 50.0), take(settings::SEC_CLAUDE, 104.0));
    Lay { sys, gpu, net, mouse, claude, h: y + 2.0 }
}

thread_local! {
    // ouverture au survol en attente : hors de l'état de l'app, qui peut être occupé (redessin) quand
    // Windows envoie les messages de souris, et qui sinon les perdrait
    static OPENING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    // refermée d'un clic : le survol ne la rouvre pas tant que la souris n'est pas sortie du widget
    static CLICK_CLOSED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

pub struct Desk {
    pub surf: Surface,
    pub pos: (i32, i32),       // coin haut-gauche du carré, à l'écran
    press: Option<(i32, i32)>, // bouton enfoncé sur le carré : clic, ou début d'un déplacement
    pub locked: bool,          // ouvert par un clic : reste ouvert jusqu'au clic suivant
    big: bool,                 // fenêtre agrandie pour la goutte
    right: bool,               // la goutte s'ouvre vers la droite / vers le bas
    down: bool,
    sys: sys::Sys,
    pub stats: sys::Stats,
    gpu: Option<gpu::Gpu>, // NVML, ouvert seulement goutte ouverte
    gstats: gpu::Stats,
    ctemp: Result<f32, cputemp::Missing>, // publiée par le service capteurs, ou ce qui manque
    // valeurs affichées, qui glissent vers les mesures :
    // CPU, RAM, température CPU (÷ 100), GPU, mémoire vidéo, température GPU (÷ 100), batterie, session, semaine
    vals: [Spring; 9],
    pub ph: f32, // hauteur de la goutte, selon les sections affichées
}

impl Desk {
    pub unsafe fn new(hwnd: HWND, pos: Option<(i32, i32)>, s: f32) -> Self {
        Desk { surf: Surface::new(hwnd), pos: pos.unwrap_or_else(|| default_pos(s)), press: None, locked: false,
            big: false, right: false, down: true, sys: sys::Sys::default(), stats: sys::Stats::default(),
            gpu: None, gstats: gpu::Stats::default(), ctemp: Err(cputemp::Missing::Off), vals: [Spring::new(0.0, 90.0, 1.0); 9],
            ph: lay(settings::SEC_ALL).h }
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
        let (pw, ph, g) = (PW * s, self.ph * s, GAP * s);
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
        let (pw, ph, g) = ((PW * s) as i32, (self.ph * s) as i32, (GAP * s) as i32);
        self.right = self.pos.0 + sq + g + pw <= wk.right || self.pos.0 - g - pw < wk.left;
        self.down = self.pos.1 + sq + g + ph <= wk.bottom || self.pos.1 - g - ph < wk.top;
        let (ww, wh) = (sq + g + pw + 2 * m, sq + g + ph + 2 * m);
        let left = if self.right { self.pos.0 - m } else { self.pos.0 + sq + m - ww };
        let top = if self.down { self.pos.1 - m } else { self.pos.1 + sq + m - wh };
        self.surf.place(RECT { left, top, right: left + ww, bottom: top + wh });
    }

    /// Relève GPU (NVML) et température CPU (PawnIO), s'ils sont ouverts.
    fn sample_sensors(&mut self) {
        self.gstats = self.gpu.as_ref().map(gpu::Gpu::sample).unwrap_or_default();
        self.ctemp = sensors::read().ok_or_else(cputemp::missing);
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
            self.desk.ph = lay(self.cfg.desk_sections).h;
            self.desk.layout(s, true);
            let probe = self.desk.drop_rr(s);
            self.desk.surf.capture((10.0 * s) as usize, probe);
        }
        if self.desk.surf.open.x < 0.01 {
            self.desk.vals.iter_mut().for_each(|v| v.snap(0.0)); // à l'ouverture, tout se remplit depuis zéro
        }
        self.desk.sys.reset();
        self.desk.stats = self.desk.sys.sample();
        if self.desk.gpu.is_none() {
            self.desk.gpu = gpu::Gpu::open();
        }
        self.desk.sample_sensors();
        self.desk_targets();
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

    /// Nouvelles cibles des valeurs affichées : elles y glissent pendant les images suivantes.
    fn desk_targets(&mut self) {
        let st = self.desk.stats;
        let share = |used: u64, total: u64| used as f32 / total.max(1) as f32;
        let g = self.desk.gstats;
        let targets = [st.cpu, share(st.ram_used, st.ram_total), self.desk.ctemp.unwrap_or(0.0) / 100.0,
            g.load, share(g.vram_used, g.vram_total), g.temp / 100.0, self.battery().map_or(0.0, |b| b as f32 / 100.0), self.usage.map_or(0.0, |u| u.session.pct),
            self.usage.map_or(0.0, |u| u.weekly.pct)];
        for (v, t) in self.desk.vals.iter_mut().zip(targets) {
            v.target = t;
        }
        self.kick();
    }

    /// Données changées (relevé, souris, Claude) : on redessine ce qui est visible.
    /// Sections changées : la goutte ouverte prend sa nouvelle hauteur.
    pub unsafe fn desk_resize(&mut self) {
        if self.desk.big {
            let s = self.scale();
            self.desk.ph = lay(self.cfg.desk_sections).h;
            self.desk.layout(s, true);
            let probe = self.desk.drop_rr(s);
            self.desk.surf.capture((10.0 * s) as usize, probe);
        }
        self.desk_refresh();
    }

    pub unsafe fn desk_refresh(&mut self) {
        if !self.desk.surf.shown {
            return;
        }
        self.desk_targets();
        if self.desk.surf.open.target > 0.5 {
            self.rebuild_desk_layer();
        }
        self.draw_desk();
    }

    /// Une image d'animation ; fermée et au repos, la fenêtre redevient un simple carré.
    pub unsafe fn desk_tick(&mut self, dt: f32) -> bool {
        let moving = !self.desk.surf.open.rest();
        let values = self.desk.big && !self.desk.vals.iter().all(|v| v.rest());
        self.desk.surf.open.step(dt);
        self.desk.vals.iter_mut().for_each(|v| v.step(dt));
        if values {
            self.rebuild_desk_layer(); // anneaux et nombres suivent les valeurs qui glissent
        }
        if (moving || values) && self.desk.surf.shown {
            self.draw_desk();
        }
        if self.desk.big && self.desk.surf.open.target == 0.0 && self.desk.surf.open.rest() {
            self.desk.surf.layer = Layer::empty();
            self.desk.gpu = None; // libère NVML
            self.apply_desk(); // retour au carré : petite fenêtre, petites images
            crate::trim();
        }
        moving || values
    }

    /// Textes et anneaux de la goutte, dans un seul calque (aux dimensions de la goutte).
    unsafe fn rebuild_desk_layer(&mut self) {
        let s = self.scale();
        let fr = self.fr();
        let st = self.desk.stats;
        let px = |v: f32| (v * s) as i32;
        let rect = |x: f32, y: f32, w: f32, h: f32| RECT { left: px(x), top: px(y), right: px(x + w), bottom: px(y + h) };
        let t = |txt, r, size: f32, weight, flags, level| win::Text { s: txt, r, size: size * s, weight, flags, level };
        let [cpu, ram, ctemp, gload, vram, gtemp, ..] = self.desk.vals.map(|v| v.x.max(0.0));
        let gs = self.desk.gstats;
        let has_gpu = self.desk.gpu.is_some();
        let no_gpu = tr(fr, ("No NVIDIA GPU", "Pas de GPU NVIDIA"));
        // (valeur 0–1, unité, nom, détail) ; la température s'affiche en °C (÷ 100 pour l'anneau et les seuils)
        let gauges: [(f32, &str, &str, String); 6] = [
            (cpu, "%", "CPU", format!("{} threads", st.threads)),
            (ctemp, "°C", tr(fr, ("CPU temp.", "Temp. CPU")), tr(fr, match self.desk.ctemp {
                Ok(_) => ("package", "package"),
                Err(cputemp::Missing::Off) => ("click to enable", "cliquer pour activer"),
                Err(cputemp::Missing::NotIntel) => ("Intel only", "Intel seulement"),
                Err(cputemp::Missing::NoPawnIO) => ("install PawnIO", "installer PawnIO"),
            }).into()),
            (ram, "%", "RAM", sys::fmt_gb(st.ram_used, st.ram_total, fr)),
            (gload, "%", "GPU", if has_gpu { tr(fr, ("load", "charge")).into() } else { no_gpu.into() }),
            (vram, "%", tr(fr, ("Video memory", "Mémoire vidéo")), if has_gpu { sys::fmt_gb(gs.vram_used, gs.vram_total, fr) } else { "—".into() }),
            (gtemp, "°C", tr(fr, ("GPU temp.", "Temp. GPU")), if has_gpu { tr(fr, ("sensor", "capteur")).into() } else { "—".into() }),
        ];
        // sans carte NVIDIA, la rangée GPU affiche des tirets
        let values: Vec<String> = gauges.iter().enumerate().map(|(i, g)| match i {
            1 if self.desk.ctemp.is_err() => "—".into(), // température CPU : pas de pilote ou service arrêté
            0 | 2 => format!("{:.0}", g.0 * 100.0),
            _ if has_gpu => format!("{:.0}", g.0 * 100.0),
            _ => "—".into(),
        }).collect();
        let net = format!("↓ {}   ↑ {}", sys::fmt_rate(st.down, fr), sys::fmt_rate(st.up, fr));
        let bat_txt = self.battery().map_or("—".into(), |b| format!("{b} %"));
        let mouse_line = self.mouse_line();
        let (session, weekly) = (self.usage.map(|u| u.session), self.usage.map(|u| u.weekly));
        let (session_pct, weekly_pct) = (Self::pct(session), Self::pct(weekly));
        let session_line = self.session_line();
        let weekly_line = self.window_line(weekly, ("All models", "Tous les modèles"));
        let (l, ly) = (LOCK, lay(self.cfg.desk_sections));
        let mut texts = vec![
            t("DESKORB", rect(20.0, 12.0, 160.0, 20.0), 11.0, 700, DT_LEFT | DT_VCENTER, 0.55),
            // verrou : fermé = la goutte reste ouverte
            t(if self.desk.locked { "\u{E72E}" } else { "\u{E785}" }, rect(l.0, l.1, l.2, l.2), 13.0, 400, DT_CENTER | DT_VCENTER, 0.75),
        ];
        if let Some(y) = ly.net {
            texts.push(t(tr(fr, ("Network", "Réseau")), rect(20.0, y, 100.0, 18.0), 12.0, 600, DT_LEFT | DT_VCENTER, 0.9));
            texts.push(t(&net, rect(100.0, y, 180.0, 18.0), 12.0, 600, DT_RIGHT | DT_VCENTER, 0.85));
        }
        if let Some(y) = ly.mouse {
            texts.push(t("Pulsar TenZ", rect(20.0, y, 180.0, 20.0), 13.0, 600, DT_LEFT | DT_VCENTER, 1.0));
            texts.push(t(&bat_txt, rect(180.0, y - 2.0, 100.0, 24.0), 16.0, 600, DT_RIGHT | DT_VCENTER, 1.0));
            texts.push(t(&mouse_line, rect(20.0, y + 20.0, 260.0, 16.0), 11.0, 400, DT_LEFT | DT_VCENTER | DT_END_ELLIPSIS, 0.65));
        }
        if let Some(y) = ly.claude {
            texts.push(t("Claude", rect(20.0, y, 180.0, 20.0), 13.0, 600, DT_LEFT | DT_VCENTER, 1.0));
            texts.push(t(&session_pct, rect(180.0, y - 2.0, 100.0, 24.0), 16.0, 600, DT_RIGHT | DT_VCENTER, 1.0));
            texts.push(t(&session_line, rect(20.0, y + 32.0, 260.0, 16.0), 11.0, 400, DT_LEFT | DT_VCENTER | DT_END_ELLIPSIS, 0.65));
            texts.push(t(tr(fr, ("Weekly", "Hebdomadaire")), rect(20.0, y + 54.0, 180.0, 18.0), 12.0, 600, DT_LEFT | DT_VCENTER, 0.9));
            texts.push(t(&weekly_pct, rect(180.0, y + 53.0, 100.0, 20.0), 13.0, 600, DT_RIGHT | DT_VCENTER, 0.9));
            texts.push(t(&weekly_line, rect(20.0, y + 82.0, 260.0, 16.0), 11.0, 400, DT_LEFT | DT_VCENTER | DT_END_ELLIPSIS, 0.65));
        }
        // rangées de trois anneaux (système, carte graphique) : valeur au centre, nom et détail dessous
        let row_y = |i: usize| if i < 3 { ly.sys } else { ly.gpu };
        let at = |i: usize| row_y(i).map(|y| (15.0 + (i % 3) as f32 * 90.0, y));
        for (i, (g, v)) in gauges.iter().zip(&values).enumerate() {
            let Some((cx, y)) = at(i) else { continue };
            texts.push(t(v, rect(cx, y + RING / 2.0 - 13.0, 90.0, 22.0), 18.0, 600, DT_CENTER | DT_VCENTER, 1.0));
            texts.push(t(g.1, rect(cx, y + RING / 2.0 + 7.0, 90.0, 14.0), 10.0, 600, DT_CENTER | DT_VCENTER, 0.55));
            texts.push(t(g.2, rect(cx, y + 74.0, 90.0, 16.0), 11.5, 600, DT_CENTER | DT_VCENTER, 0.9));
            texts.push(t(&g.3, rect(cx, y + 90.0, 90.0, 14.0), 10.5, 400, DT_CENTER | DT_VCENTER | DT_END_ELLIPSIS, 0.6));
        }
        let ink = look(self.cfg.theme, self.desk.surf.light_back).ink;
        let (lw, lh) = ((PW * s) as usize, (ly.h * s) as usize);
        let mut layer = win::text_layer(lw, lh, &texts, ink);
        let n = (RING * s).round() as usize;
        for (i, g) in gauges.iter().enumerate() {
            let Some((cx, y)) = at(i) else { continue };
            let (ox, oy) = (((cx + (90.0 - RING) / 2.0) * s) as usize, (y * s) as usize);
            for (k, p) in win::ring(n, g.0, crate::gauge_color(g.0, ink), ink).into_iter().enumerate() {
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
            let [.., bat, session, weekly] = d.vals.map(|v| v.x.max(0.0));
            let (ly, white) = (lay(self.cfg.desk_sections), crate::accent(&look));
            if let Some(y) = ly.mouse {
                pills.extend(bar(y + 39.0, bat, crate::battery_color(bat, white), 5.0));
            }
            if let Some(y) = ly.claude {
                pills.extend(bar(y + 25.0, session, crate::gauge_color(session, white), 5.0));
                pills.extend(bar(y + 75.0, weekly, crate::gauge_color(weekly, white), 4.0));
            }
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
            // opacité choisie au repos, pleine goutte ouverte
            alpha: lerp(self.cfg.desk_opacity, 1.0, o.clamp(0.0, 1.0)),
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
            // sur le widget : on (re)demande à être prévenu de la sortie, et petite attente avant d'ouvrir
            // (on peut le traverser ou l'attraper)
            track();
            KillTimer(h, T_CLOSE);
            if !OPENING.get() && !CLICK_CLOSED.get() && app(|a| a.desk.surf.open.target < 0.5 && !a.cfg.desk_click).unwrap_or(false) {
                OPENING.set(true);
                SetTimer(h, T_OPEN, 140, None);
            }
            0
        }
        WM_MOUSELEAVE => {
            // la goutte s'agrandit sous la souris : fausse sortie, on vérifie où elle est vraiment
            let mut c: POINT = std::mem::zeroed();
            GetCursorPos(&mut c);
            // app occupée : on suppose la souris dedans et on redemande la sortie, plutôt que de la perdre
            let inside = app(|a| {
                let s = a.scale();
                a.desk.press.is_some() || a.desk.over(s, (c.x - a.desk.surf.win.left) as f32, (c.y - a.desk.surf.win.top) as f32)
            }).unwrap_or(true);
            if inside {
                track();
                return 0;
            }
            (OPENING.set(false), CLICK_CLOSED.set(false));
            KillTimer(h, T_OPEN);
            if !app(|a| a.desk.locked).unwrap_or(false) {
                SetTimer(h, T_CLOSE, 300, None); // pardonne un écart de souris
            }
            0
        }
        WM_TIMER => {
            match wp {
                // app occupée : le minuteur reste armé et réessaie au prochain tic
                T_OPEN => {
                    if app(|a| a.desk_open()).is_some() {
                        KillTimer(h, T_OPEN);
                        OPENING.set(false);
                    }
                }
                T_CLOSE => {
                    if app(|a| if !a.desk.locked { a.desk_close() }).is_some() {
                        KillTimer(h, T_CLOSE);
                    }
                }
                T_SYS => {
                    app(|a| {
                        a.desk.stats = a.desk.sys.sample();
                        a.desk.sample_sensors();
                        a.desk_targets(); // les anneaux glissent vers la nouvelle mesure
                        a.rebuild_desk_layer(); // le réseau change aussi quand les anneaux ne bougent pas
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
                // anneau « Temp. CPU » (2e de la 1re rangée) tant que le service n'est pas actif
                let sys_y = lay(a.cfg.desk_sections).sys;
                if a.desk.surf.open.x > 0.5 && a.desk.ctemp == Err(cputemp::Missing::Off)
                    && sys_y.is_some_and(|y| (105.0..195.0).contains(&lx) && (y..y + 104.0).contains(&ly)) {
                    return 3;
                }
                0
            }).unwrap_or(0);
            match target {
                1 => {
                    KillTimer(h, T_OPEN);
                    SetCapture(h);
                }
                3 => sensors::setup(),
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
            // clic sur le carré : fermé, il ouvre et verrouille ; ouvert (survol ou verrou), il referme
            app(|a| {
                if a.desk.press.take().is_some() {
                    if a.desk.surf.open.target > 0.5 {
                        a.desk_close();
                        CLICK_CLOSED.set(true);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_follows_sections() {
        let all = lay(settings::SEC_ALL);
        assert_eq!((all.sys, all.gpu, all.net, all.mouse, all.claude, all.h), (Some(40.0), Some(152.0), Some(264.0), Some(296.0), Some(346.0), 452.0));
        let some = lay(settings::SEC_GPU | settings::SEC_CLAUDE);
        assert_eq!((some.sys, some.gpu, some.claude, some.h), (None, Some(40.0), Some(152.0), 258.0));
        assert_eq!(lay(0).h, 42.0); // l'en-tête seul
    }
}
