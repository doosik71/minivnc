//! Rectangle regions for damage tracking, replacing the X server `RegionRec`
//! (pixman) that TurboVNC relies on.
//!
//! A [`Region`] is stored in pixman's canonical y-x banded form:
//!
//! - rectangles are sorted by `y1`, then `x1`;
//! - rectangles in the same band share `y1` and `y2`, and within a band they
//!   neither overlap nor touch;
//! - bands do not overlap, and two bands that touch vertically never have
//!   identical x spans (they are merged instead).
//!
//! Because the form is canonical, two regions covering the same pixels have
//! identical rectangle lists, so `==` compares coverage.

/// Half-open rectangle `[x1, x2) x [y1, y2)`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Rect {
    pub x1: i32,
    pub y1: i32,
    pub x2: i32,
    pub y2: i32,
}

impl Rect {
    pub const fn new(x1: i32, y1: i32, x2: i32, y2: i32) -> Self {
        Self { x1, y1, x2, y2 }
    }

    pub const fn from_xywh(x: i32, y: i32, w: i32, h: i32) -> Self {
        Self::new(x, y, x + w, y + h)
    }

    pub const fn width(&self) -> i32 {
        self.x2 - self.x1
    }

    pub const fn height(&self) -> i32 {
        self.y2 - self.y1
    }

    pub const fn is_empty(&self) -> bool {
        self.x1 >= self.x2 || self.y1 >= self.y2
    }
}

/// Set of pixels, stored as canonical y-x banded rectangles.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Region {
    rects: Vec<Rect>,
}

impl From<Rect> for Region {
    fn from(rect: Rect) -> Self {
        let rects = if rect.is_empty() {
            Vec::new()
        } else {
            vec![rect]
        };
        Self { rects }
    }
}

impl Region {
    pub const fn new() -> Self {
        Self { rects: Vec::new() }
    }

    /// The rectangles in band order.
    pub fn rects(&self) -> &[Rect] {
        &self.rects
    }

    pub fn num_rects(&self) -> usize {
        self.rects.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rects.is_empty()
    }

    /// Bounding box, or `None` for an empty region.
    pub fn extents(&self) -> Option<Rect> {
        let first = self.rects.first()?;
        let last = self.rects.last()?;
        let x1 = self.rects.iter().map(|r| r.x1).min()?;
        let x2 = self.rects.iter().map(|r| r.x2).max()?;
        Some(Rect::new(x1, first.y1, x2, last.y2))
    }

    pub fn union(&self, other: &Region) -> Region {
        op(self, other, |a, b| a || b)
    }

    pub fn intersect(&self, other: &Region) -> Region {
        op(self, other, |a, b| a && b)
    }

    /// Pixels in `self` that are not in `other`.
    pub fn subtract(&self, other: &Region) -> Region {
        op(self, other, |a, b| a && !b)
    }

    pub fn translate(&mut self, dx: i32, dy: i32) {
        for r in &mut self.rects {
            *r = Rect::new(r.x1 + dx, r.y1 + dy, r.x2 + dx, r.y2 + dy);
        }
    }

    /// Replaces the region with its bounding box when it has more than
    /// `max_rects` rectangles (TurboVNC `rfbCombineRect`, default 100).
    pub fn collapse_if_over(&mut self, max_rects: usize) {
        if self.rects.len() > max_rects
            && let Some(extents) = self.extents()
        {
            *self = Region::from(extents);
        }
    }

    /// Splits the rectangle list into bands (runs sharing `y1`).
    fn bands(&self) -> Vec<&[Rect]> {
        self.rects.chunk_by(|a, b| a.y1 == b.y1).collect()
    }
}

/// Applies a pixel-wise boolean operation to two regions.
///
/// The y axis is cut at every band edge of either input; inside each slab both
/// inputs have a fixed set of x spans, which are combined with `keep`.
fn op(a: &Region, b: &Region, keep: fn(bool, bool) -> bool) -> Region {
    let a_bands = a.bands();
    let b_bands = b.bands();
    let mut ys: Vec<i32> = a_bands
        .iter()
        .chain(&b_bands)
        .flat_map(|band| [band[0].y1, band[0].y2])
        .collect();
    ys.sort_unstable();
    ys.dedup();

    let mut out = Builder::default();
    let (mut ia, mut ib) = (0, 0);
    let mut spans = Vec::new();
    for slab in ys.windows(2) {
        let (y1, y2) = (slab[0], slab[1]);
        let a_spans = band_at(&a_bands, &mut ia, y1);
        let b_spans = band_at(&b_bands, &mut ib, y1);
        span_op(a_spans, b_spans, keep, &mut spans);
        out.push_band(y1, y2, &spans);
    }
    Region { rects: out.rects }
}

/// Returns the band covering row `y`, advancing `idx` past bands above it.
/// Calls must use non-decreasing `y`.
fn band_at<'a>(bands: &[&'a [Rect]], idx: &mut usize, y: i32) -> &'a [Rect] {
    while *idx < bands.len() && bands[*idx][0].y2 <= y {
        *idx += 1;
    }
    match bands.get(*idx) {
        Some(band) if band[0].y1 <= y => band,
        _ => &[],
    }
}

/// Combines the x spans of two bands into `out` as sorted, non-touching spans.
fn span_op(a: &[Rect], b: &[Rect], keep: fn(bool, bool) -> bool, out: &mut Vec<(i32, i32)>) {
    out.clear();
    let mut xs: Vec<i32> = a.iter().chain(b).flat_map(|r| [r.x1, r.x2]).collect();
    xs.sort_unstable();
    xs.dedup();

    let (mut i, mut j) = (0, 0);
    for seg in xs.windows(2) {
        let (x1, x2) = (seg[0], seg[1]);
        while i < a.len() && a[i].x2 <= x1 {
            i += 1;
        }
        while j < b.len() && b[j].x2 <= x1 {
            j += 1;
        }
        let in_a = i < a.len() && a[i].x1 <= x1;
        let in_b = j < b.len() && b[j].x1 <= x1;
        if keep(in_a, in_b) {
            match out.last_mut() {
                Some(last) if last.1 == x1 => last.1 = x2,
                _ => out.push((x1, x2)),
            }
        }
    }
}

/// Appends bands in increasing y, merging a band into the previous one when
/// they touch and have the same spans.
#[derive(Default)]
struct Builder {
    rects: Vec<Rect>,
    last_band: usize,
}

impl Builder {
    fn push_band(&mut self, y1: i32, y2: i32, spans: &[(i32, i32)]) {
        if spans.is_empty() {
            return;
        }
        let prev = &mut self.rects[self.last_band..];
        let mergeable = prev.first().is_some_and(|r| r.y2 == y1)
            && prev.len() == spans.len()
            && prev.iter().zip(spans).all(|(r, s)| (r.x1, r.x2) == *s);
        if mergeable {
            for r in prev {
                r.y2 = y2;
            }
        } else {
            self.last_band = self.rects.len();
            self.rects
                .extend(spans.iter().map(|&(x1, x2)| Rect::new(x1, y1, x2, y2)));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn region(rects: &[(i32, i32, i32, i32)]) -> Region {
        rects.iter().fold(Region::new(), |acc, &(x1, y1, x2, y2)| {
            acc.union(&Region::from(Rect::new(x1, y1, x2, y2)))
        })
    }

    #[test]
    fn empty_rect_gives_empty_region() {
        assert!(Region::from(Rect::new(5, 5, 5, 10)).is_empty());
        assert_eq!(Region::new().extents(), None);
    }

    #[test]
    fn union_of_overlapping_rects_is_banded() {
        // Two overlapping squares form an L-like shape with three bands.
        let r = region(&[(0, 0, 10, 10), (5, 5, 15, 15)]);
        assert_eq!(
            r.rects(),
            [
                Rect::new(0, 0, 10, 5),
                Rect::new(0, 5, 15, 10),
                Rect::new(5, 10, 15, 15),
            ]
        );
        assert_eq!(r.extents(), Some(Rect::new(0, 0, 15, 15)));
    }

    #[test]
    fn touching_rects_merge() {
        assert_eq!(
            region(&[(0, 0, 5, 5), (5, 0, 10, 5)]).rects(),
            [Rect::new(0, 0, 10, 5)]
        );
        assert_eq!(
            region(&[(0, 0, 5, 5), (0, 5, 5, 10)]).rects(),
            [Rect::new(0, 0, 5, 10)]
        );
    }

    #[test]
    fn subtract_punches_hole() {
        let r = region(&[(0, 0, 9, 9)]).subtract(&region(&[(3, 3, 6, 6)]));
        assert_eq!(
            r.rects(),
            [
                Rect::new(0, 0, 9, 3),
                Rect::new(0, 3, 3, 6),
                Rect::new(6, 3, 9, 6),
                Rect::new(0, 6, 9, 9),
            ]
        );
    }

    #[test]
    fn collapse_if_over_uses_extents() {
        let mut r = region(&[(0, 0, 1, 1), (4, 4, 5, 5)]);
        r.collapse_if_over(2);
        assert_eq!(r.num_rects(), 2);
        r.collapse_if_over(1);
        assert_eq!(r.rects(), [Rect::new(0, 0, 5, 5)]);
    }
}
