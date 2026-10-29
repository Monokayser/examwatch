//! Screen capture and H.264 encoding for ExamWatch (Windows).
//!
//! * [`capture`]: DXGI Desktop Duplication + D3D11 video-processor scaling to NV12,
//!   all on the GPU that drives the display.
//! * [`encoder`]: Media Foundation H.264. Hardware MFT on that same GPU (NVENC on
//!   the lab PCs, zero copy) or Windows' built-in software MFT (GT 730 labs).
//! * [`synth`]: synthetic frames for the load simulator and tests.

pub mod nal;
pub mod synth;

#[cfg(windows)]
pub mod capture;
#[cfg(windows)]
pub mod encoder;

/// Rounds down to an even number (NV12 needs even dimensions).
pub fn even(v: u32) -> u32 {
    v & !1
}

/// Fits `src` into a box of height `max_h` (and width `max_w`), keeping the
/// aspect ratio, never upscaling, even dimensions. Used for the 720p REC rule.
pub fn fit(src: (u32, u32), max_w: u32, max_h: u32) -> (u32, u32) {
    let (w, h) = src;
    if w == 0 || h == 0 {
        return (even(max_w.max(2)), even(max_h.max(2)));
    }
    let s = (max_w as f64 / w as f64).min(max_h as f64 / h as f64).min(1.0);
    (even(((w as f64) * s).round() as u32).max(2), even(((h as f64) * s).round() as u32).max(2))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rec_resolution_rule() {
        // §2.9 of the design: 720 px tall, keep aspect, never upscale.
        assert_eq!(fit((1920, 1080), 4096, 720), (1280, 720));
        assert_eq!(fit((1366, 768), 4096, 720), (1280, 720));
        assert_eq!(fit((1920, 1200), 4096, 720), (1152, 720));
        assert_eq!(fit((1600, 900), 4096, 720), (1280, 720));
        assert_eq!(fit((1280, 720), 4096, 720), (1280, 720));
        assert_eq!(fit((1024, 768), 4096, 720), (960, 720));
        assert_eq!(fit((800, 600), 4096, 720), (800, 600));
        assert_eq!(fit((3840, 2160), 4096, 720), (1280, 720));
        // thumbnails
        assert_eq!(fit((1920, 1080), 480, 270), (480, 270));
    }
}
