//! Anya's icons, rendered from `icons/*.svg` (rsvg-convert -w 64 / -w 256).

/// The macOS menu bar icon: black on transparent, tinted by the system.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) const TRAY_TEMPLATE: &[u8] = include_bytes!("../../icons/tray-template.png");
/// The coloured tray icon for Linux panels.
#[cfg_attr(target_os = "macos", allow(dead_code))]
pub(crate) const TRAY: &[u8] = include_bytes!("../../icons/tray.png");
/// The window icon.
#[cfg_attr(target_os = "macos", allow(dead_code))]
pub(crate) const APP: &[u8] = include_bytes!("../../icons/app.png");

/// An icon decoded to 8-bit RGBA.
pub(crate) struct Rgba {
    pub(crate) pixels: Vec<u8>,
    pub(crate) width: u32,
    pub(crate) height: u32,
}

/// Decodes one of the PNGs above.
pub(crate) fn decode(png_bytes: &[u8]) -> Result<Rgba, String> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(png_bytes));
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder.read_info().map_err(|e| e.to_string())?;
    let size = reader
        .output_buffer_size()
        .ok_or_else(|| "icon too large".to_owned())?;
    let mut pixels = vec![0; size];
    let frame = reader.next_frame(&mut pixels).map_err(|e| e.to_string())?;
    if frame.color_type != png::ColorType::Rgba || frame.bit_depth != png::BitDepth::Eight {
        return Err(format!(
            "icon is {:?} {:?}, not 8-bit RGBA",
            frame.color_type, frame.bit_depth
        ));
    }
    pixels.truncate(frame.buffer_size());
    Ok(Rgba {
        pixels,
        width: frame.width,
        height: frame.height,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_icon_decodes_at_its_size() {
        for (bytes, side) in [(TRAY_TEMPLATE, 64), (TRAY, 64), (APP, 256)] {
            let icon = decode(bytes);
            assert!(icon.is_ok(), "{:?}", icon.err());
            let Ok(icon) = icon else { return };
            assert_eq!((icon.width, icon.height), (side, side));
            assert_eq!(icon.pixels.len(), (side * side * 4) as usize);
        }
    }

    #[test]
    fn the_template_is_black_on_transparent() {
        let Ok(icon) = decode(TRAY_TEMPLATE) else {
            panic!("template icon does not decode");
        };
        let mut drawn = 0;
        for px in icon.pixels.chunks_exact(4) {
            if px[3] > 0 {
                drawn += 1;
                assert_eq!(&px[..3], &[0, 0, 0]);
            }
        }
        assert!(drawn > 200, "the template draws {drawn} pixels");
        assert!(icon.pixels.chunks_exact(4).any(|px| px[3] == 0));
    }
}
