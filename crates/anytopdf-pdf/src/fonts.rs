use anyhow::Result;
use anytopdf_core::*;
use std::path::PathBuf;

pub(crate) fn subset_document_font(bytes: &[u8], graph: &DocumentGraph) -> Result<Vec<u8>> {
    use allsorts::{binary::read::ReadScope, font_data::FontData, subset};
    use std::collections::BTreeSet;

    // ASCII also covers generated labels, annotation kinds, timestamps and numbers.
    let mut characters: BTreeSet<char> = (' '..='~').collect();
    characters.insert('–');
    for (key, value) in &graph.metadata {
        characters.extend(key.chars());
        characters.extend(value.chars());
    }
    for source in &graph.sources {
        characters.extend(source.path.to_string_lossy().chars());
        for (key, value) in &source.metadata {
            characters.extend(key.chars());
            characters.extend(value.chars());
        }
    }
    for unit in &graph.units {
        if let Some(text) = &unit.visible_text {
            characters.extend(text.chars());
        }
        for annotation in &unit.annotations {
            characters.extend(annotation.text.chars());
            characters.extend(annotation.provider.chars());
        }
    }
    let face = ttf_parser::Face::parse(bytes, 0)?;
    let mut glyphs = BTreeSet::from([0]);
    glyphs.extend(
        characters
            .into_iter()
            .filter_map(|ch| face.glyph_index(ch).map(|id| id.0)),
    );
    let font = ReadScope::new(bytes)
        .read::<FontData<'_>>()
        .map_err(|e| anyhow::anyhow!("read font for subsetting: {e:?}"))?;
    let provider = font
        .table_provider(0)
        .map_err(|e| anyhow::anyhow!("read font tables: {e:?}"))?;
    subset::subset(
        &provider,
        &glyphs.into_iter().collect::<Vec<_>>(),
        &subset::SubsetProfile::Pdf,
        subset::CmapTarget::Unicode,
    )
    .map_err(|e| anyhow::anyhow!("subset document font: {e:?}"))
}

pub(crate) fn find_system_font() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("ANYTOPDF_FONT") {
        return Some(path.into());
    }
    [
        "/System/Library/Fonts/Supplemental/Arial Unicode.ttf",
        "/System/Library/Fonts/Supplemental/Arial.ttf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
        "/usr/share/fonts/truetype/noto/NotoSans-Regular.ttf",
        r"C:\Windows\Fonts\segoeui.ttf",
        r"C:\Windows\Fonts\arial.ttf",
    ]
    .into_iter()
    .map(PathBuf::from)
    .find(|p| p.exists())
}

#[cfg(test)]
mod tests {
    use super::*;
    use printpdf::*;

    #[test]
    fn font_subset_retains_visible_and_metadata_characters() {
        let bytes = BuiltinFont::Helvetica.get_subset_font().bytes;
        let mut source = SourceRecord::new(PathBuf::from("résumé.txt"));
        source.metadata.insert("label".into(), "café".into());
        let graph = DocumentGraph {
            units: vec![Unit::text(source.id, "Résumé éàç".into())],
            sources: vec![source],
            ..Default::default()
        };
        let subset = subset_document_font(&bytes, &graph).unwrap();
        let original_face = ttf_parser::Face::parse(&bytes, 0).unwrap();
        let subset_face = ttf_parser::Face::parse(&subset, 0).unwrap();
        // The library's compact built-in fixture maps spaces to glyph zero.
        for ch in "Résuméeàçcafé.txtlabelSOURCE".chars() {
            assert!(
                original_face.glyph_index(ch).is_some(),
                "fixture lacks {ch}"
            );
            assert!(subset_face.glyph_index(ch).is_some(), "subset lost {ch}");
        }
        assert!(subset_face.number_of_glyphs() < original_face.number_of_glyphs());
    }
}
