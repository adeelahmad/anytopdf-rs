//! Aligned face crops for downstream face-embedding models.
//!
//! Crops use the 112x112 five-point template of ArcFace-style recognizers, so a
//! later recognition step can embed them without detecting again.

use image::{Rgb, RgbImage};

pub const CROP_SIZE: u32 = 112;

/// Template landmark positions in a 112x112 crop, in YuNet landmark order.
const TEMPLATE: [(f32, f32); 5] = [
    (38.2946, 51.6963),
    (73.5318, 51.5014),
    (56.0252, 71.7366),
    (41.5493, 92.3655),
    (70.7299, 92.2041),
];

/// A similarity transform `(x, y) -> (a*x - b*y + tx, b*x + a*y + ty)`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Similarity {
    pub a: f32,
    pub b: f32,
    pub tx: f32,
    pub ty: f32,
}

impl Similarity {
    /// Least-squares rotation, uniform scale and translation from `from` to `to`.
    pub fn estimate(from: &[(f32, f32)], to: &[(f32, f32)]) -> Option<Self> {
        let n = from.len().min(to.len());
        if n < 2 {
            return None;
        }
        let mean = |points: &[(f32, f32)]| {
            let (sx, sy) = points[..n]
                .iter()
                .fold((0.0, 0.0), |(sx, sy), (x, y)| (sx + x, sy + y));
            (sx / n as f32, sy / n as f32)
        };
        let (fx, fy) = mean(from);
        let (tx, ty) = mean(to);
        let (mut dot, mut cross, mut norm) = (0.0f32, 0.0f32, 0.0f32);
        for i in 0..n {
            let (xs, ys) = (from[i].0 - fx, from[i].1 - fy);
            let (xd, yd) = (to[i].0 - tx, to[i].1 - ty);
            dot += xs * xd + ys * yd;
            cross += xs * yd - ys * xd;
            norm += xs * xs + ys * ys;
        }
        if norm.is_nan() || norm <= f32::EPSILON {
            return None;
        }
        let a = dot / norm;
        let b = cross / norm;
        Some(Self {
            a,
            b,
            tx: tx - (a * fx - b * fy),
            ty: ty - (b * fx + a * fy),
        })
    }

    pub fn apply(&self, (x, y): (f32, f32)) -> (f32, f32) {
        (
            self.a * x - self.b * y + self.tx,
            self.b * x + self.a * y + self.ty,
        )
    }

    pub fn inverse(&self) -> Option<Self> {
        let det = self.a * self.a + self.b * self.b;
        if det.is_nan() || det <= f32::EPSILON {
            return None;
        }
        let (a, b) = (self.a / det, -self.b / det);
        Some(Self {
            a,
            b,
            tx: -(a * self.tx - b * self.ty),
            ty: -(b * self.tx + a * self.ty),
        })
    }
}

/// Warps the face with these landmarks (source pixels) onto the template.
pub fn aligned_crop(image: &RgbImage, landmarks: &[(f32, f32); 5]) -> Option<RgbImage> {
    let inverse = Similarity::estimate(landmarks, &TEMPLATE)?.inverse()?;
    Some(RgbImage::from_fn(CROP_SIZE, CROP_SIZE, |u, v| {
        let (x, y) = inverse.apply((u as f32 + 0.5, v as f32 + 0.5));
        bilinear(image, x - 0.5, y - 0.5)
    }))
}

fn bilinear(image: &RgbImage, x: f32, y: f32) -> Rgb<u8> {
    let (w, h) = (image.width() as f32, image.height() as f32);
    if !(x > -1.0 && y > -1.0 && x < w && y < h) {
        return Rgb([0, 0, 0]);
    }
    let (x0, y0) = (x.floor(), y.floor());
    let (dx, dy) = (x - x0, y - y0);
    let sample = |sx: f32, sy: f32| -> [f32; 3] {
        if sx < 0.0 || sy < 0.0 || sx >= w || sy >= h {
            return [0.0; 3];
        }
        let p = image.get_pixel(sx as u32, sy as u32);
        [f32::from(p[0]), f32::from(p[1]), f32::from(p[2])]
    };
    let (p00, p10) = (sample(x0, y0), sample(x0 + 1.0, y0));
    let (p01, p11) = (sample(x0, y0 + 1.0), sample(x0 + 1.0, y0 + 1.0));
    let mut out = [0u8; 3];
    for c in 0..3 {
        let top = p00[c] * (1.0 - dx) + p10[c] * dx;
        let bottom = p01[c] * (1.0 - dx) + p11[c] * dx;
        out[c] = (top * (1.0 - dy) + bottom * dy).round().clamp(0.0, 255.0) as u8;
    }
    Rgb(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: (f32, f32), b: (f32, f32)) -> bool {
        (a.0 - b.0).abs() < 1e-3 && (a.1 - b.1).abs() < 1e-3
    }

    #[test]
    fn estimate_recovers_a_known_similarity() {
        let truth = Similarity {
            a: 0.6,
            b: 0.8,
            tx: 5.0,
            ty: -3.0,
        };
        let from = [(0.0, 0.0), (10.0, 0.0), (0.0, 10.0), (7.0, 3.0)];
        let to: Vec<_> = from.iter().map(|&p| truth.apply(p)).collect();
        let found = Similarity::estimate(&from, &to).unwrap();
        for p in from {
            assert!(close(found.apply(p), truth.apply(p)));
        }
        let back = found.inverse().unwrap();
        assert!(close(back.apply(truth.apply((7.0, 3.0))), (7.0, 3.0)));
    }

    #[test]
    fn estimate_rejects_degenerate_points() {
        assert!(Similarity::estimate(&[(1.0, 1.0); 5], &TEMPLATE).is_none());
        assert!(Similarity::estimate(&[(1.0, 1.0)], &TEMPLATE).is_none());
    }

    #[test]
    fn crop_of_template_landmarks_is_the_identity() {
        let image = RgbImage::from_fn(112, 112, |x, y| Rgb([x as u8, y as u8, 7]));
        let crop = aligned_crop(&image, &TEMPLATE).unwrap();
        assert_eq!(crop.dimensions(), (112, 112));
        assert_eq!(crop.get_pixel(40, 90), &Rgb([40, 90, 7]));
    }
}
