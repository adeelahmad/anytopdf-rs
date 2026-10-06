//! Five-point face alignment onto the ArcFace 112x112 template.

use anyhow::{Context, Result, bail};
use image::{Rgb, RgbImage};

/// Canonical landmark positions for a 112x112 ArcFace crop, in InsightFace
/// order: left eye, right eye, nose tip, left and right mouth corner (image
/// left/right).
pub const ARCFACE_TEMPLATE: [[f32; 2]; 5] = [
    [38.2946, 51.6963],
    [73.5318, 51.5014],
    [56.0252, 71.7366],
    [41.5493, 92.3655],
    [70.7299, 92.2041],
];

/// Parses the `landmarks` attribute: five points normalized to the unit
/// image, either as ten comma-separated numbers or as a JSON array of pairs
/// (`[[x,y],…]`, the `anytopdf-plugin-faces` format), image-left eye first.
pub fn parse_landmarks(value: &str) -> Result<[[f32; 2]; 5]> {
    let numbers = value
        .split(',')
        .map(|n| n.trim_matches(|c: char| c.is_whitespace() || c == '[' || c == ']'))
        .map(str::parse::<f32>)
        .collect::<Result<Vec<_>, _>>()
        .context("landmarks must be comma-separated numbers")?;
    if numbers.len() != 10 || numbers.iter().any(|n| !n.is_finite()) {
        bail!("landmarks must hold ten finite numbers");
    }
    let mut points = [[0.0; 2]; 5];
    for (i, point) in points.iter_mut().enumerate() {
        *point = [numbers[2 * i], numbers[2 * i + 1]];
    }
    Ok(points)
}

/// A similarity transform `dst = [a -b; b a] * src + t`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Similarity {
    pub a: f32,
    pub b: f32,
    pub tx: f32,
    pub ty: f32,
}

impl Similarity {
    /// Least-squares fit (rotation, uniform scale, translation, no reflection).
    pub fn estimate(src: &[[f32; 2]], dst: &[[f32; 2]]) -> Result<Self> {
        if src.len() != dst.len() || src.len() < 2 {
            bail!("need at least two point pairs");
        }
        let n = src.len() as f32;
        let mean = |pts: &[[f32; 2]]| {
            let (x, y) = pts
                .iter()
                .fold((0.0, 0.0), |(x, y), p| (x + p[0], y + p[1]));
            (x / n, y / n)
        };
        let (mx, my) = mean(src);
        let (mu, mv) = mean(dst);
        let (mut num_a, mut num_b, mut den) = (0.0, 0.0, 0.0);
        for (s, d) in src.iter().zip(dst) {
            let (x, y) = (s[0] - mx, s[1] - my);
            let (u, v) = (d[0] - mu, d[1] - mv);
            num_a += x * u + y * v;
            num_b += x * v - y * u;
            den += x * x + y * y;
        }
        if den <= f32::EPSILON {
            bail!("landmarks are degenerate");
        }
        let (a, b) = (num_a / den, num_b / den);
        Ok(Self {
            a,
            b,
            tx: mu - a * mx + b * my,
            ty: mv - b * mx - a * my,
        })
    }

    pub fn apply(&self, [x, y]: [f32; 2]) -> [f32; 2] {
        [
            self.a * x - self.b * y + self.tx,
            self.b * x + self.a * y + self.ty,
        ]
    }

    pub fn inverse(&self) -> Self {
        let det = self.a * self.a + self.b * self.b;
        let (a, b) = (self.a / det, -self.b / det);
        Self {
            a,
            b,
            tx: -(a * self.tx - b * self.ty),
            ty: -(b * self.tx + a * self.ty),
        }
    }
}

/// Warps `image` so the normalized `landmarks` land on the ArcFace template of
/// a `size` x `size` crop. Pixels outside the source are black.
pub fn align(image: &RgbImage, landmarks: &[[f32; 2]; 5], size: u32) -> Result<RgbImage> {
    let (w, h) = (image.width() as f32, image.height() as f32);
    if w < 1.0 || h < 1.0 || size == 0 {
        bail!("empty image");
    }
    let scale = size as f32 / 112.0;
    let src: Vec<[f32; 2]> = landmarks.iter().map(|p| [p[0] * w, p[1] * h]).collect();
    let dst: Vec<[f32; 2]> = ARCFACE_TEMPLATE
        .iter()
        .map(|p| [p[0] * scale, p[1] * scale])
        .collect();
    let back = Similarity::estimate(&src, &dst)?.inverse();
    Ok(RgbImage::from_fn(size, size, |x, y| {
        let [sx, sy] = back.apply([x as f32 + 0.5, y as f32 + 0.5]);
        bilinear(image, sx - 0.5, sy - 0.5)
    }))
}

fn bilinear(image: &RgbImage, x: f32, y: f32) -> Rgb<u8> {
    let (w, h) = (image.width() as i64, image.height() as i64);
    if !(x > -1.0 && y > -1.0 && x < w as f32 && y < h as f32) {
        return Rgb([0, 0, 0]);
    }
    let (x0, y0) = (x.floor() as i64, y.floor() as i64);
    let (fx, fy) = (x - x0 as f32, y - y0 as f32);
    let pixel = |px: i64, py: i64| -> [f32; 3] {
        if px < 0 || py < 0 || px >= w || py >= h {
            [0.0; 3]
        } else {
            image.get_pixel(px as u32, py as u32).0.map(f32::from)
        }
    };
    let (p00, p10, p01, p11) = (
        pixel(x0, y0),
        pixel(x0 + 1, y0),
        pixel(x0, y0 + 1),
        pixel(x0 + 1, y0 + 1),
    );
    let mut out = [0u8; 3];
    for c in 0..3 {
        let top = p00[c] + (p10[c] - p00[c]) * fx;
        let bottom = p01[c] + (p11[c] - p01[c]) * fx;
        out[c] = (top + (bottom - top) * fy).round().clamp(0.0, 255.0) as u8;
    }
    Rgb(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn similarity_recovers_a_known_transform() {
        let truth = Similarity {
            a: 0.8,
            b: 0.3,
            tx: 5.0,
            ty: -2.0,
        };
        let src = [[1.0, 2.0], [10.0, 3.0], [4.0, 9.0], [7.0, 7.0]];
        let dst: Vec<_> = src.iter().map(|p| truth.apply(*p)).collect();
        let fit = Similarity::estimate(&src, &dst).unwrap();
        for (got, want) in [
            (fit.a, truth.a),
            (fit.b, truth.b),
            (fit.tx, truth.tx),
            (fit.ty, truth.ty),
        ] {
            assert!((got - want).abs() < 1e-4, "{fit:?}");
        }
        let round = fit.inverse().apply(fit.apply([3.0, 4.0]));
        assert!((round[0] - 3.0).abs() < 1e-4 && (round[1] - 4.0).abs() < 1e-4);
    }

    #[test]
    fn landmarks_must_be_ten_finite_numbers() {
        assert!(parse_landmarks("0.1,0.2,0.3,0.4,0.5,0.6,0.7,0.8,0.9,1.0").is_ok());
        let json = parse_landmarks("[[0.1,0.2],[0.3,0.4],[0.5,0.6],[0.7,0.8],[0.9,1.0]]").unwrap();
        assert_eq!(json[4], [0.9, 1.0]);
        assert!(parse_landmarks("[[0.1,0.2],[0.3,0.4]]").is_err());
        assert!(parse_landmarks("0.1,0.2").is_err());
        assert!(parse_landmarks("a,b,c,d,e,f,g,h,i,j").is_err());
        assert!(parse_landmarks("NaN,0,0,0,0,0,0,0,0,0").is_err());
    }

    #[test]
    fn align_moves_the_left_eye_onto_the_template() {
        // A 224x224 image with a red dot where the left eye is.
        let mut image = RgbImage::new(224, 224);
        let marks = ARCFACE_TEMPLATE.map(|p| [p[0] * 2.0 / 224.0, p[1] * 2.0 / 224.0]);
        for dy in 0..4 {
            for dx in 0..4 {
                image.put_pixel(75 + dx, 102 + dy, Rgb([255, 0, 0]));
            }
        }
        let crop = align(&image, &marks, 112).unwrap();
        assert_eq!(crop.dimensions(), (112, 112));
        assert!(crop.get_pixel(38, 51)[0] > 200, "eye dot not at template");
        assert_eq!(crop.get_pixel(5, 5).0, [0, 0, 0]);
    }

    #[test]
    fn degenerate_landmarks_are_rejected() {
        let image = RgbImage::new(10, 10);
        assert!(align(&image, &[[0.5, 0.5]; 5], 112).is_err());
    }
}
