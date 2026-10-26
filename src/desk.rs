//! Widget de bureau, façon DESKTOUCH : un carré de verre posé sur le fond d'écran (sous toutes les fenêtres,
//! toujours là après Win + D) qui affiche le logo Orbite en direct. On le déplace en le faisant glisser.

use crate::{app, look, render::{Frame, Layer, Rr}, win, Surface, M};
use windows_sys::Win32::{
    Foundation::*,
    UI::{Input::KeyboardAndMouse::*, WindowsAndMessaging::*},
};

pub const SQ: f32 = 46.0;
const SQ_R: f32 = 15.0;
const GLYPH: f32 = 30.0;

pub struct Desk {
    pub surf: Surface,
    pub pos: (i32, i32), // coin haut-gauche du carré, à l'écran
    press: Option<(i32, i32)>, // bouton enfoncé sur le carré : clic, ou début d'un déplacement
}

impl Desk {
    pub unsafe fn new(hwnd: HWND, pos: Option<(i32, i32)>, s: f32) -> Self {
        Desk { surf: Surface::new(hwnd), pos: pos.unwrap_or_else(|| default_pos(s)), press: None }
    }

    fn square(&self, s: f32) -> Rr {
        Rr { x: M * s, y: M * s, w: SQ * s, h: SQ * s, r: SQ_R * s }
    }

    /// Place la fenêtre autour du carré, photographie le fond et l'affiche, ancrée au bureau.
    pub unsafe fn show(&mut self, s: f32, theme: usize, value: Option<f32>) {
        let (m, sq) = ((M * s) as i32, (SQ * s) as i32);
        let r = RECT { left: self.pos.0 - m, top: self.pos.1 - m, right: self.pos.0 + sq + m, bottom: self.pos.1 + sq + m };
        self.surf.place(r);
        let sqr = self.square(s);
        self.surf.capture((4.0 * s) as usize, sqr);
        self.draw(s, theme, value);
        if !self.surf.shown {
            ShowWindow(self.surf.hwnd, SW_SHOWNOACTIVATE);
            win::pin_to_desktop(self.surf.hwnd); // après l'affichage, qui sinon la remonte devant les fenêtres
            self.surf.shown = true;
        }
    }

    pub unsafe fn hide(&mut self) {
        if self.surf.shown {
            self.surf.release();
        }
    }

    /// Le carré et le logo Orbite, rempli selon `value` (batterie, ou session Claude sans souris).
    pub unsafe fn draw(&self, s: f32, theme: usize, value: Option<f32>) {
        if self.surf.bits.is_null() {
            return;
        }
        let look = look(theme, self.surf.light_back);
        let n = (GLYPH * s).round() as usize;
        let ink = look.ink;
        let glyph = Layer { w: n, h: n, px: win::logo_alpha(0, n, value, None).into_iter().map(|a| [ink[0] * a, ink[1] * a, ink[2] * a, a]).collect() };
        let sq = self.square(s);
        let at = ((sq.x + (sq.w - n as f32) / 2.0) as i32, (sq.y + (sq.h - n as f32) / 2.0) as i32);
        self.surf.present(&Frame {
            back: &self.surf.back,
            layers: &[(&glyph, at, 1.0)],
            a: sq,
            b: None,
            smooth: 0.0,
            refr_height: 14.0 * s,
            refr_amount: 20.0 * s,
            glyph: false,
            pills: &[],
            alpha: 1.0,
            scale: s,
            look,
        });
    }
}

/// Coin haut-droit de l'écran principal : DESKTOUCH occupe déjà le haut-gauche.
fn default_pos(s: f32) -> (i32, i32) {
    let mut wa: RECT = unsafe { std::mem::zeroed() };
    unsafe { SystemParametersInfoW(SPI_GETWORKAREA, 0, &mut wa as *mut RECT as _, 0) };
    (wa.right - ((60.0 + SQ) * s) as i32, wa.top + (120.0 * s) as i32)
}

pub unsafe extern "system" fn proc(h: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let xy = || ((lp & 0xFFFF) as i16 as i32, ((lp >> 16) & 0xFFFF) as i16 as i32);
    match msg {
        // ne jamais voler le focus ni passer devant les autres fenêtres
        WM_MOUSEACTIVATE => MA_NOACTIVATE as isize,
        WM_LBUTTONDOWN => {
            app(|a| a.desk.press = Some(xy()));
            SetCapture(h);
            0
        }
        WM_MOUSEMOVE => {
            let (x, y) = xy();
            // bouton enfoncé et la souris bouge : on déplace le carré (Windows suit la souris)
            let drag = app(|a| a.desk.press.is_some_and(|p| (p.0 - x).abs() > 4 || (p.1 - y).abs() > 4)).unwrap_or(false);
            if drag {
                app(|a| a.desk.press = None);
                ReleaseCapture();
                SendMessageW(h, WM_NCLBUTTONDOWN, HTCAPTION as usize, 0);
            }
            0
        }
        WM_LBUTTONUP => {
            ReleaseCapture();
            // clic simple : le panneau
            if app(|a| a.desk.press.take().is_some()).unwrap_or(false) {
                app(|a| a.show_panel());
            }
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
