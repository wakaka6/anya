//! Text in the system's sans-serif font, found through fontconfig and
//! rasterised by ab_glyph. A character the first font lacks is looked up
//! again by fontconfig with that character required, so Chinese, Japanese
//! and the rest draw in whatever font the system has for them.

use std::collections::HashMap;

use ab_glyph::{Font, FontVec, GlyphId, PxScale, ScaleFont, point};
use fontconfig::{CharSet, Fontconfig, Pattern};

use super::layout::Measure;
use super::model::Rgba;
use super::paint::Canvas;
use crate::OverlayError;

struct Face {
    path: String,
    index: u32,
    font: FontVec,
}

/// The fonts loaded so far: the sans-serif first, then fallbacks as text
/// needs them.
pub(super) struct Fonts {
    fc: Fontconfig,
    faces: Vec<Face>,
    /// Which face draws a character, once looked up.
    picked: HashMap<char, usize>,
}

impl Fonts {
    /// The system's sans-serif font.
    pub(super) fn system() -> Result<Fonts, OverlayError> {
        let fc = Fontconfig::new()
            .ok_or_else(|| OverlayError::Backend("fontconfig could not start".into()))?;
        let (path, index) = matching(&fc, None)
            .ok_or_else(|| OverlayError::Backend("fontconfig found no sans-serif font".into()))?;
        let font = load(&path, index)
            .ok_or_else(|| OverlayError::Backend(format!("cannot load font {path}")))?;
        Ok(Fonts {
            fc,
            faces: vec![Face { path, index, font }],
            picked: HashMap::new(),
        })
    }

    /// The face that draws `c`; the first face (its missing-glyph box) when
    /// none does.
    fn face_for(&mut self, c: char) -> usize {
        if let Some(&i) = self.picked.get(&c) {
            return i;
        }
        let i = self.find(c);
        self.picked.insert(c, i);
        i
    }

    fn find(&mut self, c: char) -> usize {
        if c.is_whitespace() || c.is_control() {
            return 0;
        }
        if let Some(i) = self.faces.iter().position(|f| f.font.glyph_id(c).0 != 0) {
            return i;
        }
        let Some((path, index)) = matching(&self.fc, Some(c)) else {
            return 0;
        };
        if self
            .faces
            .iter()
            .any(|f| f.path == path && f.index == index)
        {
            return 0;
        }
        match load(&path, index) {
            Some(font) if font.glyph_id(c).0 != 0 => {
                tracing::debug!(%path, %c, "overlay font fallback");
                self.faces.push(Face { path, index, font });
                self.faces.len() - 1
            }
            _ => 0,
        }
    }

    /// The first face's ascent and descent (negative) at `px` to the em.
    pub(super) fn metrics(&self, px: f32) -> (f32, f32) {
        let font = &self.faces[0].font;
        let scaled = font.as_scaled(em(font, px));
        (scaled.ascent(), scaled.descent())
    }

    /// Lays `text` out from `x` on `baseline` and calls `glyph` for each
    /// character with its face, glyph and pen position.
    fn run(
        &mut self,
        text: &str,
        x: f32,
        px: f32,
        mut glyph: impl FnMut(&FontVec, GlyphId, PxScale, f32),
    ) -> f32 {
        let mut caret = x;
        let mut prev: Option<(usize, GlyphId)> = None;
        for c in text.chars() {
            let i = self.face_for(c);
            let font = &self.faces[i].font;
            let scale = em(font, px);
            let scaled = font.as_scaled(scale);
            let id = font.glyph_id(c);
            if let Some((pi, pid)) = prev
                && pi == i
            {
                caret += scaled.kern(pid, id);
            }
            glyph(font, id, scale, caret);
            caret += scaled.h_advance(id);
            prev = Some((i, id));
        }
        caret - x
    }

    /// Draws `text` in `color` from `x` on `baseline`.
    pub(super) fn draw(
        &mut self,
        canvas: &mut Canvas<'_>,
        text: &str,
        x: f32,
        baseline: f32,
        px: f32,
        color: Rgba,
    ) {
        self.run(text, x, px, |font, id, scale, caret| {
            let glyph = id.with_scale_and_position(scale, point(caret, baseline));
            if let Some(outline) = font.outline_glyph(glyph) {
                let b = outline.px_bounds();
                let (left, top) = (b.min.x as i32, b.min.y as i32);
                outline.draw(|gx, gy, c| canvas.blend(left + gx as i32, top + gy as i32, color, c));
            }
        });
    }
}

impl Measure for Fonts {
    fn width(&mut self, text: &str, px: f32) -> f32 {
        self.run(text, 0.0, px, |_, _, _, _| {})
    }
}

/// The ab_glyph scale that sets `font` at `px` pixels to the em: ab_glyph
/// scales by ascent minus descent, which differs between fonts.
fn em(font: &FontVec, px: f32) -> PxScale {
    let upem = font.units_per_em().unwrap_or(1000.0);
    PxScale::from(px * font.height_unscaled() / upem)
}

/// The file and face fontconfig picks for sans-serif, holding `c` if given.
fn matching(fc: &Fontconfig, c: Option<char>) -> Option<(String, u32)> {
    let mut pattern = Pattern::new(fc).ok()?;
    pattern.add_string(c"family", c"sans-serif").ok()?;
    if let Some(c) = c {
        let mut set = CharSet::new(fc).ok()?;
        set.add_char(c).ok()?;
        pattern.add_charset(set).ok()?;
    }
    let found = pattern.font_match().ok()?;
    let path = found.filename().ok()?.to_owned();
    // The upper bits name a variable font's instance, not the face.
    let index = (found.face_index().unwrap_or(0).max(0) as u32) & 0xffff;
    Some((path, index))
}

fn load(path: &str, index: u32) -> Option<FontVec> {
    let data = std::fs::read(path).ok()?;
    FontVec::try_from_vec_and_index(data, index).ok()
}

/// The file of the system's sans-serif font, for the doctor.
pub(super) fn sans_serif() -> Option<String> {
    matching(&Fontconfig::new()?, None).map(|(path, _)| path)
}

/// The file of a font that has Chinese characters, for the doctor. Asks
/// fontconfig's index without opening any font file.
pub(super) fn chinese() -> Option<String> {
    let fc = Fontconfig::new()?;
    let mut pattern = Pattern::new(&fc).ok()?;
    let mut set = CharSet::new(&fc).ok()?;
    for c in "中文".chars() {
        set.add_char(c).ok()?;
    }
    pattern.add_charset(set).ok()?;
    let fonts = fontconfig::list_fonts(&pattern, None).ok()?;
    let first = fonts.iter().next()?;
    first.filename().ok().map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    const WHITE: Rgba = Rgba {
        r: 255,
        g: 255,
        b: 255,
        a: 255,
    };

    fn fonts() -> Fonts {
        // The Linux build image ships DejaVu Sans; every desktop has a sans-serif.
        Fonts::system().expect("a sans-serif font")
    }

    fn ink(fonts: &mut Fonts, text: &str) -> usize {
        let mut data = vec![0u8; 64 * 40 * 4];
        let mut canvas = Canvas::new(&mut data, 64, 40);
        fonts.draw(&mut canvas, text, 4.0, 30.0, 24.0, WHITE);
        data.chunks_exact(4).filter(|px| px[3] > 0).count()
    }

    #[test]
    fn the_system_font_measures_text_by_its_size() {
        let mut f = fonts();
        let a = f.width("A", 15.0);
        assert!(a > 4.0 && a < 15.0, "{a}");
        assert!(f.width("AA", 15.0) > a * 1.8);
        assert!((f.width("A", 30.0) - 2.0 * a).abs() < 0.5);
        let (ascent, descent) = f.metrics(15.0);
        assert!(ascent > 8.0 && descent < 0.0, "{ascent} {descent}");
    }

    #[test]
    fn every_letter_of_a_label_has_ink() {
        let mut f = fonts();
        for c in "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789…".chars() {
            assert!(ink(&mut f, &c.to_string()) > 4, "{c:?} drew nothing");
        }
        assert_eq!(ink(&mut f, " "), 0);
    }

    // Needs a CJK font installed (e.g. fonts-noto-cjk), which the build image
    // lacks: cargo test -p anya-overlay -- --ignored
    #[test]
    #[ignore = "needs a CJK font"]
    fn chinese_falls_back_to_a_font_that_has_it() {
        let mut f = fonts();
        for c in "正在聆听识别已粘贴".chars() {
            let i = f.face_for(c);
            assert_ne!(f.faces[i].font.glyph_id(c).0, 0, "{c} has no font");
            assert!(ink(&mut f, &c.to_string()) > 10, "{c} drew nothing");
        }
    }
}
