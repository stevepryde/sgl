//! RFC-1982-style arithmetic for wrapping `u16` sequence numbers.

#[inline]
pub fn newer(a: u16, b: u16) -> bool {
    a != b && a.wrapping_sub(b) < 0x8000
}

#[inline]
pub fn diff(a: u16, b: u16) -> u16 {
    a.wrapping_sub(b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    #[wasm_bindgen_test(unsupported = test)]
    fn wrap_comparison_is_unambiguous_inside_window() {
        assert!(newer(0, u16::MAX));
        assert!(!newer(u16::MAX, 0));
        assert_eq!(diff(3, u16::MAX - 1), 5);
        assert!(!newer(0x8000, 0));
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod properties {
    use super::{diff, newer};
    use crate::proptest_support::check;
    use proptest::prelude::*;

    /// RFC 1982 serial-number comparison modelled on plain integers.
    fn model_newer(a: u16, b: u16) -> bool {
        let distance = (i32::from(a) - i32::from(b)).rem_euclid(65_536);
        (1..0x8000).contains(&distance)
    }

    /// Defect: a half-range comparison that flips at the wrap, so the
    /// receiver treats a stale sequence as new (replay) or a new one as
    /// stale (stall). Oracle: the RFC 1982 definition on `i32` arithmetic.
    #[test]
    fn sequence_comparison_matches_the_rfc_1982_model() {
        check((any::<u16>(), any::<u16>(), 1u16..0x8000), |(a, b, d)| {
            prop_assert_eq!(newer(a, b), model_newer(a, b));
            prop_assert!(!(newer(a, b) && newer(b, a)), "both newer");
            prop_assert_eq!(
                u32::from(diff(a, b)),
                (u32::from(a) + 65_536 - u32::from(b)) % 65_536
            );
            prop_assert!(newer(a.wrapping_add(d), a), "+{d} must be newer");
            prop_assert!(!newer(a, a.wrapping_add(d)));
            prop_assert_eq!(diff(a.wrapping_add(d), a), d);
            Ok(())
        });
    }
}
