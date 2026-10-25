//! Verre liquide calculé pixel par pixel sur le CPU : pour un widget de bureau, c'est moins coûteux
//! que de réveiller un pipeline GPU, et ça ne tourne que pendant une animation.
//! Recette adaptée d'AndroidLiquidGlass (Kyant, Apache-2.0) : fond flouté et saturé, lentille de
//! réfraction en bord de forme (profil circulaire), dispersion chromatique, liseré de lumière à 45°.
//! Deux formes peuvent fusionner comme deux gouttes (union lissée) : le carré et le panneau qui en sort.

pub type Rgb = [f32; 3];
pub type Rgba = [f32; 4]; // prémultiplié

/// Fond capturé derrière la fenêtre, déjà flouté.
pub struct Back {
    pub w: usize,
    pub h: usize,
    pub px: Vec<Rgb>,
}

/// Calque de contenu (icônes, libellés), prémultiplié.
pub struct Layer {
    pub w: usize,
    pub h: usize,
    pub px: Vec<Rgba>,
}

impl Layer {
    pub fn empty() -> Self {
        Layer { w: 0, h: 0, px: Vec::new() }
    }
}

impl Back {
    pub fn empty() -> Self {
        Back { w: 1, h: 1, px: vec![[0.0; 3]] }
    }

    fn at(&self, x: usize, y: usize) -> Rgb {
        self.px[y.min(self.h - 1) * self.w + x.min(self.w - 1)]
    }

    fn sample(&self, x: f32, y: f32) -> Rgb {
        let x = x.clamp(0.0, (self.w - 1) as f32);
        let y = y.clamp(0.0, (self.h - 1) as f32);
        let (x0, y0) = (x as usize, y as usize);
        let (fx, fy) = (x - x0 as f32, y - y0 as f32);
        let (a, b, c, d) = (self.at(x0, y0), self.at(x0 + 1, y0), self.at(x0, y0 + 1), self.at(x0 + 1, y0 + 1));
        std::array::from_fn(|i| (a[i] * (1.0 - fx) + b[i] * fx) * (1.0 - fy) + (c[i] * (1.0 - fx) + d[i] * fx) * fy)
    }

    /// Luminance moyenne d'une zone, pour choisir un texte clair ou foncé.
    pub fn luminance(&self, x: usize, y: usize, w: usize, h: usize) -> f32 {
        let (mut sum, mut n) = (0.0, 0);
        for yy in (y..(y + h).min(self.h)).step_by(4) {
            for xx in (x..(x + w).min(self.w)).step_by(4) {
                let p = self.px[yy * self.w + xx];
                sum += 0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2];
                n += 1;
            }
        }
        if n == 0 { 0.0 } else { sum / n as f32 }
    }

    /// « Vibrancy » (saturation ×1,5) puis flou quasi gaussien (3 passes de flou boîte).
    pub fn frost(&mut self, radius: usize) {
        for p in &mut self.px {
            let l = 0.213 * p[0] + 0.715 * p[1] + 0.072 * p[2];
            *p = p.map(|c| (l + (c - l) * 1.5).clamp(0.0, 1.0));
        }
        if radius == 0 || self.w == 0 || self.h == 0 {
            return;
        }
        for _ in 0..3 {
            blur_rows(&mut self.px, self.w, self.h, radius);
        }
        let mut t = transpose(&self.px, self.w, self.h);
        for _ in 0..3 {
            blur_rows(&mut t, self.h, self.w, radius);
        }
        self.px = transpose(&t, self.h, self.w);
    }
}

fn blur_rows(px: &mut [Rgb], w: usize, h: usize, r: usize) {
    let mut line = vec![[0.0; 3]; w];
    let norm = 1.0 / (2 * r + 1) as f32;
    for y in 0..h {
        let row = &px[y * w..(y + 1) * w];
        let get = |i: isize| row[i.clamp(0, w as isize - 1) as usize];
        let mut acc = [0.0f32; 3];
        for i in -(r as isize)..=(r as isize) {
            let p = get(i);
            (0..3).for_each(|c| acc[c] += p[c]);
        }
        for (x, out) in line.iter_mut().enumerate() {
            *out = acc.map(|a| a * norm);
            let (add, sub) = (get(x as isize + r as isize + 1), get(x as isize - r as isize));
            (0..3).for_each(|c| acc[c] += add[c] - sub[c]);
        }
        px[y * w..(y + 1) * w].copy_from_slice(&line);
    }
}

fn transpose(px: &[Rgb], w: usize, h: usize) -> Vec<Rgb> {
    let mut out = vec![[0.0; 3]; w * h];
    for y in 0..h {
        for x in 0..w {
            out[x * h + y] = px[y * w + x];
        }
    }
    out
}

/// Distance signée à un rectangle arrondi centré (négative à l'intérieur).
pub fn sd_rrect(p: (f32, f32), half: (f32, f32), r: f32) -> f32 {
    let (cx, cy) = (p.0.abs() - (half.0 - r), p.1.abs() - (half.1 - r));
    // pas de hypot() : la version de libm, prudente contre les débordements, est très lente
    let (ox, oy) = (cx.max(0.0), cy.max(0.0));
    (ox * ox + oy * oy).sqrt() - r + cx.max(cy).min(0.0)
}

/// Normale (vers l'extérieur) du rectangle arrondi.
fn grad_rrect(p: (f32, f32), half: (f32, f32), r: f32) -> (f32, f32) {
    let (cx, cy) = (p.0.abs() - (half.0 - r), p.1.abs() - (half.1 - r));
    let (gx, gy) = if cx >= 0.0 || cy >= 0.0 {
        norm((cx.max(0.0), cy.max(0.0)))
    } else if cx > cy {
        (1.0, 0.0)
    } else {
        (0.0, 1.0)
    };
    (gx * p.0.signum(), gy * p.1.signum())
}

fn norm(v: (f32, f32)) -> (f32, f32) {
    let l = (v.0 * v.0 + v.1 * v.1).sqrt();
    if l < 1e-6 { (0.0, 0.0) } else { (v.0 / l, v.1 / l) }
}

pub fn smoothstep(a: f32, b: f32, x: f32) -> f32 {
    let t = ((x - a) / (b - a)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn mix(a: Rgb, b: Rgb, t: f32) -> Rgb {
    std::array::from_fn(|i| a[i] + (b[i] - a[i]) * t)
}

/// Rectangle arrondi : x, y, largeur, hauteur, rayon.
#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub struct Rr {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub r: f32,
}

impl Rr {
    pub fn lerp(a: Rr, b: Rr, t: f32) -> Rr {
        let l = |p: f32, q: f32| p + (q - p) * t;
        Rr { x: l(a.x, b.x), y: l(a.y, b.y), w: l(a.w, b.w).max(1.0), h: l(a.h, b.h).max(1.0), r: l(a.r, b.r).max(0.0) }
    }

    pub fn contains(&self, x: f32, y: f32) -> bool {
        self.eval(x, y).0 < 0.0
    }

    /// Distance signée et normale (avec « depth effect » : la normale penche vers le centre).
    fn eval(&self, x: f32, y: f32) -> (f32, (f32, f32)) {
        let half = (self.w * 0.5, self.h * 0.5);
        let p = (x - self.x - half.0, y - self.y - half.1);
        let mh = half.0.min(half.1);
        let r = self.r.min(mh);
        let g = grad_rrect(p, half, (r * 1.5).min(mh));
        let n = norm(p);
        (sd_rrect(p, half, r), norm((g.0 + n.0, g.1 + n.1)))
    }

    fn contains_box(&self, x: f32, y: f32, pad: f32) -> bool {
        x >= self.x - pad && x <= self.x + self.w + pad && y >= self.y - pad && y <= self.y + self.h + pad
    }
}

/// Élément posé sur le verre (bulle de survol, curseur, interrupteur…).
#[derive(Clone, Copy)]
pub struct Pill {
    pub rr: Rr,
    pub color: Rgb,
    pub amount: f32, // opacité du remplissage
    pub rim: f32,    // force du liseré
}

/// Apparence choisie dans les réglages.
#[derive(Clone, Copy)]
pub struct Look {
    pub refraction: f32, // 0 = verre dépoli simple, 1 = lentille complète
    pub tint: Rgb,
    pub tint_amount: f32,
    pub ink: Rgb, // couleur du texte et des 4 points du carré
}

/// Tout ce qu'il faut pour dessiner une image.
pub struct Frame<'a> {
    pub back: &'a Back,
    pub layers: &'a [(&'a Layer, (i32, i32), f32)], // calques (tuiles, textes) : position, opacité ; le dernier dessus
    pub a: Rr,
    pub b: Option<Rr>, // seconde forme fusionnée à la première
    pub smooth: f32,   // rayon de fusion entre les deux formes
    pub refr_height: f32,
    pub refr_amount: f32,
    pub glyph: bool, // 4 points au centre de la forme `a`
    pub pills: &'a [Pill],
    pub alpha: f32,
    pub scale: f32,
    pub look: Look,
}

impl Frame<'_> {
    fn bounds(&self) -> Rr {
        let Some(b) = self.b else { return self.a };
        let (x, y) = (self.a.x.min(b.x), self.a.y.min(b.y));
        let (x2, y2) = ((self.a.x + self.a.w).max(b.x + b.w), (self.a.y + self.a.h).max(b.y + b.h));
        Rr { x, y, w: x2 - x, h: y2 - y, r: 0.0 }
    }

    /// Union lissée des deux formes : distance et normale.
    fn shape(&self, x: f32, y: f32) -> (f32, (f32, f32)) {
        let (da, ga) = self.a.eval(x, y);
        let Some(b) = self.b else { return (da, ga) };
        let (db, gb) = b.eval(x, y);
        let k = self.smooth.max(1e-3);
        let h = (0.5 + 0.5 * (db - da) / k).clamp(0.0, 1.0);
        let d = db + (da - db) * h - k * h * (1.0 - h);
        (d, norm((gb.0 + (ga.0 - gb.0) * h, gb.1 + (ga.1 - gb.1) * h)))
    }
}

/// Dessine dans `out` (BGRA prémultiplié, lignes de `w` pixels), en parallèle par bandes.
pub fn render(f: &Frame, out: &mut [u32], w: usize) {
    let h = out.len() / w;
    // on ne calcule que la forme et son ombre ; le reste est transparent
    let bb = f.bounds();
    let m = 26.0 * f.scale;
    let (x0, x1) = ((bb.x - m).max(0.0) as usize, ((bb.x + bb.w + m).ceil().max(0.0) as usize).min(w));
    let (y0, y1) = ((bb.y - m).max(0.0) as usize, ((bb.y + bb.h + m + 5.0 * f.scale).ceil().max(0.0) as usize).min(h));
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get()).min(4);
    let band = h.div_ceil(threads).max(1);
    std::thread::scope(|s| {
        for (i, chunk) in out.chunks_mut(band * w).enumerate() {
            s.spawn(move || {
                for (j, row) in chunk.chunks_mut(w).enumerate() {
                    let y = i * band + j;
                    row.fill(0);
                    if (y0..y1).contains(&y) {
                        for (x, px) in row.iter_mut().enumerate().take(x1).skip(x0) {
                            *px = shade(f, bb, x as f32 + 0.5, y as f32 + 0.5);
                        }
                    }
                }
            });
        }
    });
}

fn shade(f: &Frame, bb: Rr, x: f32, y: f32) -> u32 {
    let s = f.scale;
    let (sd, g) = f.shape(x, y);

    // ombre portée douce, décalée vers le bas
    let cov = (0.5 - sd).clamp(0.0, 1.0);
    let shadow = if cov >= 1.0 {
        0.0 // entièrement recouverte par le verre
    } else {
        0.22 * (1.0 - smoothstep(-6.0 * s, 20.0 * s, f.shape(x, y - 5.0 * s).0))
    };
    if cov <= 0.0 {
        return pack([0.0; 3], shadow * f.alpha);
    }

    // réfraction : près du bord, on lit le fond plus vers l'intérieur, comme à travers une lentille
    let inner = -sd;
    let amount = f.refr_amount * f.look.refraction;
    let mut col = if amount > 0.0 && inner < f.refr_height {
        let t = 1.0 - inner.max(0.0) / f.refr_height;
        let d = (1.0 - (1.0 - t * t).max(0.0).sqrt()) * amount;
        let (bx, by) = (x - d * g.0, y - d * g.1);
        // dispersion : plus forte dans les coins, nulle au milieu des côtés
        let (cx, cy) = (x - bb.x - bb.w * 0.5, y - bb.y - bb.h * 0.5);
        let k = 0.6 * (cx * cy) / (bb.w * bb.h * 0.25);
        let (dx, dy) = (d * g.0 * k, d * g.1 * k);
        [f.back.sample(bx - dx, by - dy)[0], f.back.sample(bx, by)[1], f.back.sample(bx + dx, by + dy)[2]]
    } else {
        f.back.at(x as usize, y as usize)
    };

    col = mix(col, f.look.tint, f.look.tint_amount);
    // léger reflet en haut de la forme
    let top = 1.0 - ((y - bb.y) / bb.h).clamp(0.0, 1.0);
    col = mix(col, [1.0; 3], 0.07 * top * top);

    // éléments posés sur le verre
    for p in f.pills {
        if p.amount <= 0.003 || !p.rr.contains_box(x, y, 1.0) {
            continue;
        }
        let (psd, pg) = p.rr.eval(x, y);
        let pc = (0.5 - psd).clamp(0.0, 1.0);
        if pc > 0.0 {
            col = mix(col, p.color, pc * p.amount);
            let rim = (1.0 - (-psd) / (1.4 * s)).clamp(0.0, 1.0);
            if rim > 0.0 && p.rim > 0.0 {
                let light = (pg.0 + pg.1).abs() * 0.7071;
                col = mix(col, [1.0; 3], pc * rim * p.rim * (0.35 + 0.65 * light));
            }
        }
    }

    // les 4 points du carré
    if f.glyph {
        let a = f.a;
        let p = (x - a.x - a.w * 0.5, y - a.y - a.h * 0.5);
        // mêmes proportions que le logo (assets/desktouch.svg) : points de 38/224, écart 22/224, rayon 11/38
        let (size, gap) = (a.w * 38.0 / 224.0, a.w * 22.0 / 224.0);
        let q = (p.0.abs() - (gap + size) * 0.5, p.1.abs() - (gap + size) * 0.5);
        let gc = (0.5 - sd_rrect(q, (size * 0.5, size * 0.5), size * 11.0 / 38.0)).clamp(0.0, 1.0) * 0.92;
        col = mix(col, f.look.ink, gc);
    }

    // contenu (icônes, libellés)
    for &(layer, (px, py), alpha) in f.layers {
        let (lx, ly) = (x as i32 - px, y as i32 - py);
        if alpha > 0.003 && lx >= 0 && ly >= 0 && (lx as usize) < layer.w && (ly as usize) < layer.h {
            let c = layer.px[ly as usize * layer.w + lx as usize];
            let a = c[3] * alpha;
            col = std::array::from_fn(|i| c[i] * alpha + col[i] * (1.0 - a));
        }
    }

    // liseré : lumière directionnelle à 45° (haut-gauche, bas-droite) plus un peu tout autour
    let rim = (1.0 - inner / (1.3 * s)).clamp(0.0, 1.0);
    if rim > 0.0 {
        let light = (g.0 + g.1).abs() * 0.7071;
        col = mix(col, [1.0; 3], rim * (0.18 + 0.55 * light));
    }

    // l'intérieur recouvre l'ombre
    let a = cov + shadow * (1.0 - cov);
    let rgb = col.map(|c| c * cov / a.max(1e-6));
    pack(rgb, a * f.alpha)
}

/// Couleur droite + opacité → BGRA prémultiplié.
pub fn pack(rgb: Rgb, a: f32) -> u32 {
    let a = a.clamp(0.0, 1.0);
    let ch = |c: f32| ((c.clamp(0.0, 1.0) * a * 255.0 + 0.5) as u32).min(255);
    (((a * 255.0 + 0.5) as u32) << 24) | ch(rgb[0]) << 16 | ch(rgb[1]) << 8 | ch(rgb[2])
}

/// Ressort amorti (masse 1), intégré à petits pas pour rester stable quel que soit le taux d'images.
#[derive(Clone, Copy)]
pub struct Spring {
    pub x: f32,
    pub v: f32,
    pub target: f32,
    pub stiffness: f32,
    pub damping: f32, // 1 = critique, < 1 = léger rebond
}

impl Spring {
    pub const fn new(x: f32, stiffness: f32, damping: f32) -> Self {
        Spring { x, v: 0.0, target: x, stiffness, damping }
    }

    pub fn step(&mut self, dt: f32) {
        let c = 2.0 * self.damping * self.stiffness.sqrt();
        let n = (dt / 0.002).ceil().max(1.0);
        let h = dt / n;
        for _ in 0..n as usize {
            self.v += (-self.stiffness * (self.x - self.target) - c * self.v) * h;
            self.x += self.v * h;
        }
        if self.rest() {
            self.x = self.target;
            self.v = 0.0;
        }
    }

    pub fn rest(&self) -> bool {
        (self.x - self.target).abs() < 5e-4 && self.v.abs() < 5e-3
    }

    pub fn snap(&mut self, x: f32) {
        self.x = x;
        self.target = x;
        self.v = 0.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sdf() {
        assert!((sd_rrect((0.0, 0.0), (50.0, 20.0), 10.0) + 20.0).abs() < 1e-4);
        assert!(sd_rrect((50.0, 0.0), (50.0, 20.0), 10.0).abs() < 1e-4);
        assert!(sd_rrect((60.0, 0.0), (50.0, 20.0), 10.0) > 9.9);
        // coin arrondi : le coin exact du rectangle est dehors
        assert!(sd_rrect((50.0, 20.0), (50.0, 20.0), 10.0) > 4.0);
    }

    fn frame<'a>(back: &'a Back, _layer: &'a Layer, b: Option<Rr>, smooth: f32) -> Frame<'a> {
        Frame {
            back, layers: &[],
            a: Rr { x: 10.0, y: 10.0, w: 40.0, h: 40.0, r: 12.0 }, b, smooth,
            refr_height: 10.0, refr_amount: 20.0, glyph: false, pills: &[], alpha: 1.0, scale: 1.0,
            look: Look { refraction: 1.0, tint: [1.0; 3], tint_amount: 0.0, ink: [1.0; 3] },
        }
    }

    #[test]
    fn smooth_union_bridges_close_shapes_only() {
        let (back, layer) = (Back::empty(), Layer::empty());
        let b = Rr { x: 58.0, y: 10.0, w: 40.0, h: 40.0, r: 12.0 }; // 8 px d'écart
        // point au milieu de l'écart
        assert!(frame(&back, &layer, Some(b), 20.0).shape(54.0, 30.0).0 < 0.0, "pont liquide");
        assert!(frame(&back, &layer, Some(b), 2.0).shape(54.0, 30.0).0 > 0.0, "formes séparées");
        // loin de l'autre forme, l'union ne change rien
        let alone = frame(&back, &layer, None, 0.0).shape(20.0, 30.0).0;
        assert!((frame(&back, &layer, Some(b), 2.0).shape(20.0, 30.0).0 - alone).abs() < 1e-4);
    }

    #[test]
    fn spring_settles_with_small_overshoot() {
        let mut s = Spring::new(0.0, 260.0, 0.75);
        s.target = 1.0;
        let mut peak: f32 = 0.0;
        for _ in 0..240 {
            s.step(1.0 / 120.0);
            peak = peak.max(s.x);
        }
        assert!(s.rest() && s.x == 1.0);
        assert!(peak > 1.0 && peak < 1.06);
    }

    #[test]
    fn frost_keeps_flat_color() {
        let mut b = Back { w: 20, h: 10, px: vec![[0.5, 0.5, 0.5]; 200] };
        b.frost(3);
        assert!(b.px.iter().all(|p| (p[0] - 0.5).abs() < 1e-4));
    }

    #[test]
    fn pack_is_premultiplied() {
        assert_eq!(pack([1.0; 3], 1.0), 0xFFFF_FFFF);
        assert_eq!(pack([1.0; 3], 0.0), 0);
        assert_eq!(pack([1.0, 0.0, 0.0], 0.5) >> 16 & 255, 128);
    }
}
