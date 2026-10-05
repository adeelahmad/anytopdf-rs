//! Fonts and text drawing for the PDF/A renderer: per-character font fallback,
//! bidirectional reordering and shaping (rustybuzz, through krilla).
use crate::fonts::{BUNDLED_FONT, subset_document_font};
use anyhow::{Context, Result, anyhow};
use anytopdf_core::DocumentGraph;
use krilla::geom::Point;
use krilla::surface::Surface;
use krilla::text::{Font, TextDirection};
use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use unicode_bidi::{BidiInfo, Level};

/// One embedded font with the advance (in ems) of every character it maps.
struct FaceInfo {
    font: Font,
    advances: BTreeMap<char, f32>,
}

/// The primary font plus the fallbacks that cover characters it lacks.
pub(crate) struct FontSet {
    faces: Vec<FaceInfo>,
}

/// Characters the document draws: visible text, annotations and generated labels.
fn document_chars(graph: &DocumentGraph) -> BTreeSet<char> {
    let mut chars: BTreeSet<char> = (' '..='~').collect();
    chars.insert('–');
    for (k, v) in &graph.metadata {
        chars.extend(k.chars().chain(v.chars()));
    }
    for source in &graph.sources {
        chars.extend(source.path.to_string_lossy().chars());
    }
    for unit in &graph.units {
        if let Some(text) = &unit.visible_text {
            chars.extend(text.chars());
        }
        for a in &unit.annotations {
            chars.extend(a.text.chars().chain(a.provider.chars()));
        }
    }
    chars.retain(|c| !c.is_control());
    chars
}

impl FontSet {
    /// Load `primary` (the bundled DejaVu Sans when `None`) and then each fallback
    /// that maps a still-missing character. Fonts whose licence forbids embedding are
    /// skipped with a warning.
    pub(crate) fn load(
        primary: Option<&Path>,
        fallbacks: &[PathBuf],
        graph: &DocumentGraph,
        warnings: &mut Vec<String>,
    ) -> Result<Self> {
        let mut missing = document_chars(graph);
        let mut faces = Vec::new();
        let bundled = Path::new("bundled DejaVu Sans");
        let paths = std::iter::once(primary).chain(fallbacks.iter().map(|p| Some(p.as_path())));
        for (index, path) in paths.enumerate() {
            if index > 0 && missing.iter().all(|c| c.is_whitespace()) {
                break;
            }
            let bytes = match path.map(std::fs::read) {
                None => Cow::Borrowed(BUNDLED_FONT),
                Some(Ok(bytes)) => Cow::Owned(bytes),
                Some(Err(e)) if index == 0 => {
                    let path = path.unwrap_or(bundled);
                    return Err(e).with_context(|| format!("read font {}", path.display()));
                }
                Some(Err(_)) => continue,
            };
            let path = path.unwrap_or(bundled);
            let Ok(face) = ttf_parser::Face::parse(&bytes, 0) else {
                if index == 0 {
                    return Err(anyhow!("parse font {}", path.display()));
                }
                continue;
            };
            if face.permissions() == Some(ttf_parser::Permissions::Restricted) {
                warnings.push(format!(
                    "Font {} does not permit embedding; PDF/A output skips it.",
                    path.display()
                ));
                continue;
            }
            if index > 0
                && !missing
                    .iter()
                    .any(|c| face.glyph_index(*c).is_some_and(|g| g.0 != 0))
            {
                continue;
            }
            let info = FaceInfo::load(&bytes, graph)
                .with_context(|| format!("load font {}", path.display()))?;
            missing.retain(|c| !info.advances.contains_key(c));
            faces.push(info);
        }
        if faces.is_empty() {
            return Err(anyhow!("no usable font for PDF/A output"));
        }
        Ok(Self { faces })
    }

    fn face_for(&self, ch: char) -> Option<usize> {
        self.faces.iter().position(|f| f.advances.contains_key(&ch))
    }

    pub(crate) fn has(&self, ch: char) -> bool {
        self.face_for(ch).is_some()
    }

    pub(crate) fn measure(&self, ch: char) -> f32 {
        self.face_for(ch)
            .and_then(|i| self.faces[i].advances.get(&ch).copied())
            .unwrap_or(0.5)
    }

    /// Draw `text` with its baseline at `origin` (points, y down).
    ///
    /// The line is split into bidi runs, laid out in visual order; each run is split
    /// into same-font segments that krilla shapes in the run's direction. Characters no
    /// font maps are skipped (PDF/A forbids `.notdef`) but still advance the pen.
    pub(crate) fn draw(&self, surface: &mut Surface, origin: Point, size: f32, text: &str) {
        let text: String = text.chars().filter(|c| !c.is_control()).collect();
        if text.is_empty() {
            return;
        }
        let mut x = origin.x;
        for (run, rtl) in visual_runs(&text) {
            let mut segments = self.segments(&run);
            if rtl {
                segments.reverse();
            }
            let direction = if rtl {
                TextDirection::RightToLeft
            } else {
                TextDirection::LeftToRight
            };
            for (face, segment, width) in segments {
                if let Some(face) = face {
                    surface.draw_text(
                        Point::from_xy(x, origin.y),
                        self.faces[face].font.clone(),
                        size,
                        &segment,
                        false,
                        direction,
                    );
                }
                x += width * size;
            }
        }
    }

    /// Split a run into maximal same-font segments: (font, text, width in ems).
    fn segments(&self, run: &str) -> Vec<(Option<usize>, String, f32)> {
        let mut out: Vec<(Option<usize>, String, f32)> = Vec::new();
        for ch in run.chars() {
            let face = self.face_for(ch);
            let advance = self.measure(ch);
            match out.last_mut() {
                Some((f, s, w)) if *f == face => {
                    s.push(ch);
                    *w += advance;
                }
                _ => out.push((face, ch.to_string(), advance)),
            }
        }
        out
    }
}

/// The bidi runs of one line in visual (left-to-right) order, each in logical order
/// with whether it is right-to-left.
fn visual_runs(text: &str) -> Vec<(String, bool)> {
    let bidi = BidiInfo::new(text, None);
    let mut out = Vec::new();
    for para in &bidi.paragraphs {
        let (levels, runs) = bidi.visual_runs(para, para.range.clone());
        for run in runs {
            let rtl = levels[run.start].is_rtl();
            out.push((text[run].to_string(), rtl));
        }
    }
    out
}

/// Whether `text` holds right-to-left characters, so extraction needs `ActualText`.
pub(crate) fn has_rtl(text: &str) -> bool {
    let bidi = BidiInfo::new(text, None);
    bidi.levels.iter().any(|l: &Level| l.is_rtl())
}

impl FaceInfo {
    fn load(bytes: &[u8], graph: &DocumentGraph) -> Result<Self> {
        let subset = subset_document_font(bytes, graph)?;
        let face = ttf_parser::Face::parse(&subset, 0).context("parse subset font")?;
        let em = f32::from(face.units_per_em());
        let mut advances = BTreeMap::new();
        for table in face.tables().cmap.iter().flat_map(|c| c.subtables) {
            if !table.is_unicode() {
                continue;
            }
            table.codepoints(|cp| {
                if let Some(ch) = char::from_u32(cp)
                    && let Some(glyph) = face.glyph_index(ch).filter(|g| g.0 != 0)
                {
                    let advance = face.glyph_hor_advance(glyph).unwrap_or(0);
                    advances.insert(ch, f32::from(advance) / em);
                }
            });
        }
        let font = Font::new(subset.into(), 0).ok_or_else(|| anyhow!("could not load font"))?;
        Ok(Self { font, advances })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn visual_runs_order_mixed_direction_lines() {
        assert_eq!(
            visual_runs("abc אבג def"),
            [
                ("abc ".to_string(), false),
                ("אבג".to_string(), true),
                (" def".to_string(), false)
            ]
        );
        // A right-to-left paragraph puts its first word on the right and keeps digits LTR.
        assert_eq!(
            visual_runs("שלום 123"),
            [("123".to_string(), false), ("שלום ".to_string(), true)]
        );
        assert!(has_rtl("abc אבג") && has_rtl("مرحبا") && !has_rtl("abc 漢字"));
    }
}
