//! Dialect flag for the shared Postgres connector code.
//!
//! CockroachDB speaks the Postgres wire protocol, so the bulk of the source /
//! sink / storage stack works against both engines unchanged. The handful of
//! places where behaviour must diverge (UPSERT vs `ON CONFLICT`, retry on
//! `40001 RETRY_SERIALIZABLE`, type allow-list) branch on this enum.

use air_elt_core::types::DataType;

/// Identifies which Postgres-compatible engine a connector is talking to.
///
/// `Postgres` is the default and is byte-for-byte the historical behaviour.
/// `Cockroach` opts into CockroachDB-specific code paths.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Dialect {
    #[default]
    Postgres,
    Cockroach,
}

impl Dialect {
    pub fn is_cockroach(self) -> bool {
        matches!(self, Dialect::Cockroach)
    }

    /// Returns `Some(reason)` if the dialect refuses this `DataType`.
    ///
    /// Used by source/sink validation. CockroachDB has no `XML` type, so we
    /// reject it up front rather than failing at first row.
    pub fn excludes_type(self, dt: &DataType) -> Option<&'static str> {
        match self {
            Dialect::Postgres => None,
            Dialect::Cockroach => match dt {
                DataType::Xml => Some("CockroachDB has no XML type"),
                // The `postgresql-hll` extension is a Postgres-only loadable
                // module — CockroachDB has no analogue. Mirror the XML arm
                // and reject up-front rather than failing at the first row.
                DataType::Custom(t) if t.kind() == crate::types::PgHllType::KIND => {
                    Some("CockroachDB has no HLL extension")
                }
                _ => None,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_postgres() {
        assert_eq!(Dialect::default(), Dialect::Postgres);
        assert!(!Dialect::default().is_cockroach());
    }

    #[test]
    fn excludes_hll_only_for_cockroach() {
        use crate::types::PgHllType;
        let hll = DataType::Custom(Box::new(PgHllType));
        assert_eq!(Dialect::Postgres.excludes_type(&hll), None);
        assert!(Dialect::Cockroach.excludes_type(&hll).is_some());
    }

    #[test]
    fn excludes_xml_only_for_cockroach() {
        assert_eq!(Dialect::Postgres.excludes_type(&DataType::Xml), None);
        assert_eq!(Dialect::Postgres.excludes_type(&DataType::Json), None);
        assert!(Dialect::Cockroach.excludes_type(&DataType::Xml).is_some());
        assert_eq!(Dialect::Cockroach.excludes_type(&DataType::Json), None);
        assert_eq!(Dialect::Cockroach.excludes_type(&DataType::Uuid), None);
    }
}
