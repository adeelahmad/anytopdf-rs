use std::{fs, path::Path};

/// 4x3 8-bit grayscale PNG.
pub fn write_gray_ramp_png(path: &Path) {
    let raw: Vec<u8> = (0..3).flat_map(|_| [0u8, 10, 100, 200, 250]).collect();
    fs::write(path, crate::png::encode_png(4, 3, 0, &raw)).unwrap();
}
