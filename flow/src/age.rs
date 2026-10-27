//! Age classes: how long ago the newest change inside a folder was.

use treemap::Theme;

/// Up to about 6 months.
pub const RECENT_SECS: u32 = 183 * 86_400;
/// Up to 2 years.
pub const MIDDLE_SECS: u32 = 730 * 86_400;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Age {
    Recent,
    Middle,
    Old,
    Unknown,
}

/// Age class of `newest` (scan time encoding, 0 = unknown) at time `now`.
pub fn age_of(_newest: u32, _now: u32) -> Age {
    Age::Unknown
}

/// Band colour (RGB) of an age class.
pub fn age_colour(_a: Age, _theme: Theme) -> [u8; 3] {
    [0; 3]
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u32 = 846_288_000;
    const D: u32 = 86_400;
    const ALL: [Age; 4] = [Age::Recent, Age::Middle, Age::Old, Age::Unknown];

    #[test]
    fn age_of_unknown_is_unknown() {
        assert_eq!(age_of(0, NOW), Age::Unknown);
    }

    #[test]
    fn age_of_now_and_future_is_recent() {
        assert_eq!(age_of(NOW, NOW), Age::Recent);
        assert_eq!(age_of(NOW + 10, NOW), Age::Recent);
    }

    #[test]
    fn age_of_183_days_is_recent_and_one_second_more_is_middle() {
        assert_eq!(age_of(NOW - 183 * D, NOW), Age::Recent);
        assert_eq!(age_of(NOW - 183 * D - 1, NOW), Age::Middle);
    }

    #[test]
    fn age_of_730_days_is_middle_and_one_second_more_is_old() {
        assert_eq!(age_of(NOW - 730 * D, NOW), Age::Middle);
        assert_eq!(age_of(NOW - 730 * D - 1, NOW), Age::Old);
        assert_eq!(age_of(1, NOW), Age::Old);
    }

    #[test]
    fn age_of_small_now_does_not_underflow() {
        assert_eq!(age_of(1, 100), Age::Recent);
    }

    #[test]
    fn age_colour_has_four_different_colours_in_each_theme() {
        for theme in [Theme::Light, Theme::Dark] {
            let c: Vec<[u8; 3]> = ALL.iter().map(|&a| age_colour(a, theme)).collect();
            for i in 0..c.len() {
                for j in i + 1..c.len() {
                    assert_ne!(c[i], c[j], "{theme:?} {:?} vs {:?}", ALL[i], ALL[j]);
                }
            }
        }
    }

    #[test]
    fn age_colour_unknown_is_gray() {
        for theme in [Theme::Light, Theme::Dark] {
            let [r, g, b] = age_colour(Age::Unknown, theme);
            assert!(r == g && g == b, "{theme:?} not gray");
            let [o, ..] = treemap::filetype::colour(treemap::filetype::Category::Other, theme);
            assert!(r.abs_diff(o) <= 8, "{theme:?} far from the Other gray");
        }
    }

    /// WCAG relative luminance of an sRGB colour.
    fn luminance(c: [u8; 3]) -> f64 {
        let lin = |v: u8| {
            let v = f64::from(v) / 255.0;
            if v <= 0.03928 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
        };
        0.2126 * lin(c[0]) + 0.7152 * lin(c[1]) + 0.0722 * lin(c[2])
    }

    fn contrast(a: [u8; 3], b: [u8; 3]) -> f64 {
        let (x, y) = (luminance(a), luminance(b));
        (x.max(y) + 0.05) / (x.min(y) + 0.05)
    }

    #[test]
    fn age_colour_known_classes_have_3_to_1_contrast_in_each_theme() {
        for (theme, backdrop) in [(Theme::Light, [0xf3; 3]), (Theme::Dark, [0x20; 3])] {
            for a in [Age::Recent, Age::Middle, Age::Old] {
                let ratio = contrast(age_colour(a, theme), backdrop);
                assert!(ratio >= 3.0, "{theme:?} {a:?}: {ratio:.2}");
            }
        }
    }
}
