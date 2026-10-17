//! Multi-scale content-aware fill shared with the spot-healing tool.
use anyhow::Result;

pub(super) fn try_content_fill(pixels: &mut [u8], mask: &[u8], w: usize, h: usize) -> Result<bool> {
    crate::patch_match::fill(pixels, mask, w, h, 1)
}

/// Premultiplied RGBA8. Returns false when no complete opaque donor patch exists.
pub fn content_fill(pixels: &mut [u8], mask: &[u8], w: usize, h: usize) -> bool {
    match try_content_fill(pixels, mask, w, h) {
        Ok(filled) => filled,
        Err(error) => {
            log::error!("内容识别填充失败：{error:#}");
            false
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fills_even_when_the_hole_only_touches_transparency() {
        let mut p = vec![0; 15 * 15 * 4];
        for y in 0..7 {
            for x in 0..7 {
                p[(y * 15 + x) * 4..(y * 15 + x) * 4 + 4].copy_from_slice(&[80, 60, 40, 255]);
            }
        }
        let mut mask = vec![0; 225];
        mask[12 * 15 + 12] = 255;
        assert!(content_fill(&mut p, &mask, 15, 15));
        assert_eq!(
            &p[(12 * 15 + 12) * 4..(12 * 15 + 12) * 4 + 4],
            &[80, 60, 40, 255]
        );
        assert_eq!(&p[(8 * 15 + 8) * 4..(8 * 15 + 8) * 4 + 4], &[0; 4]);
    }
    #[test]
    fn no_opaque_source_reports_failure() {
        assert!(!content_fill(&mut [0; 25 * 4], &[255; 25], 5, 5));
    }
}
