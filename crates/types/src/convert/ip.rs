//! IPv4 / IPv6 parsing, rendering, and byte round-trip helpers.
//!
//! Pure functions — they know nothing about `Value`. They accept borrowed
//! input and return typed `Ipv4Addr` / `Ipv6Addr` (or their octet form).
//! The `convert` dispatcher in `super` wraps them around `Value` variants.
//!
//! Text format: RFC 5952 lower-case for IPv6, dotted-quad for IPv4 — what
//! `Ipv4Addr::to_string()` / `Ipv6Addr::to_string()` already produce.
//!
//! Bytes format: network byte order (big-endian octets) per RFC 791 /
//! RFC 8200. Matches `Ipv4Addr::octets()` (`[u8; 4]`) and
//! `Ipv6Addr::octets()` (`[u8; 16]`), and is also what the POSIX
//! `in_addr` / `in6_addr` structs carry on the wire.

use std::net::{Ipv4Addr, Ipv6Addr};
use std::str::FromStr;

use super::error::ConvertError;

pub fn parse_v4(s: &str) -> Result<Ipv4Addr, ConvertError> {
    Ipv4Addr::from_str(s.trim()).map_err(|e| ConvertError::InvalidIp {
        family: "ipv4",
        reason: e.to_string(),
    })
}

pub fn parse_v6(s: &str) -> Result<Ipv6Addr, ConvertError> {
    Ipv6Addr::from_str(s.trim()).map_err(|e| ConvertError::InvalidIp {
        family: "ipv6",
        reason: e.to_string(),
    })
}

pub fn to_text_v4(a: Ipv4Addr) -> String {
    a.to_string()
}

pub fn to_text_v6(a: Ipv6Addr) -> String {
    a.to_string()
}

/// IPv4-mapped form: every IPv4 widens losslessly to `::ffff:a.b.c.d`.
pub fn v4_to_v6(a: Ipv4Addr) -> Ipv6Addr {
    a.to_ipv6_mapped()
}

/// Extract an IPv4 only when `a` is an IPv4-mapped IPv6 address
/// (`::ffff:a.b.c.d`). All other v6 addresses error — the conversion is
/// genuinely lossy and the caller must opt into `truncate=true` to even
/// reach this code path via the dispatcher.
pub fn v6_to_v4_if_mapped(a: Ipv6Addr) -> Result<Ipv4Addr, ConvertError> {
    a.to_ipv4_mapped()
        .ok_or_else(|| ConvertError::IpV6NotMappable {
            addr: a.to_string(),
        })
}

/// 4 octets in network byte order (BE) — matches `in_addr`.
pub fn to_bytes_v4(a: Ipv4Addr) -> [u8; 4] {
    a.octets()
}

/// 16 octets in network byte order (BE) — matches `in6_addr`.
pub fn to_bytes_v6(a: Ipv6Addr) -> [u8; 16] {
    a.octets()
}

pub fn from_bytes_v4(b: &[u8]) -> Result<Ipv4Addr, ConvertError> {
    if b.len() != 4 {
        return Err(ConvertError::Length {
            expected: 4,
            got: b.len(),
        });
    }
    let mut arr = [0u8; 4];
    arr.copy_from_slice(b);
    Ok(Ipv4Addr::from(arr))
}

pub fn from_bytes_v6(b: &[u8]) -> Result<Ipv6Addr, ConvertError> {
    if b.len() != 16 {
        return Err(ConvertError::Length {
            expected: 16,
            got: b.len(),
        });
    }
    let mut arr = [0u8; 16];
    arr.copy_from_slice(b);
    Ok(Ipv6Addr::from(arr))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn parse_v4_happy() {
        let a = parse_v4("192.0.2.1").unwrap();
        assert_eq!(a, Ipv4Addr::new(192, 0, 2, 1));
        assert_eq!(to_text_v4(a), "192.0.2.1");
    }

    #[test]
    fn parse_v4_trims_whitespace() {
        assert_eq!(
            parse_v4("  10.0.0.1\n").unwrap(),
            Ipv4Addr::new(10, 0, 0, 1)
        );
    }

    #[test]
    fn parse_v4_rejects_garbage() {
        assert!(matches!(
            parse_v4("not-an-ip"),
            Err(ConvertError::InvalidIp { family: "ipv4", .. })
        ));
        assert!(matches!(
            parse_v4("256.0.0.1"),
            Err(ConvertError::InvalidIp { family: "ipv4", .. })
        ));
    }

    #[test]
    fn parse_v6_happy_and_canonical_form() {
        let a = parse_v6("2001:0db8:0000:0000:0000:0000:0000:0001").unwrap();
        // RFC 5952 canonical form is the lower-case compressed shape.
        assert_eq!(to_text_v6(a), "2001:db8::1");
    }

    #[test]
    fn parse_v6_rejects_garbage() {
        assert!(matches!(
            parse_v6("zzzz::1"),
            Err(ConvertError::InvalidIp { family: "ipv6", .. })
        ));
    }

    #[test]
    fn v4_to_v6_mapped_canonical_text_form() {
        // Anchor a specific edge: the canonical text form of the
        // IPv4-mapped IPv6 address embeds the dotted-quad untouched in
        // the trailing 32 bits — the round-trip property is covered
        // exhaustively by `ip_v4_mapped_v6_round_trip_boundaries` below.
        let a4 = Ipv4Addr::new(203, 0, 113, 42);
        let a6 = v4_to_v6(a4);
        assert_eq!(to_text_v6(a6), "::ffff:203.0.113.42");
    }

    #[test]
    fn v6_to_v4_rejects_non_mapped() {
        let a6: Ipv6Addr = "2001:db8::1".parse().unwrap();
        assert!(matches!(
            v6_to_v4_if_mapped(a6),
            Err(ConvertError::IpV6NotMappable { .. })
        ));
        // IPv4-compatible form (RFC 4291 §2.5.5.1, deprecated):
        // `::a.b.c.d` has the IPv4 host in the low 32 bits but lacks
        // the `::ffff:` prefix. `to_ipv4_mapped()` rejects it (only
        // IPv4-mapped is accepted), so we surface IpV6NotMappable.
        let v4_compat: Ipv6Addr = "::192.0.2.1".parse().unwrap();
        assert!(matches!(
            v6_to_v4_if_mapped(v4_compat),
            Err(ConvertError::IpV6NotMappable { .. })
        ));
        // Loopback `::1` is not mappable either.
        let loopback: Ipv6Addr = "::1".parse().unwrap();
        assert!(matches!(
            v6_to_v4_if_mapped(loopback),
            Err(ConvertError::IpV6NotMappable { .. })
        ));
    }

    #[test]
    fn to_bytes_v4_matches_dotted_quad_octets() {
        // Anchor a single specific byte order claim — the property
        // `ipv4_be_round_trip` exercises round-tripping exhaustively;
        // this case nails down the BE octet layout itself.
        let a4 = Ipv4Addr::new(10, 0, 0, 1);
        assert_eq!(to_bytes_v4(a4), [10, 0, 0, 1]);
    }

    #[test]
    fn ip_v4_broadcast_and_loopback_round_trip() {
        // Explicit anchors for the well-known boundary IPv4 addresses.
        // The randomised PT below already covers the property; these
        // names document the intent for future readers.
        for a4 in [Ipv4Addr::BROADCAST, Ipv4Addr::LOCALHOST] {
            let raw = to_bytes_v4(a4);
            assert_eq!(from_bytes_v4(&raw).unwrap(), a4);
            let mapped = v4_to_v6(a4);
            assert_eq!(v6_to_v4_if_mapped(mapped).unwrap(), a4);
        }
    }

    #[test]
    fn from_bytes_v4_rejects_wrong_lengths() {
        for len in [0usize, 3, 5, 16] {
            let buf = vec![0u8; len];
            assert!(matches!(
                from_bytes_v4(&buf),
                Err(ConvertError::Length { expected: 4, .. })
            ));
        }
    }

    #[test]
    fn from_bytes_v6_rejects_wrong_lengths() {
        for len in [0usize, 4, 15, 17] {
            let buf = vec![0u8; len];
            assert!(matches!(
                from_bytes_v6(&buf),
                Err(ConvertError::Length { expected: 16, .. })
            ));
        }
    }

    // ---- Property-based tests --------------------------------------

    use proptest::prelude::*;

    fn any_ipv4() -> impl Strategy<Value = Ipv4Addr> {
        any::<[u8; 4]>().prop_map(Ipv4Addr::from)
    }

    fn any_ipv6() -> impl Strategy<Value = Ipv6Addr> {
        any::<[u8; 16]>().prop_map(Ipv6Addr::from)
    }

    #[test_strategy::proptest(ProptestConfig::with_cases(256))]
    fn ipv4_be_round_trip(#[strategy(any_ipv4())] a: Ipv4Addr) {
        let raw = to_bytes_v4(a);
        let back = from_bytes_v4(&raw).expect("decode");
        prop_assert_eq!(back, a);
    }

    #[test_strategy::proptest(ProptestConfig::with_cases(256))]
    fn ipv6_be_round_trip(#[strategy(any_ipv6())] a: Ipv6Addr) {
        let raw = to_bytes_v6(a);
        let back = from_bytes_v6(&raw).expect("decode");
        prop_assert_eq!(back, a);
    }

    /// Widen every IPv4 to its `::ffff:a.b.c.d` form and narrow back —
    /// the four octets must survive bit-for-bit. This exercises the
    /// `truncate=true` arm of `v6_to_v4_if_mapped`.
    #[test_strategy::proptest(ProptestConfig::with_cases(256))]
    fn ip_v4_mapped_v6_round_trip_boundaries(#[strategy(any_ipv4())] a4: Ipv4Addr) {
        let a6 = v4_to_v6(a4);
        let back = v6_to_v4_if_mapped(a6).expect("mapped");
        prop_assert_eq!(back, a4);
        // The mapped form must keep the IPv4 octets in the trailing 32 bits.
        let octets6 = to_bytes_v6(a6);
        prop_assert_eq!(&octets6[12..], &a4.octets()[..]);
    }
}
