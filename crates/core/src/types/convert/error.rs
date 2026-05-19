use crate::types::DataType;

#[derive(Debug, thiserror::Error)]
pub enum ConvertError {
    #[error("conversion expected {expected} bytes, got {got}")]
    Length { expected: usize, got: usize },

    #[error("invalid hex digit in input")]
    InvalidHex,

    #[error("input does not parse as a UUID: {reason}")]
    InvalidUuid { reason: String },

    #[error("conversion {src} → {dst} is not supported by the runner")]
    Unsupported { src: DataType, dst: DataType },

    #[error("source value variant does not match declared source DataType {src}")]
    ValueShapeMismatch { src: DataType },

    #[error("conversion {src} → {dst} forbids truncation (would corrupt syntax)")]
    TruncationForbidden { src: DataType, dst: DataType },

    #[error("value overflows target {dst}")]
    Overflow { dst: DataType },

    #[error("input is not well-formed XML: {reason}")]
    InvalidXml { reason: String },

    #[error("string {value:?} is not a recognised boolean literal")]
    InvalidBool { value: String },

    #[error("input does not parse as an {family} address: {reason}")]
    InvalidIp {
        family: &'static str,
        reason: String,
    },

    #[error("IPv6 address {addr} is not IPv4-mapped (::ffff:a.b.c.d) — cannot lower to Ipv4")]
    IpV6NotMappable { addr: String },
}
