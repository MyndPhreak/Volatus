//! Folder colours: one hue per top-level folder, a shade per depth below it.

use treemap::Theme;

/// Band colour (RGB) of a folder at `depth` (0 = a top-level folder) inside the top-level folder
/// number `index`. Each index has its own hue and each depth its own shade; every colour has 3:1
/// contrast with the backdrop of its theme (#f3f3f3 light, #202020 dark).
pub fn folder_colour(index: usize, depth: u16, theme: Theme) -> [u8; 3] {
    let _ = (index, depth, theme);
    [0, 0, 0]
}

#[cfg(test)]
mod tests {
    use super::*;

    const THEMES: [Theme; 2] = [Theme::Light, Theme::Dark];

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
    fn every_hue_and_depth_has_3_to_1_contrast_in_each_theme() {
        for (theme, backdrop) in [(Theme::Light, [0xf3; 3]), (Theme::Dark, [0x20; 3])] {
            for index in 0..40 {
                for depth in 0..6 {
                    let ratio = contrast(folder_colour(index, depth, theme), backdrop);
                    assert!(ratio >= 3.0, "{theme:?} index {index} depth {depth}: {ratio:.2}");
                }
            }
        }
    }

    #[test]
    fn the_first_twelve_top_level_folders_have_different_colours() {
        for theme in THEMES {
            let c: Vec<[u8; 3]> = (0..12).map(|i| folder_colour(i, 0, theme)).collect();
            for i in 0..c.len() {
                for j in i + 1..c.len() {
                    assert_ne!(c[i], c[j], "{theme:?} {i} vs {j}");
                }
            }
        }
    }

    #[test]
    fn the_first_four_depths_of_a_folder_have_different_colours() {
        for theme in THEMES {
            for index in [0, 1, 5] {
                let c: Vec<[u8; 3]> = (0..4).map(|d| folder_colour(index, d, theme)).collect();
                for i in 0..c.len() {
                    for j in i + 1..c.len() {
                        assert_ne!(c[i], c[j], "{theme:?} index {index}: depth {i} vs {j}");
                    }
                }
            }
        }
    }

    #[test]
    fn the_colour_is_the_same_on_every_call() {
        for theme in THEMES {
            assert_eq!(folder_colour(3, 2, theme), folder_colour(3, 2, theme));
        }
    }
}
