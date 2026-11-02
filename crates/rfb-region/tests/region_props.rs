//! Property tests: region operations against a pixel-set reference model.

use std::collections::BTreeSet;

use proptest::prelude::*;
use rfb_region::{Rect, Region};

type Pixels = BTreeSet<(i32, i32)>;

/// Coordinates stay small so the reference model is cheap and rectangles
/// overlap often.
const SPAN: i32 = 16;

fn rect_strategy() -> impl Strategy<Value = Rect> {
    (0..SPAN, 0..SPAN, 0..8, 0..8).prop_map(|(x, y, w, h)| Rect::from_xywh(x, y, w, h))
}

fn rects_strategy() -> impl Strategy<Value = Vec<Rect>> {
    prop::collection::vec(rect_strategy(), 0..8)
}

fn region_of(rects: &[Rect]) -> Region {
    rects
        .iter()
        .fold(Region::new(), |acc, &r| acc.union(&Region::from(r)))
}

fn pixels_of_rects(rects: &[Rect]) -> Pixels {
    rects
        .iter()
        .flat_map(|r| (r.y1..r.y2).flat_map(move |y| (r.x1..r.x2).map(move |x| (x, y))))
        .collect()
}

fn pixels(region: &Region) -> Pixels {
    pixels_of_rects(region.rects())
}

/// Builds a region one pixel at a time.
fn region_of_pixels(p: &Pixels) -> Region {
    let rects: Vec<Rect> = p
        .iter()
        .map(|&(x, y)| Rect::from_xywh(x, y, 1, 1))
        .collect();
    region_of(&rects)
}

fn assert_canonical(region: &Region) {
    let rects = region.rects();
    for r in rects {
        assert!(!r.is_empty(), "empty rect {r:?}");
    }
    let bands: Vec<&[Rect]> = rects.chunk_by(|a, b| a.y1 == b.y1).collect();
    for band in &bands {
        let (y1, y2) = (band[0].y1, band[0].y2);
        for r in *band {
            assert_eq!((r.y1, r.y2), (y1, y2), "ragged band {band:?}");
        }
        for pair in band.windows(2) {
            assert!(
                pair[0].x2 < pair[1].x1,
                "overlapping/touching spans {band:?}"
            );
        }
    }
    for pair in bands.windows(2) {
        let (upper, lower) = (pair[0], pair[1]);
        assert!(upper[0].y2 <= lower[0].y1, "bands out of order");
        let same_spans = upper.len() == lower.len()
            && upper
                .iter()
                .zip(lower)
                .all(|(a, b)| (a.x1, a.x2) == (b.x1, b.x2));
        assert!(
            !(upper[0].y2 == lower[0].y1 && same_spans),
            "unmerged bands {upper:?} {lower:?}"
        );
    }
}

proptest! {
    #[test]
    fn union_matches_model(a in rects_strategy(), b in rects_strategy()) {
        let (ra, rb) = (region_of(&a), region_of(&b));
        let r = ra.union(&rb);
        assert_canonical(&r);
        let expected: Pixels = pixels_of_rects(&a).union(&pixels_of_rects(&b)).copied().collect();
        prop_assert_eq!(pixels(&r), expected);
        prop_assert_eq!(&r, &rb.union(&ra));
    }

    #[test]
    fn intersect_matches_model(a in rects_strategy(), b in rects_strategy()) {
        let r = region_of(&a).intersect(&region_of(&b));
        assert_canonical(&r);
        let expected: Pixels =
            pixels_of_rects(&a).intersection(&pixels_of_rects(&b)).copied().collect();
        prop_assert_eq!(pixels(&r), expected);
    }

    #[test]
    fn subtract_matches_model(a in rects_strategy(), b in rects_strategy()) {
        let r = region_of(&a).subtract(&region_of(&b));
        assert_canonical(&r);
        let expected: Pixels =
            pixels_of_rects(&a).difference(&pixels_of_rects(&b)).copied().collect();
        prop_assert_eq!(pixels(&r), expected);
    }

    #[test]
    fn form_is_canonical(a in rects_strategy()) {
        // The same pixels built a different way give the same rectangles.
        let r = region_of(&a);
        prop_assert_eq!(&region_of_pixels(&pixels(&r)), &r);
        let mut reversed = a.clone();
        reversed.reverse();
        prop_assert_eq!(&region_of(&reversed), &r);
    }

    #[test]
    fn extents_bound_pixels(a in rects_strategy()) {
        let r = region_of(&a);
        let p = pixels(&r);
        match r.extents() {
            None => prop_assert!(p.is_empty()),
            Some(e) => {
                let xs = p.iter().map(|&(x, _)| x);
                let ys = p.iter().map(|&(_, y)| y);
                prop_assert_eq!(e.x1, xs.clone().min().unwrap());
                prop_assert_eq!(e.x2, xs.max().unwrap() + 1);
                prop_assert_eq!(e.y1, ys.clone().min().unwrap());
                prop_assert_eq!(e.y2, ys.max().unwrap() + 1);
            }
        }
    }

    #[test]
    fn translate_shifts_pixels(a in rects_strategy(), dx in -SPAN..SPAN, dy in -SPAN..SPAN) {
        let mut r = region_of(&a);
        let expected: Pixels = pixels(&r).iter().map(|&(x, y)| (x + dx, y + dy)).collect();
        r.translate(dx, dy);
        assert_canonical(&r);
        prop_assert_eq!(pixels(&r), expected);
    }
}
