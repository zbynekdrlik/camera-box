//! The camera number of a box, from its hostname (issue 1242, moved here by issue 1487 so the
//! NDI send stagger and the JPEG XS header read the same parser).

/// The highest camera number the parser accepts. Keeps a garbage hostname like `CAM4294967295`
/// from being read as a real camera; any real cambox is far below this.
pub const MAX_CAMERA_NUMBER: u32 = 99;

/// Parse the camera number from a box hostname: `CAM7` / `cam7` / ` Cam3 ` → `Some(n)`.
/// Anything else (`camera-box`, `CAM`, `CAM0`, `CAM1-lx`, `strih-snv`, empty) → `None`.
pub fn camera_number_from_hostname(hostname: &str) -> Option<u32> {
    let h = hostname.trim();
    if h.len() < 4 || !h.is_char_boundary(3) || !h[..3].eq_ignore_ascii_case("cam") {
        return None;
    }
    let digits = &h[3..];
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let n: u32 = digits.parse().ok()?;
    if !(1..=MAX_CAMERA_NUMBER).contains(&n) {
        return None;
    }
    Some(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_fleet_hostname_convention_1242() {
        assert_eq!(camera_number_from_hostname("CAM1"), Some(1));
        assert_eq!(camera_number_from_hostname("CAM7"), Some(7));
        assert_eq!(camera_number_from_hostname("cam3"), Some(3));
        assert_eq!(camera_number_from_hostname(" Cam5 \n"), Some(5));
        assert_eq!(camera_number_from_hostname("CAM07"), Some(7));
        assert_eq!(camera_number_from_hostname("CAM12"), Some(12));
    }

    #[test]
    fn unknown_or_garbage_hostname_is_none_never_a_panic_1242() {
        for h in [
            "",
            "   ",
            "CAM",
            "CAM0",
            "camera-box",
            "CAM1-lx",
            "strih-snv",
            "cam 1",
            "CAM-1",
            "CAM4294967295",
            "CAM100",
            "čam1",
            "ca",
            "C",
        ] {
            assert_eq!(camera_number_from_hostname(h), None, "hostname {h:?}");
        }
    }
}
