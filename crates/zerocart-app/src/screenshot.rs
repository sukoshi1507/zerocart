// SPDX-License-Identifier: MIT
// Copied from ZeroTag (MIT): PNG writer used by --png.

use std::fs::File;
use std::io::BufWriter;
use std::path::Path;

pub fn write_png(path: &Path, rgba: &[u8], width: u32, height: u32) -> Result<(), String> {
    if rgba.len() != width as usize * height as usize * 4 {
        return Err("Invalid snapshot".into());
    }
    let file = File::create(path).map_err(|_| "Unable to open PNG file".to_string())?;
    let mut enc = png::Encoder::new(BufWriter::new(file), width, height);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let mut writer = enc.write_header().map_err(|_| "Unable to initialize PNG encoder".to_string())?;
    writer.write_image_data(rgba).map_err(|_| "PNG encoding failed".to_string())
}
