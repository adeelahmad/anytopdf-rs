//! Document-scan cleanup for photographed pages: finds the sheet of paper in a
//! phone photo, corrects its perspective, and straightens skewed text lines,
//! so OCR reads an upright, flat page.
//!
//! Analysis runs on a downscaled greyscale copy; the full-resolution image is
//! resampled once with the combined page and deskew transform.

use anyhow::{Context, Result};
use anytopdf_core::{
    DocumentGraph, JobContext, Plugin, PluginDescriptor, SourceRecord, Unit, UnitEnricher, UnitKind,
};
use image::{GrayImage, Rgb, RgbImage, imageops};
use std::fmt;
use std::str::FromStr;

/// Long edge of the greyscale copy the detectors work on.
const WORK_EDGE: u32 = 800;
/// Images smaller than this on their long edge are left alone.
const MIN_EDGE: u32 = 200;
/// Skew below this many degrees is not worth a resample.
const MIN_SKEW_DEGREES: f64 = 0.3;
const MAX_SKEW_DEGREES: f64 = 15.0;

pub const SCAN_PAGE_KEY: &str = "scan.page";
pub const SCAN_DESKEW_KEY: &str = "scan.deskew-degrees";

/// When photographed pages are flattened and straightened.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ScanMode {
    /// Never change page images.
    Off,
    /// Only when the photo clearly shows a document: a sheet with text on a
    /// distinct background, or a page-filling scan with skewed text lines.
    #[default]
    Auto,
    /// Crop to any detected sheet and straighten every photo with text lines.
    On,
}

impl FromStr for ScanMode {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "off" | "none" => Ok(Self::Off),
            "auto" => Ok(Self::Auto),
            "on" => Ok(Self::On),
            other => Err(format!(
                "unknown scan mode {other:?} (expected auto, on or off)"
            )),
        }
    }
}

impl fmt::Display for ScanMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Off => "off",
            Self::Auto => "auto",
            Self::On => "on",
        })
    }
}

type Point = (f64, f64);

/// Normalised greyscale working copy, values 0 to 255.
#[derive(Clone)]
struct Gray {
    w: usize,
    h: usize,
    px: Vec<f32>,
}

impl Gray {
    fn from_image(img: &GrayImage) -> Self {
        Self {
            w: img.width() as usize,
            h: img.height() as usize,
            px: img.pixels().map(|p| f32::from(p.0[0])).collect(),
        }
    }

    fn at(&self, x: usize, y: usize) -> f32 {
        self.px[y * self.w + x]
    }

    /// Approximately Gaussian blur: three separable box passes.
    fn blurred(&self, radius: usize) -> Self {
        let mut out = self.clone();
        let mut tmp = vec![0f32; out.px.len()];
        for _ in 0..3 {
            box_pass(&out.px, &mut tmp, out.w, out.h, radius, true);
            box_pass(&tmp, &mut out.px, out.w, out.h, radius, false);
        }
        out
    }

    fn bilinear(&self, x: f64, y: f64) -> Option<f32> {
        sample(x, y, self.w, self.h, |xi, yi| [self.at(xi, yi)]).map(|[v]| v)
    }
}

fn box_pass(src: &[f32], dst: &mut [f32], w: usize, h: usize, r: usize, horizontal: bool) {
    let (lines, len) = if horizontal { (h, w) } else { (w, h) };
    let index = |line: usize, i: usize| {
        if horizontal {
            line * w + i
        } else {
            i * w + line
        }
    };
    for line in 0..lines {
        let mut sum = 0f32;
        let mut count = 0f32;
        for i in 0..r.min(len) {
            sum += src[index(line, i)];
            count += 1.0;
        }
        for i in 0..len {
            if i + r < len {
                sum += src[index(line, i + r)];
                count += 1.0;
            }
            if i > r {
                sum -= src[index(line, i - r - 1)];
                count -= 1.0;
            }
            dst[index(line, i)] = sum / count;
        }
    }
}

/// Bilinear sample of a `w`x`h` grid at pixel-centre coordinates.
fn sample<const N: usize>(
    x: f64,
    y: f64,
    w: usize,
    h: usize,
    get: impl Fn(usize, usize) -> [f32; N],
) -> Option<[f32; N]> {
    let (fx, fy) = (x - 0.5, y - 0.5);
    if !(fx > -1.0 && fy > -1.0 && fx < w as f64 && fy < h as f64) {
        return None;
    }
    let x0 = fx.floor().clamp(0.0, (w - 1) as f64) as usize;
    let y0 = fy.floor().clamp(0.0, (h - 1) as f64) as usize;
    let x1 = (x0 + 1).min(w - 1);
    let y1 = (y0 + 1).min(h - 1);
    let tx = (fx - x0 as f64).clamp(0.0, 1.0) as f32;
    let ty = (fy - y0 as f64).clamp(0.0, 1.0) as f32;
    let (a, b, c, d) = (get(x0, y0), get(x1, y0), get(x0, y1), get(x1, y1));
    Some(std::array::from_fn(|i| {
        let top = a[i] + (b[i] - a[i]) * tx;
        let bottom = c[i] + (d[i] - c[i]) * tx;
        top + (bottom - top) * ty
    }))
}

/// Otsu's threshold with the means of the dark and bright classes.
fn otsu(values: &[f32]) -> (f32, f32, f32) {
    let mut hist = [0u64; 256];
    for &v in values {
        hist[v.clamp(0.0, 255.0) as usize] += 1;
    }
    let total = values.len() as f64;
    let sum_all: f64 = hist
        .iter()
        .enumerate()
        .map(|(i, &c)| i as f64 * c as f64)
        .sum();
    let (mut w_lo, mut sum_lo) = (0f64, 0f64);
    let (mut best, mut best_t, mut means) = (-1f64, 0usize, (0f64, 0f64));
    for (t, &count) in hist.iter().enumerate() {
        w_lo += count as f64;
        sum_lo += t as f64 * count as f64;
        let w_hi = total - w_lo;
        if w_lo == 0.0 || w_hi == 0.0 {
            continue;
        }
        let (m_lo, m_hi) = (sum_lo / w_lo, (sum_all - sum_lo) / w_hi);
        let between = w_lo * w_hi * (m_lo - m_hi).powi(2);
        if between > best {
            (best, best_t, means) = (between, t, (m_lo, m_hi));
        }
    }
    (best_t as f32 + 0.5, means.0 as f32, means.1 as f32)
}

/// Projective transform, row-major with the last entry fixed at 1.
#[derive(Debug, Clone, Copy)]
struct Homography([f64; 8]);

impl Homography {
    /// The transform that maps each `from[i]` onto `to[i]`.
    fn between(from: [Point; 4], to: [Point; 4]) -> Option<Self> {
        let mut m = [[0f64; 9]; 8];
        for (i, ((x, y), (u, v))) in from.into_iter().zip(to).enumerate() {
            m[2 * i] = [x, y, 1.0, 0.0, 0.0, 0.0, -u * x, -u * y, u];
            m[2 * i + 1] = [0.0, 0.0, 0.0, x, y, 1.0, -v * x, -v * y, v];
        }
        for col in 0..8 {
            let pivot = (col..8).max_by(|&a, &b| m[a][col].abs().total_cmp(&m[b][col].abs()))?;
            if m[pivot][col].abs() < 1e-12 {
                return None;
            }
            m.swap(col, pivot);
            for row in 0..8 {
                if row != col {
                    let f = m[row][col] / m[col][col];
                    let pivot_row = m[col];
                    for (cell, p) in m[row].iter_mut().zip(pivot_row).skip(col) {
                        *cell -= f * p;
                    }
                }
            }
        }
        Some(Self(std::array::from_fn(|i| m[i][8] / m[i][i])))
    }

    fn apply(&self, (x, y): Point) -> Point {
        let h = &self.0;
        let d = h[6] * x + h[7] * y + 1.0;
        (
            (h[0] * x + h[1] * y + h[2]) / d,
            (h[3] * x + h[4] * y + h[5]) / d,
        )
    }
}

fn cross(o: Point, a: Point, b: Point) -> f64 {
    (a.0 - o.0) * (b.1 - o.1) - (a.1 - o.1) * (b.0 - o.0)
}

fn polygon_area(pts: &[Point]) -> f64 {
    let n = pts.len();
    (0..n)
        .map(|i| {
            let (a, b) = (pts[i], pts[(i + 1) % n]);
            a.0 * b.1 - b.0 * a.1
        })
        .sum::<f64>()
        .abs()
        / 2.0
}

fn distance(a: Point, b: Point) -> f64 {
    (a.0 - b.0).hypot(a.1 - b.1)
}

/// Andrew's monotone chain; counter-clockwise in a y-up frame.
fn convex_hull(mut pts: Vec<Point>) -> Vec<Point> {
    pts.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.total_cmp(&b.1)));
    pts.dedup();
    if pts.len() < 3 {
        return pts;
    }
    let mut hull: Vec<Point> = Vec::with_capacity(pts.len() * 2);
    for pass in [false, true] {
        let start = hull.len();
        let iter: Box<dyn Iterator<Item = &Point>> = if pass {
            Box::new(pts.iter().rev())
        } else {
            Box::new(pts.iter())
        };
        for &p in iter {
            while hull.len() >= start + 2
                && cross(hull[hull.len() - 2], hull[hull.len() - 1], p) <= 0.0
            {
                hull.pop();
            }
            hull.push(p);
        }
        hull.pop();
    }
    hull
}

/// Douglas-Peucker simplification of one open chain.
fn simplify_chain(chain: &[Point], eps: f64, out: &mut Vec<Point>) {
    let (first, last) = (chain[0], chain[chain.len() - 1]);
    let len = distance(first, last).max(1e-9);
    let farthest = chain[1..chain.len() - 1]
        .iter()
        .enumerate()
        .map(|(i, &p)| (i + 1, cross(first, last, p).abs() / len))
        .max_by(|a, b| a.1.total_cmp(&b.1));
    match farthest {
        Some((i, d)) if d > eps => {
            simplify_chain(&chain[..=i], eps, out);
            out.pop();
            simplify_chain(&chain[i..], eps, out);
        }
        _ => out.extend([first, last]),
    }
}

fn simplify_polygon(hull: &[Point], eps: f64) -> Vec<Point> {
    let far = (0..hull.len())
        .max_by(|&a, &b| distance(hull[0], hull[a]).total_cmp(&distance(hull[0], hull[b])))
        .unwrap_or(0);
    if far == 0 {
        return hull.to_vec();
    }
    let mut out = Vec::new();
    simplify_chain(&hull[..=far], eps, &mut out);
    out.pop();
    let mut back: Vec<Point> = hull[far..].to_vec();
    back.push(hull[0]);
    simplify_chain(&back, eps, &mut out);
    out.pop();
    out
}

/// Largest-area quadrilateral whose corners are vertices of a convex polygon.
fn largest_quad(poly: &[Point]) -> Option<[Point; 4]> {
    let n = poly.len();
    if n < 4 {
        return None;
    }
    let mut best = (0f64, [0usize; 4]);
    for i in 0..n {
        for j in i + 1..n {
            for k in j + 1..n {
                for l in k + 1..n {
                    let area = polygon_area(&[poly[i], poly[j], poly[k], poly[l]]);
                    if area > best.0 {
                        best = (area, [i, j, k, l]);
                    }
                }
            }
        }
    }
    Some(best.1.map(|i| poly[i]))
}

/// Orders corners top-left, top-right, bottom-right, bottom-left.
fn order_corners(mut quad: [Point; 4]) -> [Point; 4] {
    let cx = quad.iter().map(|p| p.0).sum::<f64>() / 4.0;
    let cy = quad.iter().map(|p| p.1).sum::<f64>() / 4.0;
    // Increasing atan2 in a y-down frame runs clockwise on screen.
    quad.sort_by(|a, b| {
        (a.1 - cy)
            .atan2(a.0 - cx)
            .total_cmp(&(b.1 - cy).atan2(b.0 - cx))
    });
    let first = (0..4)
        .min_by(|&a, &b| (quad[a].0 + quad[a].1).total_cmp(&(quad[b].0 + quad[b].1)))
        .unwrap_or(0);
    quad.rotate_left(first);
    quad
}

fn interior_angles_ok(q: &[Point; 4]) -> bool {
    (0..4).all(|i| {
        let (prev, p, next) = (q[(i + 3) % 4], q[i], q[(i + 1) % 4]);
        let a = (prev.0 - p.0, prev.1 - p.1);
        let b = (next.0 - p.0, next.1 - p.1);
        let cos = (a.0 * b.0 + a.1 * b.1) / (a.0.hypot(a.1) * b.0.hypot(b.1)).max(1e-9);
        let deg = cos.clamp(-1.0, 1.0).acos().to_degrees();
        (40.0..=140.0).contains(&deg)
    })
}

/// Finds a bright sheet on a darker background, in work-copy coordinates.
fn find_page(gray: &Gray, mode: ScanMode) -> Option<[Point; 4]> {
    let (w, h) = (gray.w, gray.h);
    // Blur away text so the sheet reads as one solid bright region.
    let blurred = gray.blurred((w.max(h) / 100).max(2));
    let (threshold, dark, bright) = otsu(&blurred.px);
    if bright - dark < 40.0 {
        return None;
    }
    let mask: Vec<bool> = blurred.px.iter().map(|&v| v > threshold).collect();

    // Largest 4-connected bright component.
    let mut label = vec![0u32; w * h];
    let (mut best_label, mut best_area, mut next) = (0u32, 0usize, 0u32);
    let mut stack = Vec::new();
    for start in 0..w * h {
        if !mask[start] || label[start] != 0 {
            continue;
        }
        next += 1;
        label[start] = next;
        stack.push(start);
        let mut area = 0;
        while let Some(i) = stack.pop() {
            area += 1;
            let (x, y) = (i % w, i / w);
            let mut visit = |j: usize| {
                if mask[j] && label[j] == 0 {
                    label[j] = next;
                    stack.push(j);
                }
            };
            if x > 0 {
                visit(i - 1);
            }
            if x + 1 < w {
                visit(i + 1);
            }
            if y > 0 {
                visit(i - w);
            }
            if y + 1 < h {
                visit(i + w);
            }
        }
        if area > best_area {
            (best_label, best_area) = (next, area);
        }
    }
    let frame = (w * h) as f64;
    if best_label == 0 || (best_area as f64) < 0.15 * frame || (best_area as f64) > 0.98 * frame {
        return None;
    }

    // Row extents of the component outline the sheet.
    let mut extents = Vec::with_capacity(h * 2);
    for y in 0..h {
        let row = &label[y * w..(y + 1) * w];
        if let (Some(l), Some(r)) = (
            row.iter().position(|&v| v == best_label),
            row.iter().rposition(|&v| v == best_label),
        ) {
            for yy in [y as f64, y as f64 + 1.0] {
                extents.push((l as f64, yy));
                extents.push((r as f64 + 1.0, yy));
            }
        }
    }
    let hull = convex_hull(extents);
    let mut eps = w.max(h) as f64 * 0.005;
    let mut poly = simplify_polygon(&hull, eps);
    while poly.len() > 24 {
        eps *= 1.5;
        poly = simplify_polygon(&hull, eps);
    }
    let quad = order_corners(largest_quad(&poly)?);
    let quad_area = polygon_area(&quad);
    let fill = best_area as f64 / quad_area;
    if quad_area < 0.15 * frame || !(0.88..=1.12).contains(&fill) || !interior_angles_ok(&quad) {
        return None;
    }
    let margin = w.max(h) as f64 * 0.02;
    let on_border = quad
        .iter()
        .filter(|p| {
            p.0 < margin || p.1 < margin || p.0 > w as f64 - margin || p.1 > h as f64 - margin
        })
        .count();
    // A real sheet is surrounded by background on at least three sides. A
    // bright band hugging the frame (sky, a wall) would otherwise be cropped.
    let limit = if mode == ScanMode::On { 3 } else { 1 };
    (on_border <= limit).then(|| refine_quad(gray, quad))
}

/// Total-least-squares line through `pts`: a point on it and its direction.
fn fit_line(pts: &[Point]) -> Option<(Point, Point)> {
    if pts.len() < 2 {
        return None;
    }
    let n = pts.len() as f64;
    let (mx, my) = (
        pts.iter().map(|p| p.0).sum::<f64>() / n,
        pts.iter().map(|p| p.1).sum::<f64>() / n,
    );
    let (mut sxx, mut syy, mut sxy) = (0.0, 0.0, 0.0);
    for &(x, y) in pts {
        sxx += (x - mx).powi(2);
        syy += (y - my).powi(2);
        sxy += (x - mx) * (y - my);
    }
    let theta = 0.5 * (2.0 * sxy).atan2(sxx - syy);
    Some(((mx, my), (theta.cos(), theta.sin())))
}

fn intersect(a: (Point, Point), b: (Point, Point)) -> Option<Point> {
    let ((p, r), (q, s)) = (a, b);
    let denom = r.0 * s.1 - r.1 * s.0;
    if denom.abs() < 1e-9 {
        return None;
    }
    let t = ((q.0 - p.0) * s.1 - (q.1 - p.1) * s.0) / denom;
    Some((p.0 + t * r.0, p.1 + t * r.1))
}

/// Snaps each side of a rough sheet outline to the strongest bright-to-dark
/// edge nearby and rebuilds the corners from the fitted lines. The rough
/// outline comes from a heavily blurred mask, which rounds corners and drifts
/// under uneven lighting.
fn refine_quad(gray: &Gray, quad: [Point; 4]) -> [Point; 4] {
    let smooth = gray.blurred(1);
    let reach = (gray.w.max(gray.h) as f64 * 0.025).max(4.0);
    let cx = quad.iter().map(|p| p.0).sum::<f64>() / 4.0;
    let cy = quad.iter().map(|p| p.1).sum::<f64>() / 4.0;
    let mut lines = Vec::with_capacity(4);
    for i in 0..4 {
        let (p, q) = (quad[i], quad[(i + 1) % 4]);
        let len = distance(p, q).max(1e-9);
        let dir = ((q.0 - p.0) / len, (q.1 - p.1) / len);
        let mut normal = (dir.1, -dir.0);
        let mid = ((p.0 + q.0) / 2.0, (p.1 + q.1) / 2.0);
        if (mid.0 - cx) * normal.0 + (mid.1 - cy) * normal.1 < 0.0 {
            normal = (-normal.0, -normal.1);
        }
        let mut edge = Vec::new();
        for k in 1..48 {
            let t = 0.08 + 0.84 * f64::from(k) / 48.0;
            let m = (p.0 + t * (q.0 - p.0), p.1 + t * (q.1 - p.1));
            let at = |s: f64| smooth.bilinear(m.0 + s * normal.0, m.1 + s * normal.1);
            let mut best: Option<(f64, f32)> = None;
            let mut s = -reach;
            while s <= reach {
                // Brightness drop when stepping outward across the sheet edge.
                if let (Some(inside), Some(outside)) = (at(s - 1.0), at(s + 1.0)) {
                    let drop = inside - outside;
                    if best.is_none_or(|(_, d)| drop > d) {
                        best = Some((s, drop));
                    }
                }
                s += 0.5;
            }
            if let Some((s, _)) = best.filter(|&(_, d)| d > 8.0) {
                edge.push((m.0 + s * normal.0, m.1 + s * normal.1));
            }
        }
        let Some(mut line) = fit_line(&edge) else {
            return quad;
        };
        // One refit without outliers (text or shadows near the edge).
        let residual =
            |l: (Point, Point), e: Point| cross(l.0, (l.0.0 + l.1.0, l.0.1 + l.1.1), e).abs();
        let inliers: Vec<Point> = edge
            .iter()
            .copied()
            .filter(|&e| residual(line, e) <= 1.5)
            .collect();
        if inliers.len() >= edge.len() / 2 {
            line = fit_line(&inliers).unwrap_or(line);
        }
        if inliers.len() < 12 {
            return quad;
        }
        lines.push(line);
    }
    let mut refined = quad;
    for (i, corner) in refined.iter_mut().enumerate() {
        match intersect(lines[(i + 3) % 4], lines[i]) {
            Some(c) if distance(c, quad[i]) <= reach * 2.0 => *corner = c,
            _ => return quad,
        }
    }
    if interior_angles_ok(&refined) {
        refined
    } else {
        quad
    }
}

/// Pixels clearly darker than their surroundings: text strokes and rules.
fn ink_points(gray: &Gray) -> Vec<Point> {
    let background = gray.blurred((gray.w.max(gray.h) / 40).max(3));
    let mut pts = Vec::new();
    for y in 0..gray.h {
        for x in 0..gray.w {
            let (v, bg) = (gray.at(x, y), background.at(x, y));
            if bg - v > 20.0_f32.max(0.2 * bg) {
                pts.push((x as f64 + 0.5, y as f64 + 0.5));
            }
        }
    }
    pts
}

fn ink_fraction_ok(count: usize, gray: &Gray) -> bool {
    let fraction = count as f64 / (gray.w * gray.h) as f64;
    (0.002..=0.35).contains(&fraction)
}

/// Sharpness of the horizontal projection of `pts` when rotated by `degrees`.
fn profile_score(pts: &[Point], degrees: f64, height: usize) -> f64 {
    let (sin, cos) = degrees.to_radians().sin_cos();
    let offset = height as f64;
    let mut bins = vec![0u32; height * 3];
    for &(x, y) in pts {
        let projected = y * cos - x * sin + offset;
        if let Some(bin) = bins.get_mut(projected.max(0.0) as usize) {
            *bin += 1;
        }
    }
    bins.iter().map(|&c| f64::from(c).powi(2)).sum()
}

/// Angle in degrees that text lines slope down to the right, if clear.
fn estimate_skew(gray: &Gray, ink: &[Point]) -> Option<f64> {
    if !ink_fraction_ok(ink.len(), gray) {
        return None;
    }
    let step = (ink.len() / 150_000).max(1);
    let pts: Vec<Point> = ink.iter().step_by(step).copied().collect();
    let search = |from: f64, to: f64, by: f64| {
        let steps = ((to - from) / by).round() as i64;
        (0..=steps)
            .map(|i| from + i as f64 * by)
            .map(|a| (a, profile_score(&pts, a, gray.h)))
            .max_by(|a, b| a.1.total_cmp(&b.1))
            .expect("non-empty range")
    };
    let (coarse, _) = search(-MAX_SKEW_DEGREES, MAX_SKEW_DEGREES, 0.5);
    let (angle, score) = search(coarse - 0.5, coarse + 0.5, 0.1);
    let level = profile_score(&pts, 0.0, gray.h);
    // Text lines give a markedly sharper profile at the right angle.
    (angle.abs() < MAX_SKEW_DEGREES - 0.5 && score > level * 1.05).then_some(angle)
}

/// What the cleanup found and the corrected page.
pub(crate) struct Correction {
    pub image: RgbImage,
    /// Sheet corners in the original image, normalised, TL TR BR BL.
    pub page: Option<[Point; 4]>,
    pub deskew_degrees: f64,
}

fn work_copy(img: &RgbImage) -> (Gray, f64) {
    let scale = f64::from(WORK_EDGE) / f64::from(img.width().max(img.height()));
    let luma = imageops::grayscale(img);
    if scale >= 1.0 {
        return (Gray::from_image(&luma), 1.0);
    }
    let w = ((f64::from(img.width()) * scale).round() as u32).max(1);
    let h = ((f64::from(img.height()) * scale).round() as u32).max(1);
    let small = imageops::resize(&luma, w, h, imageops::FilterType::Triangle);
    (
        Gray::from_image(&small),
        f64::from(w) / f64::from(img.width()),
    )
}

/// Output size for a sheet with these corners.
fn page_size(q: &[Point; 4]) -> (f64, f64) {
    let w = distance(q[0], q[1]).max(distance(q[3], q[2]));
    let h = distance(q[0], q[3]).max(distance(q[1], q[2]));
    (w.max(1.0), h.max(1.0))
}

/// Maps output pixels back to source pixels for a page warp plus a rotation.
struct Mapping {
    page: Option<Homography>,
    /// Size of the page before rotation.
    flat: (f64, f64),
    out: (f64, f64),
    sin_cos: (f64, f64),
}

impl Mapping {
    fn new(
        page: Option<([Point; 4], (f64, f64))>,
        source: (f64, f64),
        degrees: f64,
    ) -> Option<Self> {
        let (homography, flat) = match page {
            Some((corners, size)) => {
                let rect = [(0.0, 0.0), (size.0, 0.0), (size.0, size.1), (0.0, size.1)];
                (Some(Homography::between(rect, corners)?), size)
            }
            None => (None, source),
        };
        let (sin, cos) = degrees.to_radians().sin_cos();
        let out = (
            (flat.0 * cos.abs() + flat.1 * sin.abs()).round().max(1.0),
            (flat.0 * sin.abs() + flat.1 * cos.abs()).round().max(1.0),
        );
        Some(Self {
            page: homography,
            flat,
            out,
            sin_cos: (sin, cos),
        })
    }

    /// The source pixel for an output pixel; `None` outside the sheet.
    fn source_of(&self, (x, y): Point) -> Option<Point> {
        let (sin, cos) = self.sin_cos;
        let (dx, dy) = (x - self.out.0 / 2.0, y - self.out.1 / 2.0);
        // Undo the deskew: a point on a straightened line came from the
        // sloped line in the flat page.
        let flat = (
            dx * cos - dy * sin + self.flat.0 / 2.0,
            dx * sin + dy * cos + self.flat.1 / 2.0,
        );
        match &self.page {
            // Rotation margins must not show the table around the sheet.
            Some(h) => ((0.0..=self.flat.0).contains(&flat.0)
                && (0.0..=self.flat.1).contains(&flat.1))
            .then(|| h.apply(flat)),
            None => Some(flat),
        }
    }
}

fn resample(img: &RgbImage, map: &Mapping, scale: f64, fill: [f32; 3]) -> RgbImage {
    let (w, h) = ((map.out.0 * scale) as u32, (map.out.1 * scale) as u32);
    let (sw, sh) = (img.width() as usize, img.height() as usize);
    RgbImage::from_fn(w.max(1), h.max(1), |x, y| {
        let p = ((f64::from(x) + 0.5) / scale, (f64::from(y) + 0.5) / scale);
        let px = map
            .source_of(p)
            .and_then(|(sx, sy)| {
                sample(sx, sy, sw, sh, |xi, yi| {
                    img.get_pixel(xi as u32, yi as u32).0.map(f32::from)
                })
            })
            .unwrap_or(fill);
        Rgb(px.map(|v| v.round().clamp(0.0, 255.0) as u8))
    })
}

/// Average colour of the bright (paper) pixels, used to fill new margins.
fn paper_colour(img: &RgbImage) -> [f32; 3] {
    let lumas: Vec<f32> = img
        .pixels()
        .step_by(7)
        .map(|p| 0.299 * f32::from(p[0]) + 0.587 * f32::from(p[1]) + 0.114 * f32::from(p[2]))
        .collect();
    let (threshold, _, _) = otsu(&lumas);
    let (mut sum, mut n) = ([0f64; 3], 0f64);
    for (p, &l) in img.pixels().step_by(7).zip(&lumas) {
        if l > threshold {
            for c in 0..3 {
                sum[c] += f64::from(p[c]);
            }
            n += 1.0;
        }
    }
    if n == 0.0 {
        return [255.0; 3];
    }
    sum.map(|s| (s / n) as f32)
}

/// Flattens and straightens a photographed page; `None` leaves it unchanged.
pub(crate) fn correct(img: &RgbImage, mode: ScanMode) -> Option<Correction> {
    if mode == ScanMode::Off || img.width().max(img.height()) < MIN_EDGE {
        return None;
    }
    let (gray, scale) = work_copy(img);

    // Page warp on the work copy, kept only when the sheet carries text in
    // auto mode, so a plain bright rectangle is not mistaken for a page.
    let mut page = find_page(&gray, mode);
    let mut flat = gray.clone();
    if let Some(corners) = page {
        let size = page_size(&corners);
        let map = Mapping::new(Some((corners, size)), (gray.w as f64, gray.h as f64), 0.0)?;
        let (fw, fh) = (size.0.round() as usize, size.1.round() as usize);
        let px = (0..fw * fh)
            .map(|i| {
                let p = ((i % fw) as f64 + 0.5, (i / fw) as f64 + 0.5);
                map.source_of(p)
                    .and_then(|(sx, sy)| gray.bilinear(sx, sy))
                    .unwrap_or(255.0)
            })
            .collect();
        let warped = Gray { w: fw, h: fh, px };
        if mode == ScanMode::Auto && !ink_fraction_ok(ink_points(&warped).len(), &warped) {
            page = None;
        } else {
            flat = warped;
        }
    }

    let ink = ink_points(&flat);
    let paper_like = || {
        let bg = flat.blurred((flat.w.max(flat.h) / 40).max(3));
        bg.px.iter().filter(|&&v| v > 150.0).count() as f64 / bg.px.len() as f64 >= 0.6
    };
    let deskew = match mode {
        ScanMode::On => estimate_skew(&flat, &ink),
        _ if page.is_some() || paper_like() => estimate_skew(&flat, &ink),
        _ => None,
    }
    .filter(|a| a.abs() >= MIN_SKEW_DEGREES)
    .unwrap_or(0.0);

    if page.is_none() && deskew == 0.0 {
        return None;
    }
    let full = page.map(|q| q.map(|(x, y)| (x / scale, y / scale)));
    let map = Mapping::new(
        full.map(|q| (q, page_size(&q))),
        (f64::from(img.width()), f64::from(img.height())),
        deskew,
    )?;
    // Never upscale beyond the source's pixel count.
    let budget = f64::from(img.width()) * f64::from(img.height());
    let out_scale = (budget / (map.out.0 * map.out.1)).sqrt().min(1.0);
    let image = resample(img, &map, out_scale, paper_colour(img));
    let normalised =
        full.map(|q| q.map(|(x, y)| (x / f64::from(img.width()), y / f64::from(img.height()))));
    Some(Correction {
        image,
        page: normalised,
        deskew_degrees: deskew,
    })
}

const PHOTO_TYPES: &[&str] = &[
    "image/jpeg",
    "image/png",
    "image/webp",
    "image/bmp",
    "image/tiff",
    "image/heif",
    "image/heic",
    "image/avif",
    "image/x-canon-cr2",
];
const PHOTO_EXTENSIONS: &[&str] = &[
    "jpg", "jpeg", "png", "webp", "bmp", "tif", "tiff", "heic", "heif", "hif", "avif",
];

/// Still photos and scans; not video frames, rendered PDF or Office pages, or print jobs.
fn is_photo(source: &SourceRecord) -> bool {
    if let Some(mime) = source.detected_type.as_deref() {
        return PHOTO_TYPES.contains(&mime) || crate::importers::is_raw_extension(&source.path);
    }
    crate::importers::is_raw_extension(&source.path)
        || source
            .path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| PHOTO_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
}

pub struct ScanEnricher {
    mode: ScanMode,
}

impl ScanEnricher {
    pub fn new(mode: ScanMode) -> Self {
        Self { mode }
    }
}

impl Plugin for ScanEnricher {
    fn descriptor(&self) -> PluginDescriptor {
        PluginDescriptor {
            name: "scan".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            kind: "unit-enricher".into(),
            extensions: vec![],
            mime_types: vec![],
            priority: 100,
        }
    }
}

impl UnitEnricher for ScanEnricher {
    fn supports(&self, graph: &DocumentGraph, unit: &Unit) -> bool {
        self.mode != ScanMode::Off
            && unit.kind == UnitKind::Visual
            && unit.visual_path.is_some()
            && unit.time_range.is_none()
            && !unit.metadata.contains_key(SCAN_DESKEW_KEY)
            && unit
                .metadata
                .get(crate::importers::TEXT_LAYER_KEY)
                .map(String::as_str)
                != Some(crate::importers::TEXT_LAYER_NATIVE)
            && graph.unit_source(unit).is_some_and(|s| is_photo(&s))
    }

    fn enrich_unit(
        &self,
        ctx: &JobContext,
        _graph: &DocumentGraph,
        unit: &mut Unit,
    ) -> Result<Vec<String>> {
        let path = unit.visual_path.clone().expect("supports checked");
        let image = image::open(&path)
            .with_context(|| format!("decode {}", path.display()))?
            .to_rgb8();
        let Some(fix) = correct(&image, self.mode) else {
            return Ok(vec![]);
        };
        let out = ctx.workspace.join(format!("scan-{}.png", unit.id));
        fix.image.save(&out)?;
        unit.visual_path = Some(out);
        if let Some(page) = fix.page {
            let corners: Vec<String> = page.iter().map(|(x, y)| format!("{x:.4},{y:.4}")).collect();
            unit.metadata
                .insert(SCAN_PAGE_KEY.into(), corners.join(" "));
        }
        unit.metadata
            .insert(SCAN_DESKEW_KEY.into(), format!("{:.1}", fix.deskew_degrees));
        Ok(vec![])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anytopdf_core::Uuid;

    const BACKGROUND: u8 = 45;
    const PAPER: u8 = 235;
    const INK: u8 = 20;

    /// Paints a sheet with ruled "text lines" at `corners` (TL TR BR BL) over a
    /// dark table; the lines follow the sheet's perspective.
    fn photographed_page(w: u32, h: u32, corners: [Point; 4]) -> RgbImage {
        let size = (1000.0, 1400.0);
        let rect = [(0.0, 0.0), (size.0, 0.0), (size.0, size.1), (0.0, size.1)];
        let to_page = Homography::between(corners, rect).unwrap();
        RgbImage::from_fn(w, h, |x, y| {
            let (u, v) = to_page.apply((f64::from(x) + 0.5, f64::from(y) + 0.5));
            let value = if !(0.0..size.0).contains(&u) || !(0.0..size.1).contains(&v) {
                BACKGROUND
            } else if (100.0..900.0).contains(&u)
                && (150.0..1250.0).contains(&v)
                && (v as u32 % 60) < 14
            {
                INK
            } else {
                PAPER
            };
            Rgb([value; 3])
        })
    }

    /// A page-filling white scan with text lines sloping down by `degrees`.
    fn skewed_scan(w: u32, h: u32, degrees: f64) -> RgbImage {
        let (sin, cos) = degrees.to_radians().sin_cos();
        let (cx, cy) = (f64::from(w) / 2.0, f64::from(h) / 2.0);
        RgbImage::from_fn(w, h, |x, y| {
            let (dx, dy) = (f64::from(x) - cx, f64::from(y) - cy);
            // Coordinates in the unrotated page.
            let u = dx * cos + dy * sin;
            let v = -dx * sin + dy * cos;
            let ink = u.abs() < f64::from(w) * 0.35
                && v.abs() < f64::from(h) * 0.38
                && (v + 1000.0) as u32 % 40 < 10;
            Rgb([if ink { INK } else { PAPER }; 3])
        })
    }

    fn close(a: Point, b: Point, tolerance: f64) -> bool {
        distance(a, b) <= tolerance
    }

    #[test]
    fn homography_maps_each_corner_onto_its_target() {
        let from = [(0.0, 0.0), (100.0, 0.0), (100.0, 50.0), (0.0, 50.0)];
        let to = [(10.0, 5.0), (90.0, 12.0), (95.0, 70.0), (3.0, 60.0)];
        let h = Homography::between(from, to).unwrap();
        for (f, t) in from.into_iter().zip(to) {
            assert!(close(h.apply(f), t, 1e-6));
        }
    }

    #[test]
    fn finds_the_corners_of_a_photographed_sheet() {
        let corners = [
            (150.0, 70.0),
            (610.0, 110.0),
            (640.0, 700.0),
            (110.0, 660.0),
        ];
        let img = photographed_page(800, 800, corners);
        let (gray, scale) = work_copy(&img);
        assert_eq!(scale, 1.0);
        let found = find_page(&gray, ScanMode::Auto).expect("sheet found");
        for (f, c) in found.into_iter().zip(corners) {
            assert!(close(f, c, 3.0), "{found:?} vs {corners:?}");
        }
    }

    #[test]
    fn auto_mode_flattens_a_photographed_page() {
        // Larger than the work copy, so corners are scaled back up.
        let corners = [
            (300.0, 140.0),
            (1220.0, 220.0),
            (1280.0, 1400.0),
            (220.0, 1320.0),
        ];
        let img = photographed_page(1600, 1600, corners);
        let fix = correct(&img, ScanMode::Auto).expect("page corrected");
        let page = fix.page.unwrap();
        for (p, c) in page.into_iter().zip(corners) {
            assert!(close((p.0 * 1600.0, p.1 * 1600.0), c, 6.0), "{page:?}");
        }
        assert_eq!(fix.deskew_degrees, 0.0);
        // The flattened sheet is portrait and its corners are paper, not table.
        let (w, h) = fix.image.dimensions();
        assert!(h > w && w > 800, "{w}x{h}");
        for (x, y) in [(4, 4), (w - 5, 4), (w - 5, h - 5), (4, h - 5)] {
            assert!(fix.image.get_pixel(x, y)[0] > 180, "corner {x},{y}");
        }
    }

    #[test]
    fn estimates_and_removes_text_skew() {
        let img = skewed_scan(900, 700, 4.0);
        let (gray, _) = work_copy(&img);
        let angle = estimate_skew(&gray, &ink_points(&gray)).unwrap();
        assert!((angle - 4.0).abs() <= 0.3, "{angle}");

        let fix = correct(&img, ScanMode::Auto).expect("deskewed");
        assert!(fix.page.is_none());
        assert!((fix.deskew_degrees - 4.0).abs() <= 0.3);
        let (straight, _) = work_copy(&fix.image);
        let residual = estimate_skew(&straight, &ink_points(&straight)).unwrap_or(0.0);
        assert!(residual.abs() <= 0.3, "{residual}");
    }

    #[test]
    fn auto_mode_leaves_straight_scans_and_ordinary_photos_alone() {
        assert!(correct(&skewed_scan(900, 700, 0.0), ScanMode::Auto).is_none());
        // Bright sky over dark ground: a bright band hugging the frame.
        let landscape = RgbImage::from_fn(900, 600, |_, y| {
            Rgb(if y < 250 {
                [200, 220, 250]
            } else {
                [40, 70, 30]
            })
        });
        assert!(correct(&landscape, ScanMode::Auto).is_none());
        // A blank bright card on a table has no text, so it is not a page.
        let card = photographed_page(
            800,
            800,
            [
                (200.0, 200.0),
                (600.0, 200.0),
                (600.0, 600.0),
                (200.0, 600.0),
            ],
        );
        let blank = RgbImage::from_fn(800, 800, |x, y| {
            let p = card.get_pixel(x, y);
            if p[0] == INK { Rgb([PAPER; 3]) } else { *p }
        });
        assert!(correct(&blank, ScanMode::Auto).is_none());
        assert!(correct(&blank, ScanMode::Off).is_none());
    }

    #[test]
    fn enricher_rewrites_photo_units_before_ocr_and_records_provenance() {
        let dir = tempfile::tempdir().unwrap();
        let photo = dir.path().join("receipt.jpg");
        skewed_scan(900, 700, -3.0)
            .save(dir.path().join("page.png"))
            .unwrap();
        let mut source = SourceRecord::new(photo);
        source.detected_type = Some("image/jpeg".into());
        let mut unit = Unit::visual(source.id, dir.path().join("page.png"));
        let mut frame = unit.clone();
        frame.id = Uuid::new_v4();
        frame.time_range = Some(anytopdf_core::TimeRange::point(1.0));
        let mut video = SourceRecord::new(dir.path().join("clip.mp4"));
        video.detected_type = Some("video/mp4".into());
        let mut video_frame = Unit::visual(video.id, dir.path().join("page.png"));
        video_frame.id = Uuid::new_v4();
        let graph = DocumentGraph {
            sources: vec![source, video],
            units: vec![unit.clone(), frame.clone(), video_frame.clone()],
            ..Default::default()
        };

        let enricher = ScanEnricher::new(ScanMode::Auto);
        assert!(enricher.supports(&graph, &unit));
        assert!(
            !enricher.supports(&graph, &frame),
            "timed frames are skipped"
        );
        assert!(
            !enricher.supports(&graph, &video_frame),
            "video sources are skipped"
        );
        assert!(!ScanEnricher::new(ScanMode::Off).supports(&graph, &unit));

        let ctx = JobContext {
            workspace: dir.path().into(),
            quiet: true,
        };
        enricher.enrich_unit(&ctx, &graph, &mut unit).unwrap();
        assert_eq!(
            unit.visual_path.as_deref(),
            Some(dir.path().join(format!("scan-{}.png", unit.id)).as_path())
        );
        let degrees: f64 = unit.metadata[SCAN_DESKEW_KEY].parse().unwrap();
        assert!((degrees + 3.0).abs() <= 0.3, "{degrees}");
        assert!(!unit.metadata.contains_key(SCAN_PAGE_KEY));
        assert!(
            !enricher.supports(&graph, &unit),
            "a unit is corrected once"
        );

        let mut registry = anytopdf_core::Registry::default();
        crate::register_builtins(&mut registry, crate::BuiltinOptions::default());
        let names: Vec<String> = registry
            .unit_enrichers()
            .iter()
            .map(|e| e.descriptor().name)
            .collect();
        let pos = |n: &str| names.iter().position(|x| x == n).unwrap();
        assert!(pos("scan") < pos("ocr-auto"), "{names:?}");
    }

    #[test]
    fn scan_modes_parse() {
        assert_eq!("ON".parse::<ScanMode>(), Ok(ScanMode::On));
        assert_eq!("none".parse::<ScanMode>(), Ok(ScanMode::Off));
        assert_eq!(ScanMode::default().to_string(), "auto");
        assert!("maybe".parse::<ScanMode>().is_err());
    }
}
