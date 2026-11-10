//! Rectangles and constraint splits. margin documents flow vertically, so the layout is small:
//! insets, horizontal/vertical splits with fixed, percentage and fill constraints.

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Hash)]
pub struct Rect {
    pub x: u16,
    pub y: u16,
    pub w: u16,
    pub h: u16,
}

impl Rect {
    pub const fn new(x: u16, y: u16, w: u16, h: u16) -> Rect {
        Rect { x, y, w, h }
    }
    pub fn right(&self) -> u16 {
        self.x.saturating_add(self.w)
    }
    pub fn bottom(&self) -> u16 {
        self.y.saturating_add(self.h)
    }
    pub fn is_empty(&self) -> bool {
        self.w == 0 || self.h == 0
    }
    pub fn intersect(&self, o: Rect) -> Rect {
        let x = self.x.max(o.x);
        let y = self.y.max(o.y);
        let r = self.right().min(o.right());
        let b = self.bottom().min(o.bottom());
        Rect::new(x, y, r.saturating_sub(x), b.saturating_sub(y))
    }
    pub fn inset(&self, l: u16, t: u16, r: u16, b: u16) -> Rect {
        Rect::new(
            self.x + l.min(self.w),
            self.y + t.min(self.h),
            self.w.saturating_sub(l + r),
            self.h.saturating_sub(t + b),
        )
    }
    pub fn row(&self, i: u16) -> Rect {
        Rect::new(self.x, self.y + i, self.w, 1)
    }
    pub fn contains(&self, x: u16, y: u16) -> bool {
        x >= self.x && x < self.right() && y >= self.y && y < self.bottom()
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Constraint {
    Len(u16),
    Pct(u16),
    /// Share of what's left after Len and Pct, by weight.
    Fill(u16),
}

/// Split a length into parts. Rounding remainders go to the last Fill, so parts always sum to `total`
/// when any Fill exists.
pub fn split(total: u16, cs: &[Constraint]) -> Vec<u16> {
    let mut out: Vec<u16> = cs
        .iter()
        .map(|c| match *c {
            Constraint::Len(n) => n,
            Constraint::Pct(p) => (total as u32 * p as u32 / 100) as u16,
            Constraint::Fill(_) => 0,
        })
        .collect();
    let used: u32 = out.iter().map(|&n| n as u32).sum();
    let left = (total as u32).saturating_sub(used);
    let weights: u32 = cs.iter().map(|c| if let Constraint::Fill(w) = c { *w as u32 } else { 0 }).sum();
    if weights > 0 {
        let mut given = 0;
        let mut last = None;
        for (i, c) in cs.iter().enumerate() {
            if let Constraint::Fill(w) = c {
                let n = left * *w as u32 / weights;
                out[i] = n as u16;
                given += n;
                last = Some(i);
            }
        }
        if let Some(i) = last {
            out[i] += (left - given) as u16;
        }
    }
    // never exceed the total: shrink from the end
    let mut sum: u32 = out.iter().map(|&n| n as u32).sum();
    for n in out.iter_mut().rev() {
        if sum <= total as u32 {
            break;
        }
        let cut = (*n as u32).min(sum - total as u32);
        *n -= cut as u16;
        sum -= cut;
    }
    out
}

pub fn split_h(r: Rect, cs: &[Constraint]) -> Vec<Rect> {
    let mut x = r.x;
    split(r.w, cs)
        .into_iter()
        .map(|w| {
            let o = Rect::new(x, r.y, w, r.h);
            x += w;
            o
        })
        .collect()
}

pub fn split_v(r: Rect, cs: &[Constraint]) -> Vec<Rect> {
    let mut y = r.y;
    split(r.h, cs)
        .into_iter()
        .map(|h| {
            let o = Rect::new(r.x, y, r.w, h);
            y += h;
            o
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use Constraint::*;
    #[test]
    fn splits() {
        assert_eq!(split(10, &[Len(3), Fill(1)]), vec![3, 7]);
        assert_eq!(split(10, &[Fill(1), Fill(1), Fill(1)]), vec![3, 3, 4]);
        assert_eq!(split(10, &[Pct(50), Fill(1)]), vec![5, 5]);
        assert_eq!(split(4, &[Len(3), Len(3)]), vec![3, 1]);
        let r = split_v(Rect::new(0, 0, 5, 10), &[Len(1), Fill(1), Len(1)]);
        assert_eq!(r[2], Rect::new(0, 9, 5, 1));
    }
    #[test]
    fn intersect_inset() {
        assert_eq!(Rect::new(0, 0, 10, 10).intersect(Rect::new(5, 5, 10, 10)), Rect::new(5, 5, 5, 5));
        assert_eq!(Rect::new(0, 0, 10, 4).inset(1, 1, 1, 1), Rect::new(1, 1, 8, 2));
    }
}
