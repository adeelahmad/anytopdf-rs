use std::{fs, path::Path};

/// A width x height 8-bit mid-gray PNG.
pub fn write_png(path: &Path, width: u32, height: u32) {
    let mut raw = Vec::new();
    for _ in 0..height {
        raw.push(0);
        raw.extend(std::iter::repeat_n(128u8, width as usize));
    }
    fs::write(path, crate::png::encode_png(width, height, 0, &raw)).unwrap();
}
