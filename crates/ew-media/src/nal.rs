//! Minimal H.264 Annex-B inspection.

/// Iterates NAL unit types in an Annex-B buffer.
pub fn nal_types(data: &[u8]) -> impl Iterator<Item = u8> + '_ {
    let mut i = 0;
    std::iter::from_fn(move || {
        while i + 3 < data.len() {
            if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
                let t = data[i + 3] & 0x1f;
                i += 3;
                return Some(t);
            }
            i += 1;
        }
        None
    })
}

pub fn is_idr(data: &[u8]) -> bool {
    nal_types(data).any(|t| t == 5)
}

pub fn has_sps(data: &[u8]) -> bool {
    nal_types(data).any(|t| t == 7)
}

/// Converts an AVCC/`MF_MT_MPEG_SEQUENCE_HEADER` blob to Annex-B if needed.
pub fn to_annexb(seq: &[u8]) -> Vec<u8> {
    if seq.starts_with(&[0, 0, 0, 1]) || seq.starts_with(&[0, 0, 1]) {
        return seq.to_vec();
    }
    // Length-prefixed (4-byte) units.
    let mut out = Vec::new();
    let mut i = 0;
    while i + 4 <= seq.len() {
        let n = u32::from_be_bytes(seq[i..i + 4].try_into().unwrap()) as usize;
        i += 4;
        if i + n > seq.len() {
            break;
        }
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(&seq[i..i + n]);
        i += n;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_types() {
        let d = [0, 0, 0, 1, 0x67, 1, 2, 0, 0, 1, 0x68, 3, 0, 0, 1, 0x65, 9];
        assert_eq!(nal_types(&d).collect::<Vec<_>>(), vec![7, 8, 5]);
        assert!(is_idr(&d) && has_sps(&d));
        assert!(!is_idr(&[0, 0, 1, 0x41, 0]));
    }

    #[test]
    fn avcc_to_annexb() {
        let avcc = [0, 0, 0, 2, 0x67, 1, 0, 0, 0, 1, 0x68];
        assert_eq!(to_annexb(&avcc), vec![0, 0, 0, 1, 0x67, 1, 0, 0, 0, 1, 0x68]);
    }
}
