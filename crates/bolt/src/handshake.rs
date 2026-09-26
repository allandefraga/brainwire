//! Bolt version negotiation (research/bolt.md, section 1).

pub const MAGIC: [u8; 4] = [0x60, 0x60, 0xB0, 0x17];

const SUPPORTED: [(u8, u8); 4] = [(5, 4), (5, 3), (5, 2), (5, 1)];

pub fn negotiate(proposals: &[u8; 16]) -> Option<(u8, u8)> {
    for p in proposals.chunks(4) {
        let (range, minor, major) = (p[1], p[2], p[3]);
        if major == 0xFF {
            continue; // 5.7 manifest proposal, not offered
        }
        let lowest = minor.saturating_sub(range);
        if let Some(v) = SUPPORTED
            .iter()
            .find(|(ma, mi)| *ma == major && *mi <= minor && *mi >= lowest)
        {
            return Some(*v);
        }
    }
    None
}

pub fn response(version: Option<(u8, u8)>) -> [u8; 4] {
    match version {
        Some((major, minor)) => [0, 0, minor, major],
        None => [0, 0, 0, 0],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn driver_5_28_gets_5_4() {
        // What driver 5.28.6 sends after the magic: manifest, 5.8..5.0, 4.4..4.2, 3.0.
        let p = [
            0x00, 0x00, 0x01, 0xFF, 0x00, 0x08, 0x08, 0x05, 0x00, 0x02, 0x04, 0x04, 0x00, 0x00,
            0x00, 0x03,
        ];
        assert_eq!(negotiate(&p), Some((5, 4)));
        assert_eq!(response(Some((5, 4))), [0x00, 0x00, 0x04, 0x05]);
    }

    #[test]
    fn exact_5_2_is_accepted_and_5_5_is_not() {
        let mut p = [0u8; 16];
        p[..4].copy_from_slice(&[0, 0, 2, 5]);
        assert_eq!(negotiate(&p), Some((5, 2)));
        p[..4].copy_from_slice(&[0, 0, 5, 5]);
        assert_eq!(negotiate(&p), None);
        assert_eq!(response(None), [0, 0, 0, 0]);
    }

    #[test]
    fn bolt_4_only_is_refused() {
        let mut p = [0u8; 16];
        p[..4].copy_from_slice(&[0, 2, 4, 4]);
        assert_eq!(negotiate(&p), None);
    }
}
