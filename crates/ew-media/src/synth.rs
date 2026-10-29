//! Synthetic NV12 "desktops" for the load simulator: a per-PC background,
//! the PC number and a running wall clock drawn as big seven-segment digits
//! (so recordings can be checked for correct timestamps by eye), a moving bar,
//! and lines of fake text that change slowly like someone typing.

pub struct Synth {
    pub w: u32,
    pub h: u32,
    seed: u32,
    buf: Vec<u8>,
}

const SEGS: [u8; 10] = [0x3f, 0x06, 0x5b, 0x4f, 0x66, 0x6d, 0x7d, 0x07, 0x7f, 0x6f];

impl Synth {
    pub fn new(w: u32, h: u32, seed: u32) -> Self {
        let w = crate::even(w);
        let h = crate::even(h);
        Self { w, h, seed, buf: vec![0; (w * h * 3 / 2) as usize] }
    }

    /// Renders the frame for `t_ms` (wall clock, ms since epoch) and returns NV12 bytes.
    pub fn frame(&mut self, t_ms: u64, label: u32) -> &[u8] {
        let (w, h) = (self.w as usize, self.h as usize);
        let base = 200u8.saturating_sub((self.seed % 7) as u8 * 6);
        self.buf[..w * h].fill(base);
        // chroma: tint per seed
        let (u, v) = (118 + (self.seed % 5) as u8 * 4, 124 + (self.seed % 3) as u8 * 4);
        for p in self.buf[w * h..].chunks_exact_mut(2) {
            p[0] = u;
            p[1] = v;
        }
        let unit = (h / 40).max(2);
        // "window title bar"
        self.rect(0, 0, w, unit * 3, 60);
        // fake text lines; one more appears every 5 s
        let lines = ((t_ms / 5000) % 12) as usize + 3;
        for i in 0..lines {
            let len = (w / 2) * (((self.seed as usize + i * 7) % 5) + 3) / 8;
            self.rect(unit * 4, unit * (6 + i * 2), len, unit, 90);
        }
        // moving bar (shows motion/latency)
        let x = ((t_ms / 40) as usize * unit) % w.max(1);
        self.rect(x.min(w - unit), h - unit * 3, unit, unit * 2, 30);
        // PC number top-right, clock bottom-right
        let digit = unit * 3;
        self.number(label as u64, 3, w - digit * 4 - unit * 4, unit * 6, digit, 20);
        let secs = (t_ms / 1000) % 86400;
        let (hh, mm, ss) = (secs / 3600, (secs / 60) % 60, secs % 60);
        let mut x = w - digit * 9 - unit * 4;
        for (i, part) in [hh, mm, ss].iter().enumerate() {
            self.number(*part, 2, x, h - digit * 2 - unit * 6, digit, 20);
            x += digit * 3;
            if i < 2 {
                self.rect(x - digit / 2, h - digit * 2 - unit * 6 + digit / 2, unit, unit, 20);
                self.rect(x - digit / 2, h - unit * 6 - digit / 2, unit, unit, 20);
            }
        }
        &self.buf
    }

    fn rect(&mut self, x: usize, y: usize, rw: usize, rh: usize, luma: u8) {
        let (w, h) = (self.w as usize, self.h as usize);
        for yy in y.min(h)..(y + rh).min(h) {
            let row = &mut self.buf[yy * w..yy * w + w];
            for px in &mut row[x.min(w)..(x + rw).min(w)] {
                *px = luma;
            }
        }
    }

    fn number(&mut self, n: u64, digits: usize, x: usize, y: usize, size: usize, luma: u8) {
        let s = format!("{:0width$}", n % 10u64.pow(digits as u32), width = digits);
        for (i, ch) in s.bytes().enumerate() {
            self.digit((ch - b'0') as usize, x + i * (size + size / 2), y, size, luma);
        }
    }

    fn digit(&mut self, d: usize, x: usize, y: usize, s: usize, luma: u8) {
        let t = (s / 5).max(1);
        let m = SEGS[d];
        let segs: [(usize, usize, usize, usize); 7] = [
            (x, y, s, t),                 // a
            (x + s - t, y, t, s),         // b
            (x + s - t, y + s, t, s),     // c
            (x, y + 2 * s - t, s, t),     // d
            (x, y + s, t, s),             // e
            (x, y, t, s),                 // f
            (x, y + s - t / 2, s, t),     // g
        ];
        for (i, r) in segs.iter().enumerate() {
            if m & (1 << i) != 0 {
                self.rect(r.0, r.1, r.2, r.3, luma);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_size_and_changes_over_time() {
        let mut s = Synth::new(481, 271, 3);
        assert_eq!((s.w, s.h), (480, 270));
        let a = s.frame(1_000, 7).to_vec();
        assert_eq!(a.len(), 480 * 270 * 3 / 2);
        let b = s.frame(2_000, 7).to_vec();
        assert_ne!(a, b, "clock must advance");
    }
}
