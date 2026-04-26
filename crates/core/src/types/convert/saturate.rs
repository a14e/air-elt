//! Numeric saturating primitives shared by all narrow-with-truncate paths.
//!
//! Each helper clamps to the target's representable max/min instead of
//! panicking or wrapping. They never panic on any finite input. NaN inputs
//! are returned as `None` so callers can decide whether to error.

use bigdecimal::{BigDecimal, ToPrimitive};
use num_bigint::{BigInt, Sign};

pub fn sat_i64_to_i32(n: i64) -> i32 {
    n.clamp(i32::MIN as i64, i32::MAX as i64) as i32
}

pub fn sat_i64_to_i16(n: i64) -> i16 {
    n.clamp(i16::MIN as i64, i16::MAX as i64) as i16
}

pub fn sat_i32_to_i16(n: i32) -> i16 {
    n.clamp(i16::MIN as i32, i16::MAX as i32) as i16
}

pub fn sat_u64_to_u32(n: u64) -> u32 {
    n.min(u32::MAX as u64) as u32
}

pub fn sat_u64_to_u16(n: u64) -> u16 {
    n.min(u16::MAX as u64) as u16
}

pub fn sat_u64_to_u8(n: u64) -> u8 {
    n.min(u8::MAX as u64) as u8
}

pub fn sat_u32_to_u16(n: u32) -> u16 {
    n.min(u16::MAX as u32) as u16
}

pub fn sat_u32_to_u8(n: u32) -> u8 {
    n.min(u8::MAX as u32) as u8
}

pub fn sat_u16_to_u8(n: u16) -> u8 {
    n.min(u8::MAX as u16) as u8
}

/// Signed → unsigned: negative clamps to 0, otherwise saturate to the
/// unsigned max.
pub fn sat_i64_to_u64(n: i64) -> u64 {
    if n < 0 { 0 } else { n as u64 }
}

pub fn sat_i64_to_u32(n: i64) -> u32 {
    if n < 0 {
        0
    } else {
        (n as u64).min(u32::MAX as u64) as u32
    }
}

pub fn sat_i64_to_u16(n: i64) -> u16 {
    if n < 0 {
        0
    } else {
        (n as u64).min(u16::MAX as u64) as u16
    }
}

pub fn sat_i64_to_u8(n: i64) -> u8 {
    if n < 0 {
        0
    } else {
        (n as u64).min(u8::MAX as u64) as u8
    }
}

/// Unsigned → signed: saturate at the signed max.
pub fn sat_u64_to_i64(n: u64) -> i64 {
    n.min(i64::MAX as u64) as i64
}

pub fn sat_u64_to_i32(n: u64) -> i32 {
    n.min(i32::MAX as u64) as i32
}

pub fn sat_u64_to_i16(n: u64) -> i16 {
    n.min(i16::MAX as u64) as i16
}

pub fn sat_u32_to_i32(n: u32) -> i32 {
    n.min(i32::MAX as u32) as i32
}

pub fn sat_u32_to_i16(n: u32) -> i16 {
    n.min(i16::MAX as u32) as i16
}

pub fn sat_u16_to_i16(n: u16) -> i16 {
    n.min(i16::MAX as u16) as i16
}

/// `f64 → f32` with saturation to `f32::MAX/MIN`. Preserves NaN.
pub fn sat_f64_to_f32(n: f64) -> f32 {
    if n.is_nan() {
        return f32::NAN;
    }
    if n > f32::MAX as f64 {
        return f32::MAX;
    }
    if n < f32::MIN as f64 {
        return f32::MIN;
    }
    n as f32
}

/// `f64 → i64` truncate-toward-zero with saturation. NaN → `None`.
pub fn sat_f64_to_i64(n: f64) -> Option<i64> {
    if n.is_nan() {
        return None;
    }
    let truncated = n.trunc();
    if truncated >= i64::MAX as f64 {
        return Some(i64::MAX);
    }
    if truncated <= i64::MIN as f64 {
        return Some(i64::MIN);
    }
    Some(truncated as i64)
}

pub fn sat_f64_to_i32(n: f64) -> Option<i32> {
    if n.is_nan() {
        return None;
    }
    let truncated = n.trunc();
    if truncated >= i32::MAX as f64 {
        return Some(i32::MAX);
    }
    if truncated <= i32::MIN as f64 {
        return Some(i32::MIN);
    }
    Some(truncated as i32)
}

pub fn sat_f64_to_i16(n: f64) -> Option<i16> {
    if n.is_nan() {
        return None;
    }
    let truncated = n.trunc();
    if truncated >= i16::MAX as f64 {
        return Some(i16::MAX);
    }
    if truncated <= i16::MIN as f64 {
        return Some(i16::MIN);
    }
    Some(truncated as i16)
}

pub fn sat_f64_to_u64(n: f64) -> Option<u64> {
    if n.is_nan() {
        return None;
    }
    let truncated = n.trunc();
    if truncated < 0.0 {
        return Some(0);
    }
    if truncated >= u64::MAX as f64 {
        return Some(u64::MAX);
    }
    Some(truncated as u64)
}

pub fn sat_f64_to_u32(n: f64) -> Option<u32> {
    sat_f64_to_u64(n).map(|x| x.min(u32::MAX as u64) as u32)
}

pub fn sat_f64_to_u16(n: f64) -> Option<u16> {
    sat_f64_to_u64(n).map(|x| x.min(u16::MAX as u64) as u16)
}

pub fn sat_f64_to_u8(n: f64) -> Option<u8> {
    sat_f64_to_u64(n).map(|x| x.min(u8::MAX as u64) as u8)
}

/// `BigInt → i64` saturating.
pub fn sat_bigint_to_i64(b: &BigInt) -> i64 {
    b.to_i64().unwrap_or_else(|| match b.sign() {
        Sign::Minus => i64::MIN,
        _ => i64::MAX,
    })
}

pub fn sat_bigint_to_i32(b: &BigInt) -> i32 {
    b.to_i32().unwrap_or_else(|| match b.sign() {
        Sign::Minus => i32::MIN,
        _ => i32::MAX,
    })
}

pub fn sat_bigint_to_i16(b: &BigInt) -> i16 {
    b.to_i16().unwrap_or_else(|| match b.sign() {
        Sign::Minus => i16::MIN,
        _ => i16::MAX,
    })
}

pub fn sat_bigint_to_u64(b: &BigInt) -> u64 {
    b.to_u64().unwrap_or_else(|| match b.sign() {
        Sign::Minus => 0,
        _ => u64::MAX,
    })
}

pub fn sat_bigint_to_u32(b: &BigInt) -> u32 {
    b.to_u32().unwrap_or_else(|| match b.sign() {
        Sign::Minus => 0,
        _ => u32::MAX,
    })
}

pub fn sat_bigint_to_u16(b: &BigInt) -> u16 {
    b.to_u16().unwrap_or_else(|| match b.sign() {
        Sign::Minus => 0,
        _ => u16::MAX,
    })
}

pub fn sat_bigint_to_u8(b: &BigInt) -> u8 {
    b.to_u8().unwrap_or_else(|| match b.sign() {
        Sign::Minus => 0,
        _ => u8::MAX,
    })
}

/// Saturate `b` to fit into `width` decimal digits (i.e. `|b| <= 10^width - 1`).
/// Negative values use `-(10^width - 1)`.
pub fn sat_bigint_to_width(b: &BigInt, width: u32) -> BigInt {
    let max = bigint_pow10(width) - BigInt::from(1);
    let min = -max.clone();
    if b > &max {
        max
    } else if b < &min {
        min
    } else {
        b.clone()
    }
}

fn bigint_pow10(exp: u32) -> BigInt {
    let mut out = BigInt::from(1);
    let ten = BigInt::from(10);
    for _ in 0..exp {
        out *= &ten;
    }
    out
}

/// `BigDecimal → BigInt` truncate-toward-zero (drops fractional part).
pub fn bigdecimal_to_bigint_truncating(d: &BigDecimal) -> BigInt {
    d.with_scale(0).into_bigint_and_exponent().0
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::str::FromStr;

    #[test]
    fn i64_to_i32_saturates() {
        assert_eq!(sat_i64_to_i32(7), 7);
        assert_eq!(sat_i64_to_i32(i32::MAX as i64 + 1), i32::MAX);
        assert_eq!(sat_i64_to_i32(i32::MIN as i64 - 1), i32::MIN);
    }

    #[test]
    fn signed_to_unsigned_zero_floor() {
        assert_eq!(sat_i64_to_u32(-1), 0);
        assert_eq!(sat_i64_to_u32(5), 5);
        assert_eq!(sat_i64_to_u32(u32::MAX as i64 + 1), u32::MAX);
    }

    #[test]
    fn f64_to_f32_saturates() {
        assert_eq!(sat_f64_to_f32(1.5), 1.5f32);
        assert_eq!(sat_f64_to_f32(f64::MAX), f32::MAX);
        assert_eq!(sat_f64_to_f32(f64::MIN), f32::MIN);
        assert!(sat_f64_to_f32(f64::NAN).is_nan());
    }

    #[test]
    fn f64_to_int_truncates_toward_zero() {
        assert_eq!(sat_f64_to_i64(1.7), Some(1));
        assert_eq!(sat_f64_to_i64(-1.7), Some(-1));
        assert_eq!(sat_f64_to_i32(1e30), Some(i32::MAX));
        assert_eq!(sat_f64_to_i64(f64::NAN), None);
    }

    #[test]
    fn bigint_to_width_saturates() {
        let big = BigInt::from_str("99999999999").unwrap();
        // 10-digit max is 9_999_999_999.
        let out = sat_bigint_to_width(&big, 10);
        assert_eq!(out, BigInt::from_str("9999999999").unwrap());

        let neg = BigInt::from_str("-99999999999").unwrap();
        let out = sat_bigint_to_width(&neg, 10);
        assert_eq!(out, BigInt::from_str("-9999999999").unwrap());

        let small = BigInt::from(123);
        let out = sat_bigint_to_width(&small, 10);
        assert_eq!(out, BigInt::from(123));
    }

    #[test]
    fn bigdecimal_truncates_toward_zero() {
        let d: BigDecimal = "12.99".parse().unwrap();
        assert_eq!(bigdecimal_to_bigint_truncating(&d), BigInt::from(12));
        let d: BigDecimal = "-12.99".parse().unwrap();
        assert_eq!(bigdecimal_to_bigint_truncating(&d), BigInt::from(-12));
        let d: BigDecimal = "0".parse().unwrap();
        assert_eq!(bigdecimal_to_bigint_truncating(&d), BigInt::from(0));
    }
}
