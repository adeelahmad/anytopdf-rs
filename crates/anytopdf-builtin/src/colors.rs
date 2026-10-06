//! Dominant colours of visual units, so searching "red" finds red frames.
//!
//! Each visual unit is downscaled, every pixel is given the nearest basic
//! colour name in Oklab, and the frame is summarised as up to [`MAX_COLORS`] `Custom` annotations with
//! `entity = color`, the hex value, the share of the frame and the nearest
//! basic colour name plus its family ("navy" is a "blue").

use anyhow::{Context, Result};
use anytopdf_core::*;

pub const PROVIDER: &str = "colors";
/// Most colours reported per unit.
pub const MAX_COLORS: usize = 5;
/// Colours covering less of the frame than this are dropped.
const MIN_SHARE: f64 = 0.05;
/// Longest side of the frame after downscaling.
const SAMPLE_SIDE: u32 = 96;

#[derive(Debug, Clone, PartialEq)]
pub struct DominantColor {
    pub rgb: [u8; 3],
    pub share: f64,
    pub name: &'static str,
    pub family: &'static str,
}

impl DominantColor {
    pub fn hex(&self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.rgb[0], self.rgb[1], self.rgb[2])
    }

    fn annotation(&self) -> Annotation {
        let label = if self.name == self.family {
            self.name.to_string()
        } else {
            format!("{} {}", self.name, self.family)
        };
        let percent = (self.share * 100.0).round() as u32;
        let mut a = Annotation::text(
            AnnotationKind::Custom,
            PROVIDER,
            format!("color {label} {} {percent}%", self.hex()),
        );
        a.attributes.insert("entity".into(), "color".into());
        a.attributes.insert("hex".into(), self.hex());
        a.attributes
            .insert("share".into(), format!("{:.3}", self.share));
        a.attributes.insert("name".into(), self.name.into());
        a.attributes.insert("family".into(), self.family.into());
        a
    }
}

pub struct ColorEnricher {
    enabled: bool,
}

impl ColorEnricher {
    pub fn new(enabled: bool) -> Self {
        Self { enabled }
    }
}

impl Plugin for ColorEnricher {
    fn descriptor(&self) -> PluginDescriptor {
        PluginDescriptor {
            name: "dominant-colors".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            kind: "unit-enricher".into(),
            extensions: vec![],
            mime_types: vec!["image/*".into()],
            priority: 0,
        }
    }
}

impl UnitEnricher for ColorEnricher {
    fn supports(&self, _graph: &DocumentGraph, unit: &Unit) -> bool {
        // Pages that carry their own text layer are documents, not pictures.
        self.enabled
            && unit.kind == UnitKind::Visual
            && unit.visual_path.is_some()
            && unit
                .metadata
                .get(crate::importers::TEXT_LAYER_KEY)
                .map(String::as_str)
                != Some(crate::importers::TEXT_LAYER_NATIVE)
    }

    fn enrich_unit(
        &self,
        _ctx: &JobContext,
        _graph: &DocumentGraph,
        unit: &mut Unit,
    ) -> Result<Vec<String>> {
        let path = unit
            .visual_path
            .as_ref()
            .context("colour unit has no visual path")?;
        let image = image::open(path)
            .with_context(|| format!("decode {} for dominant colours", path.display()))?;
        let annotations: Vec<Annotation> = dominant_colors(&image)
            .iter()
            .map(DominantColor::annotation)
            .collect();
        unit.annotations.extend(annotations);
        Ok(vec![])
    }
}

/// Up to [`MAX_COLORS`] named colours, largest share first. Every sampled
/// pixel is named, so distinct colours are never averaged into one that is not
/// in the frame; the hex is the mean of the pixels that got the name. Fully
/// transparent pixels are ignored and a fully transparent image has no colours.
pub fn dominant_colors(image: &image::DynamicImage) -> Vec<DominantColor> {
    let small = if image.width() <= SAMPLE_SIDE && image.height() <= SAMPLE_SIDE {
        image.to_rgba8()
    } else {
        image.thumbnail(SAMPLE_SIDE, SAMPLE_SIDE).to_rgba8()
    };
    let palette: Vec<[f64; 3]> = PALETTE.iter().map(|c| to_oklab(c.rgb)).collect();
    let mut sums = vec![([0u64; 3], 0u64); PALETTE.len()];
    for pixel in small.pixels().filter(|p| p.0[3] >= 128) {
        let rgb = [pixel.0[0], pixel.0[1], pixel.0[2]];
        let (sum, count) = &mut sums[nearest(&palette, rgb)];
        for (s, c) in sum.iter_mut().zip(rgb) {
            *s += u64::from(c);
        }
        *count += 1;
    }
    let total: u64 = sums.iter().map(|(_, count)| count).sum();
    if total == 0 {
        return Vec::new();
    }

    let mut colors: Vec<DominantColor> = PALETTE
        .iter()
        .zip(sums)
        .filter(|(_, (_, count))| *count > 0)
        .map(|(named, (sum, count))| DominantColor {
            rgb: sum.map(|s| ((s + count / 2) / count) as u8),
            share: count as f64 / total as f64,
            name: named.name,
            family: named.family,
        })
        .filter(|c| c.share >= MIN_SHARE)
        .collect();
    colors.sort_by(|a, b| b.share.total_cmp(&a.share).then(a.name.cmp(b.name)));
    colors.truncate(MAX_COLORS);
    colors
}

struct NamedColor {
    name: &'static str,
    family: &'static str,
    rgb: [u8; 3],
}

const fn named(name: &'static str, family: &'static str, rgb: [u8; 3]) -> NamedColor {
    NamedColor { name, family, rgb }
}

/// Basic colour names a person would type into a search box. The family is
/// the coarse word that should also match ("maroon" is found by "red").
const PALETTE: &[NamedColor] = &[
    named("black", "black", [0, 0, 0]),
    named("dark gray", "gray", [72, 72, 72]),
    named("gray", "gray", [128, 128, 128]),
    named("silver", "gray", [192, 192, 192]),
    named("white", "white", [255, 255, 255]),
    named("red", "red", [220, 30, 30]),
    named("maroon", "red", [128, 0, 0]),
    named("pink", "pink", [250, 160, 190]),
    named("magenta", "pink", [220, 40, 170]),
    named("orange", "orange", [255, 140, 0]),
    named("tan", "brown", [210, 160, 120]),
    named("brown", "brown", [140, 80, 30]),
    named("dark brown", "brown", [90, 55, 30]),
    named("beige", "brown", [235, 220, 185]),
    named("yellow", "yellow", [250, 220, 30]),
    named("gold", "yellow", [210, 170, 40]),
    named("olive", "green", [128, 128, 0]),
    named("lime", "green", [150, 220, 50]),
    named("green", "green", [30, 150, 50]),
    named("dark green", "green", [20, 70, 30]),
    named("teal", "cyan", [0, 128, 128]),
    named("cyan", "cyan", [40, 210, 220]),
    named("sky blue", "blue", [120, 180, 240]),
    named("blue", "blue", [30, 90, 220]),
    named("navy", "blue", [20, 30, 100]),
    named("purple", "purple", [128, 40, 160]),
    named("lavender", "purple", [180, 150, 220]),
];

/// Hue and chroma count twice as much as lightness, so a dark red stays red
/// rather than becoming a gray of similar lightness.
const CHROMA_WEIGHT: f64 = 2.0;

fn nearest(palette: &[[f64; 3]], rgb: [u8; 3]) -> usize {
    let p = to_oklab(rgb);
    let distance = |q: &[f64; 3]| {
        (p[0] - q[0]).powi(2)
            + CHROMA_WEIGHT.powi(2) * ((p[1] - q[1]).powi(2) + (p[2] - q[2]).powi(2))
    };
    palette
        .iter()
        .enumerate()
        .min_by(|(_, a), (_, b)| distance(a).total_cmp(&distance(b)))
        .map(|(i, _)| i)
        .expect("palette is not empty")
}

#[cfg(test)]
fn nearest_name(rgb: [u8; 3]) -> &'static str {
    let palette: Vec<[f64; 3]> = PALETTE.iter().map(|c| to_oklab(c.rgb)).collect();
    PALETTE[nearest(&palette, rgb)].name
}

/// sRGB to Oklab, a perceptual space whose distances track how different
/// colours look, including the blues that CIELAB distorts.
fn to_oklab(rgb: [u8; 3]) -> [f64; 3] {
    let [r, g, b] = rgb.map(|c| {
        let c = f64::from(c) / 255.0;
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    });
    let l = (0.412_221_470_8 * r + 0.536_332_536_3 * g + 0.051_445_992_9 * b).cbrt();
    let m = (0.211_903_498_2 * r + 0.680_699_545_1 * g + 0.107_396_956_6 * b).cbrt();
    let s = (0.088_302_461_9 * r + 0.281_718_837_6 * g + 0.629_978_700_5 * b).cbrt();
    [
        0.210_454_255_3 * l + 0.793_617_785 * m - 0.004_072_046_8 * s,
        1.977_998_495_1 * l - 2.428_592_205 * m + 0.450_593_709_9 * s,
        0.025_904_037_1 * l + 0.782_771_766_2 * m - 0.808_675_766 * s,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{DynamicImage, Rgba, RgbaImage};

    fn split_image(left: [u8; 3], right: [u8; 3], left_columns: u32) -> DynamicImage {
        DynamicImage::ImageRgba8(RgbaImage::from_fn(100, 50, |x, _| {
            let [r, g, b] = if x < left_columns { left } else { right };
            Rgba([r, g, b, 255])
        }))
    }

    #[test]
    fn mostly_red_frame_reports_red_first() {
        let colors = dominant_colors(&split_image([200, 20, 25], [20, 30, 110], 70));
        let names: Vec<_> = colors.iter().map(|c| c.name).collect();
        assert_eq!(names, ["red", "navy"]);
        assert!((colors[0].share - 0.7).abs() < 0.02, "{colors:?}");
        assert_eq!(colors[0].hex(), "#c81419");
        assert_eq!(colors[1].family, "blue");
    }

    #[test]
    fn similar_shades_merge_under_one_name() {
        let image = DynamicImage::ImageRgba8(RgbaImage::from_fn(64, 64, |x, y| {
            Rgba([200 + ((x + y) % 20) as u8, 20, 25, 255])
        }));
        let colors = dominant_colors(&image);
        assert_eq!(colors.len(), 1, "{colors:?}");
        assert_eq!(colors[0].name, "red");
        assert!((colors[0].share - 1.0).abs() < 1e-9);
    }

    #[test]
    fn small_specks_and_transparency_are_ignored() {
        let image = DynamicImage::ImageRgba8(RgbaImage::from_fn(100, 100, |x, y| {
            if y >= 50 {
                Rgba([0, 255, 0, 0])
            } else if x < 2 {
                Rgba([255, 255, 255, 255])
            } else {
                Rgba([0, 0, 0, 255])
            }
        }));
        let colors = dominant_colors(&image);
        assert_eq!(colors.len(), 1, "{colors:?}");
        assert_eq!(colors[0].name, "black");

        let clear = DynamicImage::ImageRgba8(RgbaImage::new(8, 8));
        assert!(dominant_colors(&clear).is_empty());
    }

    #[test]
    fn at_most_five_colors_are_reported() {
        let stripes = [
            [220, 30, 30],
            [30, 90, 220],
            [40, 170, 60],
            [250, 220, 30],
            [0, 0, 0],
            [255, 255, 255],
            [128, 40, 160],
        ];
        let image = DynamicImage::ImageRgba8(RgbaImage::from_fn(70, 10, |x, _| {
            let [r, g, b] = stripes[(x / 10) as usize];
            Rgba([r, g, b, 255])
        }));
        let colors = dominant_colors(&image);
        assert_eq!(colors.len(), MAX_COLORS, "{colors:?}");
        assert!(colors.windows(2).all(|w| w[0].share >= w[1].share));
        // Equal shares tie-break by name, so the result is stable.
        let names: Vec<_> = colors.iter().map(|c| c.name).collect();
        assert_eq!(names, ["black", "blue", "green", "purple", "red"]);
    }

    #[test]
    fn names_follow_perceived_colour() {
        let cases = [
            ([255, 0, 0], "red"),
            ([128, 0, 0], "maroon"),
            ([0, 0, 128], "navy"),
            ([0, 0, 255], "blue"),
            ([250, 250, 250], "white"),
            ([10, 10, 10], "black"),
            ([255, 165, 0], "orange"),
            ([0, 128, 0], "green"),
            ([0, 100, 0], "dark green"),
            ([100, 100, 100], "gray"),
            ([255, 192, 203], "pink"),
            ([128, 0, 128], "purple"),
            ([0, 255, 255], "cyan"),
            ([255, 255, 0], "yellow"),
            ([139, 69, 19], "brown"),
            ([210, 160, 140], "tan"),
        ];
        for (rgb, expected) in cases {
            assert_eq!(nearest_name(rgb), expected, "{rgb:?}");
        }
    }

    #[test]
    fn annotation_is_searchable_by_name_family_and_hex() {
        let color = DominantColor {
            rgb: [20, 30, 100],
            share: 0.314,
            name: "navy",
            family: "blue",
        };
        let a = color.annotation();
        assert_eq!(a.kind, AnnotationKind::Custom);
        assert_eq!(a.provider, PROVIDER);
        assert_eq!(a.text, "color navy blue #141e64 31%");
        assert_eq!(a.attributes["entity"], "color");
        assert_eq!(a.attributes["hex"], "#141e64");
        assert_eq!(a.attributes["share"], "0.314");
        assert_eq!(a.attributes["family"], "blue");
    }

    #[test]
    fn enricher_annotates_visual_units_and_skips_native_text_pages() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("frame.png");
        split_image([200, 20, 25], [200, 20, 25], 100)
            .save(&path)
            .unwrap();
        let graph = DocumentGraph::default();
        let ctx = JobContext {
            workspace: dir.path().to_path_buf(),
            quiet: true,
        };
        let mut unit = Unit::visual(Uuid::new_v4(), path);

        assert!(!ColorEnricher::new(false).supports(&graph, &unit));
        let enricher = ColorEnricher::new(true);
        assert!(enricher.supports(&graph, &unit));
        enricher.enrich_unit(&ctx, &graph, &mut unit).unwrap();
        assert_eq!(unit.annotations.len(), 1);
        assert!(unit.annotations[0].text.starts_with("color red #"));

        unit.metadata.insert(
            crate::importers::TEXT_LAYER_KEY.into(),
            crate::importers::TEXT_LAYER_NATIVE.into(),
        );
        assert!(!enricher.supports(&graph, &unit));
    }
}
