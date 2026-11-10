//! Truecolor values, blending, and the palettes margin draws with.
//!
//! Every colour is 24-bit: Ghostty and Zellij both pass truecolor SGR through,
//! and blending (gradients, fades, pulses) needs the full range.

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash, Default)]
pub struct Rgb(pub u8, pub u8, pub u8);

/// A colour in OKLab: `l` lightness 0..1, `a`/`b` the green-red and blue-yellow axes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Oklab {
    pub l: f32,
    pub a: f32,
    pub b: f32,
}

impl Rgb {
    /// Parse `#rrggbb` or `rrggbb`. Panics on malformed input, so palettes fail at startup, not mid-frame.
    pub const fn hex(s: &str) -> Rgb {
        let b = s.as_bytes();
        let o = if b[0] == b'#' { 1 } else { 0 };
        const fn h(c: u8) -> u8 {
            match c {
                b'0'..=b'9' => c - b'0',
                b'a'..=b'f' => c - b'a' + 10,
                b'A'..=b'F' => c - b'A' + 10,
                _ => panic!("bad hex digit"),
            }
        }
        Rgb(h(b[o]) * 16 + h(b[o + 1]), h(b[o + 2]) * 16 + h(b[o + 3]), h(b[o + 4]) * 16 + h(b[o + 5]))
    }

    pub fn parse(s: &str) -> Option<Rgb> {
        let t = s.trim_start_matches('#');
        if t.len() != 6 || !t.bytes().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        Some(Rgb::hex(t))
    }

    /// t=0 gives self, t=1 gives `o`. t outside [0,1] extrapolates (a spring's overshoot), clamped per channel.
    /// Blend toward `o` in sRGB. The palette's tints and fades (tag grounds, dim text, change
    /// bars) were tuned by eye with this, so it stays; gradients use [`Rgb::mix_lch`].
    pub fn mix(self, o: Rgb, t: f32) -> Rgb {
        let f = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t).round().clamp(0.0, 255.0) as u8;
        Rgb(f(self.0, o.0), f(self.1, o.1), f(self.2, o.2))
    }

    /// Blend toward `o` in OKLab: a straight line in a perceptual space, so lightness changes
    /// evenly and the hue holds.
    pub fn mix_lab(self, o: Rgb, t: f32) -> Rgb {
        if t <= 0.0 {
            return self;
        }
        if t >= 1.0 {
            return o;
        }
        let (a, b) = (self.to_oklab(), o.to_oklab());
        Rgb::from_oklab(Oklab { l: a.l + (b.l - a.l) * t, a: a.a + (b.a - a.a) * t, b: a.b + (b.b - a.b) * t })
    }

    /// Blend toward `o` in OKLCH, taking the shorter way round the hue circle: between two
    /// saturated colours the midpoint stays saturated (purple to aqua passed through grey twice per
    /// ring lap in sRGB and OKLab alike). When either end is nearly grey its hue means nothing, so
    /// this falls back to the OKLab line, the CSS Color 4 "powerless hue" rule.
    pub fn mix_lch(self, o: Rgb, t: f32) -> Rgb {
        if t <= 0.0 {
            return self;
        }
        if t >= 1.0 {
            return o;
        }
        let (a, b) = (self.to_oklab(), o.to_oklab());
        let (ca, cb) = (a.a.hypot(a.b), b.a.hypot(b.b));
        if ca < 0.03 || cb < 0.03 {
            return self.mix_lab(o, t);
        }
        let (ha, hb) = (a.b.atan2(a.a), b.b.atan2(b.a));
        let mut dh = hb - ha;
        if dh > std::f32::consts::PI {
            dh -= std::f32::consts::TAU;
        } else if dh < -std::f32::consts::PI {
            dh += std::f32::consts::TAU;
        }
        let (l, c, h) = (a.l + (b.l - a.l) * t, ca + (cb - ca) * t, ha + dh * t);
        Rgb::from_oklab(Oklab { l, a: c * h.cos(), b: c * h.sin() })
    }

    /// Björn Ottosson's OKLab (2020), from sRGB through linear light.
    pub fn to_oklab(self) -> Oklab {
        let lin = |c: u8| {
            let c = c as f32 / 255.0;
            if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
        };
        let (r, g, b) = (lin(self.0), lin(self.1), lin(self.2));
        let l = (0.412_221_47 * r + 0.536_332_55 * g + 0.051_445_995 * b).cbrt();
        let m = (0.211_903_5 * r + 0.680_699_5 * g + 0.107_396_96 * b).cbrt();
        let s = (0.088_302_46 * r + 0.281_718_85 * g + 0.629_978_7 * b).cbrt();
        Oklab {
            l: 0.210_454_26 * l + 0.793_617_8 * m - 0.004_072_047 * s,
            a: 1.977_998_5 * l - 2.428_592_2 * m + 0.450_593_7 * s,
            b: 0.025_904_037 * l + 0.782_771_77 * m - 0.808_675_77 * s,
        }
    }

    /// Back to sRGB; colours outside the sRGB gamut clip per channel.
    pub fn from_oklab(o: Oklab) -> Rgb {
        let l = (o.l + 0.396_337_78 * o.a + 0.215_803_76 * o.b).powi(3);
        let m = (o.l - 0.105_561_346 * o.a - 0.063_854_17 * o.b).powi(3);
        let s = (o.l - 0.089_484_18 * o.a - 1.291_485_5 * o.b).powi(3);
        let enc = |x: f32| {
            let x = x.clamp(0.0, 1.0);
            let c = if x <= 0.003_130_8 { 12.92 * x } else { 1.055 * x.powf(1.0 / 2.4) - 0.055 };
            (c * 255.0).round().clamp(0.0, 255.0) as u8
        };
        Rgb(
            enc(4.076_741_7 * l - 3.307_711_6 * m + 0.230_969_94 * s),
            enc(-1.268_438 * l + 2.609_757_4 * m - 0.341_319_38 * s),
            enc(-0.004_196_086 * l - 0.703_418_6 * m + 1.707_614_7 * s),
        )
    }

    pub fn to_hex(self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.0, self.1, self.2)
    }
}

/// Sample a multi-stop gradient at `t` in [0,1], blending in OKLCH. `stops` are evenly spaced.
pub fn gradient(stops: &[Rgb], t: f32) -> Rgb {
    match stops.len() {
        0 => Rgb(0, 0, 0),
        1 => stops[0],
        n => {
            let t = t.clamp(0.0, 1.0) * (n - 1) as f32;
            let i = (t.floor() as usize).min(n - 2);
            stops[i].mix_lch(stops[i + 1], t - i as f32)
        }
    }
}

/// Sample a cyclic gradient (first stop repeats after the last) at any `t`; wraps.
pub fn cyclic(stops: &[Rgb], t: f32) -> Rgb {
    let n = stops.len();
    if n == 0 {
        return Rgb(0, 0, 0);
    }
    let t = t.rem_euclid(1.0) * n as f32;
    let i = (t.floor() as usize) % n;
    let f = t - t.floor();
    // smoothstep inside each segment so the stops read as colours, not as a sawtooth
    let f = f * f * (3.0 - 2.0 * f);
    stops[i].mix_lch(stops[(i + 1) % n], f)
}

/// Everforest dark (medium): the family of sulin's terminal theme. Each accent carries one meaning
/// (see `Theme` docs); the names stay Everforest's so the mockups and the code share a vocabulary.
#[derive(Clone, Debug)]
pub struct Palette {
    pub bg_dim: Rgb,
    pub bg0: Rgb,
    pub bg1: Rgb,
    pub bg2: Rgb,
    pub bg3: Rgb,
    pub bg4: Rgb,
    pub bg_red: Rgb,
    pub bg_green: Rgb,
    pub bg_visual: Rgb,
    pub hl_red: Rgb,
    pub hl_green: Rgb,
    pub fg: Rgb,
    pub grey0: Rgb,
    pub grey1: Rgb,
    pub grey2: Rgb,
    pub red: Rgb,
    pub orange: Rgb,
    pub yellow: Rgb,
    pub green: Rgb,
    pub aqua: Rgb,
    pub blue: Rgb,
    pub purple: Rgb,
}

pub const EVERFOREST: Palette = Palette {
    bg_dim: Rgb::hex("#232a2e"),
    bg0: Rgb::hex("#2d353b"),
    bg1: Rgb::hex("#343f44"),
    bg2: Rgb::hex("#3d484d"),
    bg3: Rgb::hex("#475258"),
    bg4: Rgb::hex("#4f585e"),
    bg_red: Rgb::hex("#514045"),
    bg_green: Rgb::hex("#425047"),
    bg_visual: Rgb::hex("#543a48"),
    hl_red: Rgb::hex("#7a4a52"),
    hl_green: Rgb::hex("#5a7350"),
    fg: Rgb::hex("#d3c6aa"),
    grey0: Rgb::hex("#7a8478"),
    grey1: Rgb::hex("#859289"),
    grey2: Rgb::hex("#9da9a0"),
    red: Rgb::hex("#e67e80"),
    orange: Rgb::hex("#e69875"),
    yellow: Rgb::hex("#dbbc7f"),
    green: Rgb::hex("#a7c080"),
    aqua: Rgb::hex("#83c092"),
    blue: Rgb::hex("#7fbbb3"),
    purple: Rgb::hex("#d699b6"),
};

/// The Lip Gloss "layout" example's status bar colours: a #353533 bar with #C1C6B2 text, a hot-pink
/// status key and violet nuggets on the right. Copied from charmbracelet/lipgloss examples/layout.
#[derive(Clone, Debug)]
pub struct StatusColors {
    pub bar_bg: Rgb,
    pub bar_fg: Rgb,
    pub key_bg: Rgb,
    pub key_fg: Rgb,
    pub nugget_fg: Rgb,
    pub nugget_a: Rgb,
    pub nugget_b: Rgb,
}

pub const LIPGLOSS_STATUS: StatusColors = StatusColors {
    bar_bg: Rgb::hex("#353533"),
    bar_fg: Rgb::hex("#C1C6B2"),
    key_bg: Rgb::hex("#FF5F87"),
    key_fg: Rgb::hex("#FFFDF5"),
    nugget_fg: Rgb::hex("#FFFDF5"),
    nugget_a: Rgb::hex("#A550DF"),
    nugget_b: Rgb::hex("#6124DF"),
};

#[cfg(test)]
mod tests {
    #[test]
    fn oklab_round_trips_and_lch_keeps_chroma() {
        for c in [Rgb(0, 0, 0), Rgb(255, 255, 255), Rgb::hex("#d699b6"), Rgb::hex("#83c092"), Rgb(12, 200, 77)] {
            assert_eq!(Rgb::from_oklab(c.to_oklab()), c);
        }
        let (purple, aqua) = (Rgb::hex("#d699b6"), Rgb::hex("#83c092"));
        let chroma = |c: Rgb| { let o = c.to_oklab(); o.a.hypot(o.b) };
        let mid_lab = purple.mix_lab(aqua, 0.5);
        let mid_lch = purple.mix_lch(aqua, 0.5);
        assert!(chroma(mid_lch) > chroma(mid_lab) * 1.5, "lch {mid_lch:?} lab {mid_lab:?}");
    }

    use super::*;
    #[test]
    fn hex_and_mix() {
        assert_eq!(Rgb::hex("#ff0080"), Rgb(255, 0, 128));
        assert_eq!(Rgb(0, 0, 0).mix(Rgb(200, 100, 50), 0.5), Rgb(100, 50, 25));
        // overshoot clamps per channel instead of wrapping
        assert_eq!(Rgb(250, 0, 0).mix(Rgb(255, 0, 0), 3.0), Rgb(255, 0, 0));
        assert_eq!(Rgb::parse("zz0000"), None);
    }
    #[test]
    fn cyclic_wraps() {
        let s = [Rgb(0, 0, 0), Rgb(255, 255, 255)];
        assert_eq!(cyclic(&s, 0.0), cyclic(&s, 1.0));
        assert_eq!(cyclic(&s, 0.5), Rgb(255, 255, 255));
    }
}
