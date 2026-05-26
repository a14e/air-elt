//! Postgres `inet` type.
//!
//! `inet` carries an IP host address with an optional netmask
//! (e.g. `192.0.2.1` (host) or `192.0.2.0/24` (network with mask)).
//! Air Elt exposes it as a connector-local custom type so the netmask
//! survives the cross-vendor pipeline; downstream conversions to
//! canonical `Ipv4` / `Ipv6` drop the mask and therefore require the
//! operator's explicit `truncate = true` consent.
//!
//! Wire form: sqlx-postgres binds and decodes `inet` as
//! [`sqlx::types::ipnetwork::IpNetwork`] under the `ipnetwork` feature
//! (enabled at the workspace level).
//!
//! Cursor compatibility: yes. The canonical text envelope is
//! `IpNetwork::to_string()` (`"addr/prefix"`) which round-trips
//! through `IpNetwork::from_str`; ordering is delegated to the source
//! `ORDER BY inet` clause (Postgres compares numerically by family,
//! address, then prefix).

use std::any::Any;
use std::str::FromStr;

use sqlx::types::ipnetwork::IpNetwork;

use air_elt_core::error::JsonEncodeError;
use air_elt_core::types::convert::ConvertError;
use air_elt_core::types::convert::context::ConversionContext;
use air_elt_core::types::data_type::DataType;
use air_elt_core::types::dynamic::{DynType, DynValue};
use air_elt_core::types::value::Value;

#[derive(Debug, Clone, Copy, Default)]
pub struct PgInetType;

impl PgInetType {
    pub const KIND: &'static str = "postgresql.inet";

    /// Stable max canonical text width: longest possible
    /// `IpNetwork::to_string()` output is the uncompressed v6 form
    /// followed by `/128` — 39 + 4 = 43 chars.
    ///
    /// Cross-reference: `core::types::matrix` uses `39` for the bare
    /// `DataType::Ipv6 → Text` arm (no prefix); this `43` covers the
    /// prefix-aware `IpNetwork::to_string()` envelope.
    const TEXT_MAX: u32 = 43;
}

impl DynType for PgInetType {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn kind(&self) -> &str {
        Self::KIND
    }

    fn cursor_compatible(&self) -> bool {
        true
    }

    fn decode_cursor_value(&self, json: &serde_json::Value) -> Result<Box<dyn DynValue>, String> {
        let s = json
            .as_str()
            .ok_or_else(|| format!("expected string envelope for {}", self.kind()))?;
        IpNetwork::from_str(s)
            .map(|n| Box::new(PgInetValue(n)) as Box<dyn DynValue>)
            .map_err(|e| format!("invalid inet cursor value {s:?}: {e}"))
    }

    /// `Inet → Text` is lossless (canonical text keeps the prefix).
    /// `Inet → Ipv4/Ipv6` drops the netmask — admitted only under
    /// `truncate = true`; cross-family is a runtime error in `convert`.
    fn can_convert_to(&self, target: &DataType, truncate: bool) -> bool {
        match target {
            DataType::Custom(t) if t.kind() == self.kind() => true,
            DataType::Text { size } => size.is_none_or(|n| n >= Self::TEXT_MAX),
            DataType::Ipv4 | DataType::Ipv6 => truncate,
            _ => false,
        }
    }

    /// `Inet ← Text` parses via `IpNetwork::from_str` (handles both
    /// host and CIDR forms). `Inet ← Ipv4 / Ipv6` widens by stamping
    /// implicit `/32` / `/128` — both lossless.
    fn can_construct_from(&self, src: &DataType, _truncate: bool) -> bool {
        match src {
            DataType::Custom(t) if t.kind() == self.kind() => true,
            DataType::Text { .. } => true,
            DataType::Ipv4 | DataType::Ipv6 => true,
            _ => false,
        }
    }

    fn convert(
        &self,
        value: Value,
        target: &DataType,
        ctx: &ConversionContext,
    ) -> Result<Value, ConvertError> {
        let inet = match &value {
            Value::Custom(v) => v
                .as_any()
                .downcast_ref::<PgInetValue>()
                .map(|w| w.0)
                .ok_or_else(|| ConvertError::ValueShapeMismatch {
                    src: DataType::Custom(Box::new(*self)),
                })?,
            _ => {
                return Err(ConvertError::ValueShapeMismatch {
                    src: DataType::Custom(Box::new(*self)),
                });
            }
        };
        match target {
            DataType::Custom(t) if t.kind() == self.kind() => Ok(value),
            DataType::Text { .. } => Ok(Value::Text(inet.to_string())),
            DataType::Ipv4 => {
                if !ctx.truncate {
                    return Err(ConvertError::Unsupported {
                        src: DataType::Custom(Box::new(*self)),
                        dst: DataType::Ipv4,
                    });
                }
                match inet {
                    IpNetwork::V4(n) => Ok(Value::Ipv4(n.ip())),
                    IpNetwork::V6(_) => Err(ConvertError::ValueShapeMismatch {
                        src: DataType::Custom(Box::new(*self)),
                    }),
                }
            }
            DataType::Ipv6 => {
                if !ctx.truncate {
                    return Err(ConvertError::Unsupported {
                        src: DataType::Custom(Box::new(*self)),
                        dst: DataType::Ipv6,
                    });
                }
                match inet {
                    IpNetwork::V4(n) => Ok(Value::Ipv6(n.ip().to_ipv6_mapped())),
                    IpNetwork::V6(n) => Ok(Value::Ipv6(n.ip())),
                }
            }
            _ => Err(ConvertError::Unsupported {
                src: DataType::Custom(Box::new(*self)),
                dst: target.clone(),
            }),
        }
    }

    fn construct(
        &self,
        value: Value,
        src: &DataType,
        _ctx: &ConversionContext,
    ) -> Result<Value, ConvertError> {
        match (value, src) {
            (v @ Value::Custom(_), DataType::Custom(t)) if t.kind() == self.kind() => Ok(v),
            (Value::Text(s), DataType::Text { .. }) => {
                let n = IpNetwork::from_str(s.trim()).map_err(|e| ConvertError::InvalidIp {
                    family: "inet",
                    reason: e.to_string(),
                })?;
                Ok(Value::Custom(Box::new(PgInetValue(n))))
            }
            (Value::Ipv4(a), DataType::Ipv4) => {
                let n = IpNetwork::new(std::net::IpAddr::V4(a), 32).map_err(|e| {
                    ConvertError::InvalidIp {
                        family: "inet",
                        reason: e.to_string(),
                    }
                })?;
                Ok(Value::Custom(Box::new(PgInetValue(n))))
            }
            (Value::Ipv6(a), DataType::Ipv6) => {
                let n = IpNetwork::new(std::net::IpAddr::V6(a), 128).map_err(|e| {
                    ConvertError::InvalidIp {
                        family: "inet",
                        reason: e.to_string(),
                    }
                })?;
                Ok(Value::Custom(Box::new(PgInetValue(n))))
            }
            (_, _) => Err(ConvertError::Unsupported {
                src: src.clone(),
                dst: DataType::Custom(Box::new(*self)),
            }),
        }
    }

    fn parse_default(&self, literal: &toml::Value) -> Result<Option<Value>, String> {
        let s = literal
            .as_str()
            .ok_or_else(|| "expected string literal for inet default".to_owned())?;
        let n = IpNetwork::from_str(s.trim()).map_err(|e| format!("invalid inet literal: {e}"))?;
        Ok(Some(Value::Custom(Box::new(PgInetValue(n)))))
    }

    fn clone_box(&self) -> Box<dyn DynType> {
        Box::new(*self)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct PgInetValue(pub IpNetwork);

impl DynValue for PgInetValue {
    fn dyn_type(&self) -> Box<dyn DynType> {
        Box::new(PgInetType)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
    fn into_any(self: Box<Self>) -> Box<dyn std::any::Any> {
        self
    }

    fn is_equal(&self, other: &dyn DynValue) -> bool {
        match other.as_any().downcast_ref::<PgInetValue>() {
            Some(o) => self.0 == o.0,
            None => false,
        }
    }

    fn clone_box(&self) -> Box<dyn DynValue> {
        Box::new(*self)
    }

    fn to_json(&self) -> Result<serde_json::Value, JsonEncodeError> {
        Ok(serde_json::Value::String(self.0.to_string()))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn ctx_passthrough() -> ConversionContext {
        ConversionContext::passthrough()
    }

    fn ctx_truncate() -> ConversionContext {
        ConversionContext {
            default: None,
            truncate: true,
        }
    }

    fn host_v4(a: u8, b: u8, c: u8, d: u8) -> PgInetValue {
        PgInetValue(
            IpNetwork::new(
                std::net::IpAddr::V4(std::net::Ipv4Addr::new(a, b, c, d)),
                32,
            )
            .unwrap(),
        )
    }

    #[test]
    fn kind_is_stable() {
        assert_eq!(PgInetType.kind(), "postgresql.inet");
    }

    #[test]
    fn cursor_compatible() {
        assert!(PgInetType.cursor_compatible());
    }

    #[test]
    fn convert_inet_to_text_lossless() {
        let v = Value::Custom(Box::new(host_v4(192, 0, 2, 1)));
        let out = PgInetType
            .convert(v, &DataType::Text { size: None }, &ctx_passthrough())
            .unwrap();
        assert_eq!(out, Value::Text("192.0.2.1/32".to_string()));
    }

    #[test]
    fn convert_inet_to_ipv4_requires_truncate() {
        let v = Value::Custom(Box::new(host_v4(192, 0, 2, 1)));
        // Without truncate: rejected.
        let err = PgInetType.convert(v.clone(), &DataType::Ipv4, &ctx_passthrough());
        assert!(matches!(err, Err(ConvertError::Unsupported { .. })));
        // With truncate: succeeds, drops /32.
        let out = PgInetType
            .convert(v, &DataType::Ipv4, &ctx_truncate())
            .unwrap();
        assert_eq!(out, Value::Ipv4(std::net::Ipv4Addr::new(192, 0, 2, 1)));
    }

    #[test]
    fn convert_inet_to_ipv6_widens_v4_to_mapped() {
        let v = Value::Custom(Box::new(host_v4(203, 0, 113, 42)));
        let out = PgInetType
            .convert(v, &DataType::Ipv6, &ctx_truncate())
            .unwrap();
        let Value::Ipv6(a) = out else {
            panic!("expected Ipv6")
        };
        assert_eq!(a.to_string(), "::ffff:203.0.113.42");
    }

    #[test]
    fn convert_inet_with_subnet_to_ipv4_drops_mask() {
        let n = IpNetwork::from_str("192.0.2.0/24").unwrap();
        let v = Value::Custom(Box::new(PgInetValue(n)));
        let out = PgInetType
            .convert(v, &DataType::Ipv4, &ctx_truncate())
            .unwrap();
        // /24 net address has host part 192.0.2.0 — that's what
        // IpNetwork::V4::ip() returns.
        assert_eq!(out, Value::Ipv4(std::net::Ipv4Addr::new(192, 0, 2, 0)));
    }

    /// Mask edge `/0` (default route): inet.ip() returns 0.0.0.0.
    #[test]
    fn convert_inet_zero_mask_returns_zero_host() {
        let n = IpNetwork::from_str("0.0.0.0/0").unwrap();
        let v = Value::Custom(Box::new(PgInetValue(n)));
        let out = PgInetType
            .convert(v, &DataType::Ipv4, &ctx_truncate())
            .unwrap();
        assert_eq!(out, Value::Ipv4(std::net::Ipv4Addr::UNSPECIFIED));
    }

    /// IPv6 inet → Ipv4 (cross-family narrowing): even under
    /// truncate, this is a structural type mismatch — operators
    /// must extract an Ipv6 first then opt in to v6→v4 IPv4-mapped
    /// narrowing.
    #[test]
    fn convert_inet_v6_to_ipv4_is_family_mismatch_even_under_truncate() {
        let n = IpNetwork::from_str("2001:db8::1/128").unwrap();
        let v = Value::Custom(Box::new(PgInetValue(n)));
        let res = PgInetType.convert(v, &DataType::Ipv4, &ctx_truncate());
        assert!(matches!(res, Err(ConvertError::ValueShapeMismatch { .. })));
    }

    #[test]
    fn construct_inet_from_text_handles_cidr_form() {
        let v = Value::Text("10.0.0.0/8".into());
        let out = PgInetType
            .construct(v, &DataType::Text { size: None }, &ctx_passthrough())
            .unwrap();
        let Value::Custom(c) = out else { panic!() };
        let inet = c.as_any().downcast_ref::<PgInetValue>().unwrap();
        assert_eq!(inet.0.to_string(), "10.0.0.0/8");
    }

    #[test]
    fn construct_inet_from_text_handles_host_form() {
        let v = Value::Text("192.0.2.1".into());
        let out = PgInetType
            .construct(v, &DataType::Text { size: None }, &ctx_passthrough())
            .unwrap();
        let Value::Custom(c) = out else { panic!() };
        let inet = c.as_any().downcast_ref::<PgInetValue>().unwrap();
        // Bare hosts widen to /32.
        assert_eq!(inet.0.to_string(), "192.0.2.1/32");
    }

    #[test]
    fn construct_inet_from_ipv4_stamps_host_prefix() {
        let v = Value::Ipv4(std::net::Ipv4Addr::new(192, 0, 2, 1));
        let out = PgInetType
            .construct(v, &DataType::Ipv4, &ctx_passthrough())
            .unwrap();
        let Value::Custom(c) = out else { panic!() };
        let inet = c.as_any().downcast_ref::<PgInetValue>().unwrap();
        assert_eq!(inet.0.prefix(), 32);
    }

    #[test]
    fn construct_inet_from_ipv6_stamps_host_prefix() {
        let v = Value::Ipv6("2001:db8::1".parse().unwrap());
        let out = PgInetType
            .construct(v, &DataType::Ipv6, &ctx_passthrough())
            .unwrap();
        let Value::Custom(c) = out else { panic!() };
        let inet = c.as_any().downcast_ref::<PgInetValue>().unwrap();
        assert_eq!(inet.0.prefix(), 128);
    }

    #[test]
    fn parse_default_accepts_inet_literal() {
        let lit = toml::Value::String("192.0.2.0/24".into());
        let v = PgInetType.parse_default(&lit).unwrap().unwrap();
        let Value::Custom(c) = v else { panic!() };
        let inet = c.as_any().downcast_ref::<PgInetValue>().unwrap();
        assert_eq!(inet.0.to_string(), "192.0.2.0/24");
    }

    #[test]
    fn parse_default_rejects_garbage() {
        let lit = toml::Value::String("not-an-inet".into());
        let res = PgInetType.parse_default(&lit);
        assert!(res.is_err());
    }

    #[test]
    fn decode_cursor_value_round_trips_text_envelope() {
        let v = host_v4(10, 1, 2, 3);
        let json = DynValue::to_json(&v).unwrap();
        let decoded = PgInetType.decode_cursor_value(&json).unwrap();
        assert!(v.is_equal(&*decoded));
    }
}
