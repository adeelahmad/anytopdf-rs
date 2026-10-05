use std::{fs, path::Path};

/// 4x3 8-bit RGB PNG.
pub fn write_rgb_png(path: &Path) {
    let raw: Vec<u8> = (0..3u8)
        .flat_map(|row| {
            std::iter::once(0u8).chain((0..4u8).flat_map(move |col| [col * 60, row * 80, 200]))
        })
        .collect();
    fs::write(path, crate::png::encode_png(4, 3, 2, &raw)).unwrap();
}
