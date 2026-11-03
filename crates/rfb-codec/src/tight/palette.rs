//! Colour counting for the Tight encoder, ported from TurboVNC `FillPalette*`
//! and `PaletteInsert`. Palette order (most frequent first, ties in insertion
//! order) and the counts themselves follow the C code exactly, because they
//! determine the bytes on the wire.

/// How a rectangle's pixels should be encoded.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Colours {
    /// One colour.
    Solid,
    /// Two colours; `bg` is the more frequent one.
    Mono { bg: u32, fg: u32 },
    /// Three or more colours, up to the limit.
    Indexed(Palette),
    /// Too many colours (or the limit is below two).
    Full,
}

/// Slots in the colour index: a power of two, twice the most colours.
const INDEX_SLOTS: usize = 512;
const EMPTY: u16 = u16::MAX;

/// Colours sorted by pixel count, most frequent first.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Palette {
    entries: Vec<(u32, u32)>,
    /// Open-addressing table of (colour, position in `entries`). A general
    /// purpose hash map made colour counting the encoder's hot spot.
    index: Box<[(u32, u16)]>,
}

impl Default for Palette {
    fn default() -> Self {
        Self {
            entries: Vec::with_capacity(256),
            index: vec![(0, EMPTY); INDEX_SLOTS].into_boxed_slice(),
        }
    }
}

impl Palette {
    pub(crate) fn colours(&self) -> impl Iterator<Item = u32> + '_ {
        self.entries.iter().map(|&(c, _)| c)
    }

    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    pub(crate) fn index_of(&self, colour: u32) -> u8 {
        self.index[self.slot(colour)].1 as u8
    }

    /// The slot holding `colour`, or the empty one where it would go.
    fn slot(&self, colour: u32) -> usize {
        let mut i = (colour.wrapping_mul(0x9E37_79B1) >> 23) as usize;
        loop {
            let (c, pos) = self.index[i];
            if pos == EMPTY || c == colour {
                return i;
            }
            i = (i + 1) % INDEX_SLOTS;
        }
    }

    fn get(&self, colour: u32) -> Option<usize> {
        let (_, pos) = self.index[self.slot(colour)];
        (pos != EMPTY).then_some(usize::from(pos))
    }

    fn reindex(&mut self, from: usize, to: usize) {
        for i in from..=to {
            let colour = self.entries[i].0;
            let slot = self.slot(colour);
            self.index[slot] = (colour, i as u16);
        }
    }

    /// Adds `n` pixels of `colour`. Returns `false` when a new colour does not
    /// fit (`max` reached or 256 colours).
    fn insert(&mut self, colour: u32, n: u32, max: usize) -> bool {
        if let Some(idx) = self.get(colour) {
            let count = self.entries[idx].1 + n;
            let mut new_idx = idx;
            while new_idx > 0 && self.entries[new_idx - 1].1 < count {
                new_idx -= 1;
            }
            self.entries[idx].1 = count;
            if new_idx != idx {
                // Move it up past the entries with fewer pixels.
                self.entries[new_idx..=idx].rotate_right(1);
                self.reindex(new_idx, idx);
            }
            return true;
        }
        if self.entries.len() == 256 || self.entries.len() == max {
            return false;
        }
        let mut idx = self.entries.len();
        while idx > 0 && self.entries[idx - 1].1 < n {
            idx -= 1;
        }
        self.entries.insert(idx, (colour, n));
        let last = self.entries.len() - 1;
        self.reindex(idx, last);
        true
    }
}

/// Classifies `pixels` (client pixel values). `max_colours` is the palette
/// limit; `indexed` is false for 8bpp clients, which TurboVNC encodes with at
/// most two colours (`FillPalette8`).
pub(crate) fn classify(pixels: &[u32], max_colours: usize, indexed: bool) -> Colours {
    let count = pixels.len();
    let c0 = pixels[0];
    let mut i = 1;
    while i < count && pixels[i] == c0 {
        i += 1;
    }
    if i >= count {
        return Colours::Solid;
    }
    if max_colours < 2 {
        return Colours::Full;
    }

    // As in the C code, n1 does not count the first pixel of c1.
    let mut n0 = i as u32;
    let c1 = pixels[i];
    let mut n1 = 0u32;
    i += 1;
    while i < count {
        let ci = pixels[i];
        if ci == c0 {
            n0 += 1;
        } else if ci == c1 {
            n1 += 1;
        } else {
            break;
        }
        i += 1;
    }
    if i >= count {
        return if n0 > n1 {
            Colours::Mono { bg: c0, fg: c1 }
        } else {
            Colours::Mono { bg: c1, fg: c0 }
        };
    }
    if !indexed {
        return Colours::Full;
    }

    let mut palette = Palette::default();
    palette.insert(c0, n0, max_colours);
    palette.insert(c1, n1, max_colours);
    let mut ci = pixels[i];
    let mut ni = 1u32;
    for &p in &pixels[i + 1..] {
        if p == ci {
            ni += 1;
        } else {
            if !palette.insert(ci, ni, max_colours) {
                return Colours::Full;
            }
            ci = p;
            ni = 1;
        }
    }
    if !palette.insert(ci, ni, max_colours) {
        return Colours::Full;
    }
    Colours::Indexed(palette)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn order(c: Colours) -> Vec<u32> {
        match c {
            Colours::Indexed(p) => p.colours().collect(),
            other => panic!("not indexed: {other:?}"),
        }
    }

    #[test]
    fn solid_mono_full() {
        assert_eq!(classify(&[5, 5, 5], 10, true), Colours::Solid);
        assert_eq!(
            classify(&[5, 7, 7, 7], 10, true),
            Colours::Mono { bg: 7, fg: 5 }
        );
        // n0 = 1, n1 = 0 (first c1 pixel is not counted): c0 is background.
        assert_eq!(classify(&[5, 7], 10, true), Colours::Mono { bg: 5, fg: 7 });
        assert_eq!(classify(&[5, 7], 1, true), Colours::Full);
        assert_eq!(classify(&[1, 2, 3], 10, false), Colours::Full);
    }

    #[test]
    fn indexed_orders_by_count_then_insertion() {
        // Counts: 1 -> 1, 2 -> 0 (+1 later), 3 -> 3.
        assert_eq!(order(classify(&[1, 2, 3, 3, 3, 2], 10, true)), [3, 1, 2]);
        // 1 -> 2, 2 -> 0 + 1, 3 -> 1 + 1: 3 passes 2 when it reaches 1, but
        // only moves ahead of entries with strictly lower counts.
        assert_eq!(order(classify(&[1, 1, 2, 3, 2, 3], 10, true)), [1, 3, 2]);
        // A new colour with a higher count goes to the front.
        assert_eq!(order(classify(&[4, 5, 6, 6], 10, true)), [6, 4, 5]);
        // A new colour with an equal count goes after existing ones: 7 and 9
        // both end at 1.
        assert_eq!(order(classify(&[7, 8, 9], 10, true)), [7, 9, 8]);
    }

    #[test]
    fn indexed_respects_limit() {
        assert_eq!(classify(&[1, 2, 3], 2, true), Colours::Full);
        assert_eq!(order(classify(&[1, 2, 3], 3, true)), [1, 3, 2]);
        let many: Vec<u32> = (0..300).collect();
        assert_eq!(classify(&many, 1000, true), Colours::Full);
    }

    #[test]
    fn index_lookup_matches_order() {
        let Colours::Indexed(p) = classify(&[9, 8, 7, 7, 8, 8], 10, true) else {
            panic!()
        };
        for (i, c) in p.colours().enumerate() {
            assert_eq!(usize::from(p.index_of(c)), i);
        }
    }
}
