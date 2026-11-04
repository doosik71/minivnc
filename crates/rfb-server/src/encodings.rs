//! Interpreting `SetEncodings`, following TurboVNC
//! `rfbProcessClientNormalMessage`.

use rfb_codec::jpeg::Subsampling;
use rfb_codec::tight::TightSettings;
use rfb_proto::consts::*;

/// Quality level 0-9 -> JPEG quality, for TightVNC/TigerVNC clients.
const JPEG_QUAL: [u8; 10] = [15, 29, 41, 42, 62, 77, 79, 86, 92, 100];
/// Quality level 0-9 -> subsampling level (0 = 1X, 1 = 4X, 2 = 2X).
const JPEG_SUBSAMP: [u8; 10] = [1, 1, 1, 2, 2, 2, 0, 0, 0, 0];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct EncodingPrefs {
    /// `ENCODING_RAW`, `ENCODING_HEXTILE`, `ENCODING_ZRLE` or
    /// `ENCODING_TIGHT`.
    pub preferred: i32,
    pub tight: TightSettings,
}

impl Default for EncodingPrefs {
    /// Before `SetEncodings`, TurboVNC uses Tight with its defaults.
    fn default() -> Self {
        Self {
            preferred: ENCODING_TIGHT,
            tight: TightSettings::default(),
        }
    }
}

impl EncodingPrefs {
    /// Applies a `SetEncodings` list. Like TurboVNC, every message starts
    /// from the defaults (except Tight-without-zlib, which stays enabled once
    /// seen), and later entries override earlier ones.
    ///
    /// The first supported real encoding becomes the preferred one. RRE,
    /// CoRRE, Zlib and ZYWRLE are not implemented and are skipped where
    /// TurboVNC would use them.
    pub fn apply(&mut self, encodings: &[i32]) {
        let mut preferred = None;
        let without_zlib = self.tight.without_zlib;
        self.tight = TightSettings {
            without_zlib,
            ..TightSettings::default()
        };
        for &e in encodings {
            match e {
                ENCODING_RAW | ENCODING_HEXTILE | ENCODING_ZRLE | ENCODING_TIGHT => {
                    preferred.get_or_insert(e);
                }
                ENCODING_LAST_RECT => self.tight.last_rect = true,
                ENCODING_TIGHT_WITHOUT_ZLIB => self.tight.without_zlib = true,
                ENCODING_COMPRESS_LEVEL0..=ENCODING_COMPRESS_LEVEL9 => {
                    self.tight.compress_level = (e - ENCODING_COMPRESS_LEVEL0) as u8;
                }
                ENCODING_SUBSAMP_1X..=ENCODING_SUBSAMP_GRAY => {
                    self.tight.subsampling =
                        Subsampling::from_level((e - ENCODING_SUBSAMP_1X) as u8)
                            .expect("level 0-3");
                }
                ENCODING_QUALITY_LEVEL0..=ENCODING_QUALITY_LEVEL9 => {
                    let level = (e - ENCODING_QUALITY_LEVEL0) as usize;
                    self.tight.quality = Some(JPEG_QUAL[level]);
                    self.tight.subsampling =
                        Subsampling::from_level(JPEG_SUBSAMP[level]).expect("level 0-2");
                }
                // Fine quality 0 is not accepted, as in TurboVNC.
                e if (ENCODING_FINE_QUALITY_LEVEL0 + 1..=ENCODING_FINE_QUALITY_LEVEL100)
                    .contains(&e) =>
                {
                    self.tight.quality = Some((e - ENCODING_FINE_QUALITY_LEVEL0) as u8);
                }
                _ => {}
            }
        }
        self.preferred = preferred.unwrap_or(ENCODING_TIGHT);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply(list: &[i32]) -> EncodingPrefs {
        let mut p = EncodingPrefs::default();
        p.apply(list);
        p
    }

    #[test]
    fn java_viewer_presets() {
        // As sent by the TurboVNC viewer: CL, coarse quality, fine quality,
        // subsampling (later entries win).
        let medium = apply(&[
            ENCODING_TIGHT,
            ENCODING_LAST_RECT,
            ENCODING_COMPRESS_LEVEL0 + 6,
            ENCODING_QUALITY_LEVEL0 + 8,
            ENCODING_FINE_QUALITY_LEVEL0 + 80,
            ENCODING_SUBSAMP_2X,
        ]);
        assert_eq!(medium.preferred, ENCODING_TIGHT);
        assert_eq!(medium.tight.compress_level, 6);
        assert_eq!(medium.tight.quality, Some(80));
        assert_eq!(medium.tight.subsampling, Subsampling::S422);
        assert!(medium.tight.last_rect);

        let lossless = apply(&[ENCODING_TIGHT, ENCODING_COMPRESS_LEVEL0]);
        assert_eq!(lossless.tight.quality, None);
        assert_eq!(lossless.tight.compress_level, 0);
    }

    #[test]
    fn coarse_quality_uses_turbovnc_tables() {
        let p = apply(&[ENCODING_QUALITY_LEVEL0 + 3]);
        assert_eq!(p.tight.quality, Some(42));
        assert_eq!(p.tight.subsampling, Subsampling::S422);
        // Fine level 0 is ignored.
        assert_eq!(apply(&[ENCODING_FINE_QUALITY_LEVEL0]).tight.quality, None);
    }

    #[test]
    fn first_supported_encoding_wins_and_state_resets() {
        let mut p = EncodingPrefs::default();
        p.apply(&[
            ENCODING_ZYWRLE,
            ENCODING_ZRLE,
            ENCODING_RAW,
            ENCODING_TIGHT,
            ENCODING_TIGHT_WITHOUT_ZLIB,
            ENCODING_QUALITY_LEVEL0 + 9,
        ]);
        // ZYWRLE is not implemented.
        assert_eq!(p.preferred, ENCODING_ZRLE);
        p.apply(&[ENCODING_HEXTILE]);
        assert_eq!(p.preferred, ENCODING_HEXTILE);
        p.apply(&[ENCODING_RRE]);
        assert_eq!(p.preferred, ENCODING_TIGHT);
        assert_eq!(p.tight.quality, None);
        assert!(p.tight.without_zlib, "TightWithoutZlib is sticky");
    }
}
