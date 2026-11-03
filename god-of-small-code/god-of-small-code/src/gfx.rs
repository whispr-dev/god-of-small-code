//! Software renderer for the shrine: a stone brazier, an eternal flame and
//! the act of consumption. Pure Rust, no Win32, so it is testable anywhere.
//!
//! Output pixels are premultiplied BGRA packed as 0xAARRGGBB, which is what a
//! 32-bit DIB section feeding UpdateLayeredWindow(ULW_ALPHA) expects.
//!
//! Layout (logical units, scaled by the DPI factor `sf`):
//!   back layer  : near-invisible hit-test fill + bowl interior + back rim
//!   flame       : heat grid, bilinear upscaled through a colour LUT
//!   sprites     : offerings falling into the flame and charring
//!   embers      : tiny rising sparks
//!   front layer : front rim + bowl body + plinth (hides the flame's base)

use crate::oracle::Rng;

pub const LOGICAL_W: f32 = 128.0;
pub const LOGICAL_H: f32 = 160.0;

const GW: usize = 32;
const GH: usize = 48;
const CELL: f32 = 2.0;
const GRID_X0: f32 = 64.0 - GW as f32 * CELL / 2.0;
const GRID_Y1: f32 = 104.0;
const GRID_Y0: f32 = GRID_Y1 - GH as f32 * CELL;
const TICK: f32 = 0.05; // flame simulation step (20 Hz)
const FALL_TIME: f32 = 1.5; // seconds from the top of the window to ashes
const IGNITE_T: f32 = 0.6; // fraction of the fall at which a page meets the flame
const HIT_TOP: f32 = 40.0; // rows above this (logical) are click-through
const MAX_EMBERS: usize = 64;
const MAX_SPRITES: usize = 12;
const LUT_LEVELS: usize = 9;

const RIM_CX: f32 = 64.0;
const RIM_CY: f32 = 96.0;

type Px = [f32; 4]; // premultiplied r, g, b, a in 0..1

// ---------------------------------------------------------------- helpers

fn smoothstep(a: f32, b: f32, x: f32) -> f32 {
    let t = ((x - a) / (b - a)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

fn pack(p: Px) -> u32 {
    let a = (p[3].clamp(0.0, 1.0) * 255.0 + 0.5) as u32;
    let c = |v: f32| (((v.clamp(0.0, 1.0)) * 255.0 + 0.5) as u32).min(a);
    (a << 24) | (c(p[0]) << 16) | (c(p[1]) << 8) | c(p[2])
}

fn solid(rgb: [f32; 3], a: f32) -> Px {
    [rgb[0] * a, rgb[1] * a, rgb[2] * a, a]
}

fn over_f(dst: Px, src: Px) -> Px {
    let k = 1.0 - src[3];
    [
        src[0] + dst[0] * k,
        src[1] + dst[1] * k,
        src[2] + dst[2] * k,
        src[3] + dst[3] * k,
    ]
}

/// Premultiplied "source over" on packed pixels.
#[inline]
fn over(dst: u32, src: u32) -> u32 {
    let sa = src >> 24;
    if sa == 255 {
        return src;
    }
    if sa == 0 {
        return dst;
    }
    let inv = 255 - sa;
    let ch = |sh: u32| -> u32 {
        let s = (src >> sh) & 0xff;
        let d = (dst >> sh) & 0xff;
        (s + (d * inv + 127) / 255).min(255)
    };
    (ch(24) << 24) | (ch(16) << 16) | (ch(8) << 8) | ch(0)
}

/// Coverage (0..1) from a signed distance in logical units (negative = inside).
fn cov(d: f32, sf: f32) -> f32 {
    (0.5 - d * sf).clamp(0.0, 1.0)
}

/// First-order signed distance to an axis-aligned ellipse.
fn ellipse_d(x: f32, y: f32, cx: f32, cy: f32, rx: f32, ry: f32) -> f32 {
    let dx = x - cx;
    let dy = y - cy;
    let f = (dx / rx) * (dx / rx) + (dy / ry) * (dy / ry);
    let gx = dx / (rx * rx);
    let gy = dy / (ry * ry);
    let g = (gx * gx + gy * gy).sqrt() * 2.0;
    if g < 1e-6 {
        return -rx.min(ry);
    }
    (f - 1.0) / g
}

fn box_d(dx: f32, dy: f32, hx: f32, hy: f32) -> f32 {
    let qx = dx.abs() - hx;
    let qy = dy.abs() - hy;
    qx.max(0.0).hypot(qy.max(0.0)) + qx.max(qy).min(0.0)
}

fn seg_d(px: f32, py: f32, ax: f32, ay: f32, bx: f32, by: f32) -> f32 {
    let (vx, vy) = (bx - ax, by - ay);
    let (wx, wy) = (px - ax, py - ay);
    let t = ((wx * vx + wy * vy) / (vx * vx + vy * vy)).clamp(0.0, 1.0);
    (wx - vx * t).hypot(wy - vy * t)
}

/// 0..1 ramp used to split shapes at a horizontal line (soft by one pixel).
fn half(x: f32, sf: f32) -> f32 {
    (0.5 + x * sf).clamp(0.0, 1.0)
}

// ------------------------------------------------------------ static art

fn paint_back(u: f32, v: f32, sf: f32) -> Px {
    // alpha 1/255 over the shrine proper: invisible, but not click-through.
    // The empty strip above (where pages fall in) stays fully click-through.
    let mut p: Px = [0.0, 0.0, 0.0, if v >= HIT_TOP { 1.0 / 255.0 } else { 0.0 }];

    let c_out = cov(ellipse_d(u, v, RIM_CX, RIM_CY, 47.0, 11.0), sf);
    let c_in = cov(ellipse_d(u, v, RIM_CX, RIM_CY, 42.5, 8.5), sf);

    if c_in > 0.0 {
        let g = ((v - 87.5) / 17.0).clamp(0.0, 1.0);
        p = over_f(
            p,
            solid([0.09 + 0.25 * g, 0.04 + 0.10 * g, 0.02 + 0.03 * g], c_in),
        );
    }
    let ring = (c_out - c_in).max(0.0) * half(RIM_CY - v, sf);
    if ring > 0.0 {
        let lit = 0.72 + 0.28 * ((RIM_CY - v) / 11.0).clamp(0.0, 1.0);
        p = over_f(p, solid([0.45 * lit, 0.47 * lit, 0.55 * lit], ring));
    }
    p
}

fn stone(lit: f32) -> [f32; 3] {
    [0.30 * lit, 0.32 * lit, 0.40 * lit]
}

fn paint_front(u: f32, v: f32, sf: f32) -> Px {
    let mut p: Px = [0.0; 4];

    // plinth: two steps and a neck, painted first so the bowl sits in front
    for (cy, hx, hy) in [(157.25_f32, 30.0_f32, 2.75_f32), (152.5, 18.0, 3.5), (142.0, 6.5, 8.0)] {
        let c = cov(box_d(u - 64.0, v - cy, hx, hy), sf);
        if c > 0.0 {
            let nx = ((u - 64.0) / hx).clamp(-1.0, 1.0);
            let mut lit = 0.45 + 0.45 * (1.0 - nx * nx).max(0.0);
            if v - (cy - hy) < 1.0 {
                lit += 0.18; // top edge catches the light
            }
            p = over_f(p, solid(stone(lit), c));
        }
    }

    // bowl body: lower half of an ellipse, minus the opening
    let c_out = cov(ellipse_d(u, v, RIM_CX, RIM_CY, 47.0, 11.0), sf);
    let c_in = cov(ellipse_d(u, v, RIM_CX, RIM_CY, 42.5, 8.5), sf);
    let body_d = ellipse_d(u, v, 64.0, 96.0, 46.5, 42.0).max(96.0 - v);
    let body = cov(body_d, sf) * (1.0 - c_out);
    if body > 0.0 {
        let nx = ((u - 64.0) / 46.5).clamp(-1.0, 1.0);
        let lit = (1.0 - nx * nx).max(0.0).powf(0.6);
        let shade = (0.35 + 0.65 * lit) * (1.0 - 0.35 * ((v - 96.0) / 42.0).clamp(0.0, 1.0));
        let mut rgb = stone(shade * 1.25);
        // firelight on the upper belly
        let glow = (-((v - 104.0).max(0.0)) / 12.0).exp() * 0.55 * (0.5 + 0.5 * lit);
        rgb[0] += 0.95 * glow;
        rgb[1] += 0.42 * glow;
        rgb[2] += 0.12 * glow;
        // two engraved bands
        for by in [122.0_f32, 127.5] {
            if (v - by).abs() < 0.7 {
                rgb = [rgb[0] * 0.55, rgb[1] * 0.55, rgb[2] * 0.55];
            }
        }
        // the sigil: a terminal prompt, glowing amber
        let dg = seg_d(u, v, 52.0, 113.0, 61.0, 121.0)
            .min(seg_d(u, v, 61.0, 121.0, 52.0, 129.0))
            .min(seg_d(u, v, 66.0, 130.0, 78.0, 130.0))
            - 1.25;
        let halo = (-(dg.max(0.0)) / 2.2).exp() * 0.25;
        rgb[0] += halo;
        rgb[1] += halo * 0.65;
        rgb[2] += halo * 0.25;
        let gc = cov(dg, sf);
        rgb = [
            lerp(rgb[0], 1.0, gc),
            lerp(rgb[1], 0.78, gc),
            lerp(rgb[2], 0.38, gc),
        ];
        p = over_f(p, solid(rgb, body));
    }

    // front half of the rim, on top of everything
    let ring = (c_out - c_in).max(0.0) * half(v - RIM_CY, sf);
    if ring > 0.0 {
        let lit = 0.95 - 0.25 * ((v - RIM_CY) / 11.0).clamp(0.0, 1.0);
        p = over_f(p, solid([0.45 * lit, 0.47 * lit, 0.55 * lit], ring));
    }
    p
}

// ----------------------------------------------------------- flame colour

type Stops = [(f32, [f32; 3]); 5];

const ORANGE: Stops = [
    (0.00, [0.55, 0.05, 0.00]),
    (0.30, [0.95, 0.25, 0.02]),
    (0.60, [1.00, 0.65, 0.10]),
    (0.85, [1.00, 0.90, 0.45]),
    (1.10, [1.00, 1.00, 0.85]),
];
const EMERALD: Stops = [
    (0.00, [0.00, 0.30, 0.12]),
    (0.30, [0.05, 0.65, 0.25]),
    (0.60, [0.30, 0.95, 0.45]),
    (0.85, [0.70, 1.00, 0.70]),
    (1.10, [0.95, 1.00, 0.95]),
];

fn ramp(stops: &Stops, h: f32) -> [f32; 3] {
    if h <= stops[0].0 {
        return stops[0].1;
    }
    for i in 1..stops.len() {
        if h <= stops[i].0 {
            let (h0, c0) = stops[i - 1];
            let (h1, c1) = stops[i];
            let t = (h - h0) / (h1 - h0);
            return [lerp(c0[0], c1[0], t), lerp(c0[1], c1[1], t), lerp(c0[2], c1[2], t)];
        }
    }
    stops[stops.len() - 1].1
}

fn build_lut(mix: f32) -> [u32; 256] {
    let mut lut = [0u32; 256];
    for (i, slot) in lut.iter_mut().enumerate() {
        let h = i as f32 / 200.0;
        let a = smoothstep(0.05, 0.5, h);
        let o = ramp(&ORANGE, h);
        let e = ramp(&EMERALD, h);
        let rgb = [lerp(o[0], e[0], mix), lerp(o[1], e[1], mix), lerp(o[2], e[2], mix)];
        *slot = pack(solid(rgb, a));
    }
    lut
}

fn sample_row(row: &[f32; GW], xf: f32) -> f32 {
    let x0 = xf.floor();
    let f = xf - x0;
    let i = x0 as i32;
    let g = |i: i32| -> f32 {
        if i < 0 || i >= GW as i32 {
            0.0
        } else {
            row[i as usize]
        }
    };
    g(i) * (1.0 - f) + g(i + 1) * f
}

// ---------------------------------------------------------------- state

struct Ember {
    x: f32,
    y: f32,
    vx: f32,
    vy: f32,
    life: f32,
    max: f32,
}

struct Falling {
    t: f32, // < 0: waiting its turn, 0..1: falling and burning
    x0: f32,
    ph: f32,
}

pub struct Gfx {
    w: usize,
    h: usize,
    sf: f32,
    back: Vec<u32>,
    front: Vec<u32>,
    front_rows: (usize, usize),
    luts: Vec<[u32; 256]>,
    heat: [[f32; GW]; GH],
    rng: Rng,
    t: f32,
    acc: f32,
    burn: f32,
    recoil: f32,
    holy: f32,
    ember_acc: f32,
    embers: Vec<Ember>,
    falling: Vec<Falling>,
}

impl Gfx {
    pub fn new(sf: f32, seed: u64) -> Gfx {
        let sf = sf.clamp(0.75, 4.0);
        let w = (LOGICAL_W * sf).round() as usize;
        let h = (LOGICAL_H * sf).round() as usize;
        let mut back = vec![0u32; w * h];
        let mut front = vec![0u32; w * h];
        let (mut first, mut last) = (h, 0usize);
        for py in 0..h {
            let v = (py as f32 + 0.5) / sf;
            for px in 0..w {
                let u = (px as f32 + 0.5) / sf;
                back[py * w + px] = pack(paint_back(u, v, sf));
                let f = pack(paint_front(u, v, sf));
                front[py * w + px] = f;
                if f >> 24 != 0 {
                    first = first.min(py);
                    last = last.max(py);
                }
            }
        }
        let luts = (0..LUT_LEVELS)
            .map(|i| build_lut(i as f32 / (LUT_LEVELS - 1) as f32))
            .collect();
        let mut g = Gfx {
            w,
            h,
            sf,
            back,
            front,
            front_rows: (first.min(h), (last + 1).min(h)),
            luts,
            heat: [[0.0; GW]; GH],
            rng: Rng::new(seed),
            t: 0.0,
            acc: 0.0,
            burn: 0.0,
            recoil: 0.0,
            holy: 0.0,
            ember_acc: 0.0,
            embers: Vec::new(),
            falling: Vec::new(),
        };
        for _ in 0..90 {
            g.tick(); // pre-warm so the first frame already has a developed flame
        }
        g
    }

    pub fn size(&self) -> (usize, usize) {
        (self.w, self.h)
    }

    /// Feed `n` offerings to the flame (staggered, at most MAX_SPRITES).
    pub fn offer(&mut self, n: usize) {
        for i in 0..n.min(MAX_SPRITES) {
            if self.falling.len() >= MAX_SPRITES {
                break;
            }
            let x0 = (self.rng.f32() - 0.5) * 20.0;
            let ph = self.rng.f32() * std::f32::consts::TAU;
            self.falling.push(Falling { t: -(i as f32) * 0.22, x0, ph });
        }
    }

    /// The flame sputters (a refused offering).
    pub fn recoil(&mut self) {
        self.recoil = 1.0;
    }

    /// The flame turns emerald for a moment (a prayer was answered).
    pub fn holy(&mut self) {
        self.holy = 1.0;
    }

    /// A little roar, for when the shrine is summoned.
    pub fn flare(&mut self) {
        self.burn = 1.0;
    }

    /// True while something is animating faster than the idle flicker.
    pub fn busy(&self) -> bool {
        !self.falling.is_empty() || self.burn > 0.02 || self.recoil > 0.02 || self.holy > 0.02
    }

    pub fn step(&mut self, dt: f32) {
        let dt = dt.clamp(0.0, 0.25);

        self.acc += dt;
        let mut n = 0;
        while self.acc >= TICK && n < 4 {
            self.tick();
            self.acc -= TICK;
            n += 1;
        }
        if n == 4 {
            self.acc = 0.0;
        }

        for f in &mut self.falling {
            f.t += dt / FALL_TIME;
        }
        self.falling.retain(|f| f.t < 1.05);

        let burning = self.falling.iter().filter(|f| f.t >= IGNITE_T && f.t <= 1.0).count();
        let target = (0.5 * burning as f32).min(1.0);
        let k = 1.0 - (-dt * 6.0).exp();
        self.burn += (target - self.burn) * k;
        if burning == 0 && self.burn < 0.02 {
            self.burn = 0.0;
        }
        self.recoil = (self.recoil - dt * 1.2).max(0.0);
        self.holy = (self.holy - dt * 0.5).max(0.0);

        // embers
        self.ember_acc += dt * (2.5 + 40.0 * self.burn);
        while self.ember_acc >= 1.0 {
            self.ember_acc -= 1.0;
            if self.embers.len() < MAX_EMBERS {
                let spread = 10.0 + 10.0 * self.burn;
                let life = 0.8 + self.rng.f32() * 1.0;
                let e = Ember {
                    x: 64.0 + (self.rng.f32() - 0.5) * 2.0 * spread,
                    y: 92.0 - self.rng.f32() * 10.0,
                    vx: (self.rng.f32() - 0.5) * 10.0,
                    vy: -(18.0 + self.rng.f32() * 28.0),
                    life,
                    max: life,
                };
                self.embers.push(e);
            }
        }
        for e in &mut self.embers {
            e.x += e.vx * dt;
            e.y += e.vy * dt;
            e.vx += (self.rng.f32() - 0.5) * 40.0 * dt;
            e.life -= dt;
        }
        self.embers.retain(|e| e.life > 0.0 && e.y > -4.0);
    }

    #[allow(clippy::needless_range_loop)] // indices are the point: x also feeds the maths
    fn tick(&mut self) {
        self.t += TICK;
        let power = (1.0 + 0.35 * self.burn) * (1.0 - 0.75 * self.recoil);
        let cool_scale = 1.0 - 0.20 * self.burn;
        let width = 0.70 + 0.25 * self.burn;

        for x in 0..GW {
            let cx = (x as f32 + 0.5) / GW as f32 * 2.0 - 1.0;
            let env = (1.0 - (cx / width) * (cx / width)).max(0.0);
            let flick = 0.8 + 0.2 * self.rng.f32();
            self.heat[GH - 1][x] = 1.1 * power * env * flick;
        }

        for y in 0..GH - 1 {
            let fy = y as f32;
            let r1 = self.heat[y + 1];
            let r2 = if y + 2 < GH { self.heat[y + 2] } else { r1 };
            let sway = (self.t * 2.1 + fy * 0.33).sin() * 0.9 + (self.t * 0.9 - fy * 0.11).sin() * 0.7;
            let amp = 0.24 + 0.54 * (1.0 - fy / GH as f32);
            for x in 0..GW {
                let cx = (x as f32 + 0.5) / GW as f32 * 2.0 - 1.0;
                let xs = x as f32 + sway * amp;
                let below = sample_row(&r1, xs);
                let side = (sample_row(&r1, xs - 1.0) + sample_row(&r1, xs + 1.0)) * 0.5;
                let avg = below * 0.55 + side * 0.30 + r2[x] * 0.15;
                let cool = (0.028 + 0.022 * self.rng.f32()) * cool_scale + cx * cx * 0.03;
                self.heat[y][x] = (avg - cool).max(0.0);
            }
        }
    }

    // -------------------------------------------------------------- render

    pub fn render(&self, out: &mut [u32]) {
        assert_eq!(out.len(), self.w * self.h);
        out.copy_from_slice(&self.back);
        self.draw_flame(out);
        self.draw_falling(out);
        self.draw_embers(out);
        let (r0, r1) = self.front_rows;
        let span = r0 * self.w..r1 * self.w;
        for (o, &s) in out[span.clone()].iter_mut().zip(&self.front[span]) {
            if s >> 24 == 255 {
                *o = s;
            } else if s != 0 {
                *o = over(*o, s);
            }
        }
    }

    fn draw_flame(&self, out: &mut [u32]) {
        let sf = self.sf;
        let w = self.w;
        let lut = &self.luts[((self.holy * 1.6).min(1.0) * (LUT_LEVELS - 1) as f32).round() as usize];
        let px0 = (GRID_X0 * sf).floor().max(0.0) as usize;
        let px1 = (((GRID_X0 + GW as f32 * CELL) * sf).ceil() as usize).min(w);
        let py0 = (GRID_Y0 * sf).floor().max(0.0) as usize;
        let py1 = ((GRID_Y1 * sf).ceil() as usize).min(self.h);
        let hv = |yy: i32, xx: i32| -> f32 {
            if yy < 0 || yy >= GH as i32 || xx < 0 || xx >= GW as i32 {
                0.0
            } else {
                self.heat[yy as usize][xx as usize]
            }
        };
        for py in py0..py1 {
            let v = (py as f32 + 0.5) / sf;
            let gy = (v - GRID_Y0) / CELL - 0.5;
            let y0f = gy.floor();
            let fy = gy - y0f;
            let y0 = y0f as i32;
            for px in px0..px1 {
                let u = (px as f32 + 0.5) / sf;
                let gx = (u - GRID_X0) / CELL - 0.5;
                let x0f = gx.floor();
                let fx = gx - x0f;
                let x0 = x0f as i32;
                let top = hv(y0, x0) * (1.0 - fx) + hv(y0, x0 + 1) * fx;
                let bot = hv(y0 + 1, x0) * (1.0 - fx) + hv(y0 + 1, x0 + 1) * fx;
                let h = top * (1.0 - fy) + bot * fy;
                if h < 0.04 {
                    continue;
                }
                let src = lut[((h * 200.0) as usize).min(255)];
                let i = py * w + px;
                out[i] = over(out[i], src);
            }
        }
    }

    fn draw_falling(&self, out: &mut [u32]) {
        let sf = self.sf;
        for f in &self.falling {
            if f.t < 0.0 {
                continue;
            }
            let t = f.t.clamp(0.0, 1.0);
            // fast drop through the air, then a slow smouldering sink into the flame
            let cy = if t < IGNITE_T {
                lerp(4.0, 58.0, (t / IGNITE_T).powf(1.5))
            } else {
                lerp(58.0, 88.0, ((t - IGNITE_T) / (1.0 - IGNITE_T)).powf(0.8))
            };
            let cx = 64.0 + f.x0 * (1.0 - t) + (t * 9.0 + f.ph).sin() * 2.5 * (1.0 - t);
            let burn_t = smoothstep(IGNITE_T, 1.0, t);
            let scale = 1.0 - 0.75 * smoothstep(IGNITE_T + 0.02, 1.0, t);
            let fade = 1.0 - smoothstep(0.92, 1.04, f.t);
            let (s, c) = (0.5 * (t * 7.0 + f.ph).sin()).sin_cos();
            let bf = burn_t * 8.0;
            let rad = (7.0_f32 * 7.0 + 9.0 * 9.0).sqrt() * scale + 2.0;
            let x0 = (((cx - rad) * sf).floor().max(0.0)) as usize;
            let x1 = (((cx + rad) * sf).ceil().max(0.0) as usize).min(self.w);
            let y0 = (((cy - rad) * sf).floor().max(0.0)) as usize;
            let y1 = (((cy + rad) * sf).ceil().max(0.0) as usize).min(self.h);
            for py in y0..y1 {
                let dv = (py as f32 + 0.5) / sf - cy;
                for px in x0..x1 {
                    let du = (px as f32 + 0.5) / sf - cx;
                    let lx = (du * c + dv * s) / scale;
                    let ly = (-du * s + dv * c) / scale;
                    let de = (7.0 - lx.abs()).min(9.0 - ly.abs()) * scale;
                    let a = (0.5 + de * sf).clamp(0.0, 1.0) * fade;
                    if a <= 0.0 {
                        continue;
                    }
                    let mut rgb = [0.93, 0.91, 0.84];
                    if lx.abs() < 4.6 {
                        for k in [-4.5_f32, -0.5, 3.5] {
                            if (ly - k).abs() < 0.6 {
                                rgb = [0.40, 0.45, 0.60];
                            }
                        }
                    }
                    if de < bf {
                        rgb = [0.09, 0.07, 0.06];
                    } else if de < bf + 1.8 {
                        rgb = [1.0, 0.55, 0.10];
                    }
                    let i = py * self.w + px;
                    out[i] = over(out[i], pack(solid(rgb, a)));
                }
            }
        }
    }

    fn draw_embers(&self, out: &mut [u32]) {
        let sf = self.sf;
        for e in &self.embers {
            let lf = (e.life / e.max).clamp(0.0, 1.0);
            let r = 1.1_f32;
            let rgb = [lerp(0.9, 1.0, lf), lerp(0.25, 0.85, lf), lerp(0.05, 0.35, lf)];
            let x0 = (((e.x - r - 1.0) * sf).floor().max(0.0)) as usize;
            let x1 = (((e.x + r + 1.0) * sf).ceil().max(0.0) as usize).min(self.w);
            let y0 = (((e.y - r - 1.0) * sf).floor().max(0.0)) as usize;
            let y1 = (((e.y + r + 1.0) * sf).ceil().max(0.0) as usize).min(self.h);
            for py in y0..y1 {
                let dy = (py as f32 + 0.5) / sf - e.y;
                for px in x0..x1 {
                    let dx = (px as f32 + 0.5) / sf - e.x;
                    let a = (1.0 - dx.hypot(dy) / r).clamp(0.0, 1.0) * lf * 0.95;
                    if a > 0.0 {
                        let i = py * self.w + px;
                        out[i] = over(out[i], pack(solid(rgb, a)));
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Writes PPM frames (composited on a grey desktop) when SHRINE_DUMP is set,
    /// so the look can be inspected on a machine without a Windows desktop.
    fn dump(g: &Gfx, name: &str) {
        let Ok(dir) = std::env::var("SHRINE_DUMP") else { return };
        let (w, h) = g.size();
        let mut buf = vec![0u32; w * h];
        g.render(&mut buf);
        let mut ppm = format!("P6\n{} {}\n255\n", w, h).into_bytes();
        for p in buf {
            let a = (p >> 24) & 0xff;
            let bg = [58u32, 64, 76];
            let ch = |sh: u32, b: u32| (((p >> sh) & 0xff) + b * (255 - a) / 255).min(255) as u8;
            ppm.extend_from_slice(&[ch(16, bg[0]), ch(8, bg[1]), ch(0, bg[2])]);
        }
        std::fs::write(format!("{dir}/{name}.ppm"), ppm).unwrap();
    }

    #[test]
    fn renders_without_panicking_at_many_scales() {
        for sf in [1.0, 1.25, 1.5, 2.0, 3.0] {
            let mut g = Gfx::new(sf, 42);
            g.offer(5);
            let (w, h) = g.size();
            let mut buf = vec![0u32; w * h];
            for _ in 0..120 {
                g.step(0.033);
                g.render(&mut buf);
            }
            // every pixel must be a valid premultiplied colour, and the shrine
            // proper (below HIT_TOP) must never be click-through
            for (i, p) in buf.iter().enumerate() {
                let a = p >> 24;
                assert!(((p >> 16) & 0xff) <= a && ((p >> 8) & 0xff) <= a && (p & 0xff) <= a);
                let v = ((i / w) as f32 + 0.5) / sf;
                if v >= HIT_TOP {
                    assert!(a >= 1, "hit-test fill must cover the shrine");
                }
            }
        }
    }

    #[test]
    fn busy_settles_back_to_idle() {
        let mut g = Gfx::new(1.0, 1);
        g.offer(3);
        g.recoil();
        g.holy();
        assert!(g.busy());
        for _ in 0..400 {
            g.step(0.05);
        }
        assert!(!g.busy());
    }

    /// `cargo test --release -- --ignored --nocapture bench` to see the per-frame cost.
    #[test]
    #[ignore]
    fn bench_render() {
        for sf in [1.0_f32, 1.5, 2.0] {
            let mut g = Gfx::new(sf, 3);
            let (w, h) = g.size();
            let mut buf = vec![0u32; w * h];
            let n = 600;
            let t = std::time::Instant::now();
            for _ in 0..n {
                g.step(0.066);
                g.render(&mut buf);
            }
            let per = t.elapsed().as_secs_f64() / n as f64;
            println!("sf {sf}: {w}x{h}  {:.3} ms/frame  ->  {:.3}% of one core at 15 fps", per * 1e3, per * 15.0 * 100.0);
        }
    }

    #[test]
    fn dump_frames() {
        let mut g = Gfx::new(2.0, 7);
        for _ in 0..10 {
            g.step(0.05);
        }
        dump(&g, "a_idle");
        for _ in 0..12 {
            g.step(0.05);
        }
        dump(&g, "b_idle_later");
        g.offer(3);
        for i in 0..6 {
            for _ in 0..6 {
                g.step(0.05);
            }
            dump(&g, &format!("c_burn_{i}"));
        }
        g.holy();
        for _ in 0..6 {
            g.step(0.05);
        }
        dump(&g, "d_holy");
        g.recoil();
        for _ in 0..6 {
            g.step(0.05);
        }
        dump(&g, "e_recoil");
    }
}
