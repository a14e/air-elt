//! Per-flow validation knobs.
//!
//! The `[flow.<name>.validation]` block toggles individual validation
//! checks. Today only `sampling` is exposed. The `sampling` knob accepts
//! two equivalent shapes:
//!
//! ```toml
//! validation = { sampling = true }
//! # or
//! validation = { sampling = { enabled = true, size = 100 } }
//! ```
//!
//! When the knob is omitted entirely, the pipeline asks the source's
//! factory for the per-backend default (off for SQL, on for MongoDB).
//! That fallback is applied in `validation::pipeline::validate`, not in
//! the parser — at parse time we only record "the operator did not say
//! anything", which the runtime then reads via `SamplingConfig::Unset`.

use serde::de::{Deserializer, MapAccess, Visitor};
use serde::{Deserialize, Serialize};

pub const DEFAULT_SAMPLING_SIZE: usize = 100;

/// Top-level optional validation block on `[flow.<name>]`. Empty means
/// "operator did not configure validation"; the pipeline applies
/// per-backend defaults in that case.
///
/// `access`, `fields`, `inserts` default to `true` — every check runs
/// unless the operator opts out. `sampling` follows the per-backend
/// default unless overridden (Mongo on, SQL off).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ValidationConfig {
    /// Source/sink/storage `validate_access` probes (ping, schema visibility).
    #[serde(default = "default_true")]
    pub access: bool,
    /// Gates schema introspection (`describe_schema` on source and
    /// sink), `check_cursor`, and `check_mapping`. With `fields = false`
    /// no introspection runs: the validator builds identity passthrough
    /// `ColumnConversionPlan`s (`Json → Json`) directly from the mapping, so
    /// the runner ships values through untouched. `truncate` becomes a
    /// no-op (passthrough doesn't narrow); `default` is rejected
    /// (`DefaultRequiresFields`) because parsing a default literal
    /// needs the real sink type. This is the only knob that lets a
    /// flow validate against an empty Mongo collection — operators
    /// provisioning a flow before any writer exists need it.
    #[serde(default = "default_true")]
    pub fields: bool,
    /// Sink write probe (insert + delete sentinel) at `validate_access`
    /// time. Today this is folded into the sink's own `validate_access`,
    /// so disabling `inserts` skips the sink probe entirely while
    /// leaving source/storage probes intact.
    #[serde(default = "default_true")]
    pub inserts: bool,
    #[serde(default)]
    pub sampling: SamplingConfig,
}

impl Default for ValidationConfig {
    fn default() -> Self {
        Self {
            access: true,
            fields: true,
            inserts: true,
            sampling: SamplingConfig::default(),
        }
    }
}

fn default_true() -> bool {
    true
}

/// Sampling-validation knob. `Unset` means "fall back to the source
/// factory default"; `Disabled` and `Enabled` are explicit operator
/// choices and are honoured even if the backend default is the
/// opposite.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum SamplingConfig {
    #[default]
    Unset,
    Disabled,
    Enabled {
        size: usize,
    },
}

impl SamplingConfig {
    pub fn enabled_default() -> Self {
        Self::Enabled {
            size: DEFAULT_SAMPLING_SIZE,
        }
    }

    pub fn resolve(self, fallback: SamplingConfig) -> SamplingConfig {
        match self {
            SamplingConfig::Unset => fallback,
            other => other,
        }
    }

    pub fn size(self) -> Option<usize> {
        match self {
            SamplingConfig::Enabled { size } => Some(size),
            _ => None,
        }
    }
}

impl<'de> Deserialize<'de> for SamplingConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(SamplingVisitor)
    }
}

struct SamplingVisitor;

impl<'de> Visitor<'de> for SamplingVisitor {
    type Value = SamplingConfig;

    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("a bool, or a table { enabled: bool, size?: usize }")
    }

    fn visit_bool<E>(self, v: bool) -> Result<SamplingConfig, E>
    where
        E: serde::de::Error,
    {
        Ok(if v {
            SamplingConfig::enabled_default()
        } else {
            SamplingConfig::Disabled
        })
    }

    fn visit_map<A>(self, mut map: A) -> Result<SamplingConfig, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut enabled: Option<bool> = None;
        let mut size: Option<usize> = None;
        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "enabled" => {
                    if enabled.is_some() {
                        return Err(serde::de::Error::duplicate_field("enabled"));
                    }
                    enabled = Some(map.next_value()?);
                }
                "size" => {
                    if size.is_some() {
                        return Err(serde::de::Error::duplicate_field("size"));
                    }
                    let raw: usize = map.next_value()?;
                    if raw == 0 {
                        return Err(serde::de::Error::custom(
                            "validation.sampling.size must be >= 1",
                        ));
                    }
                    size = Some(raw);
                }
                other => {
                    return Err(serde::de::Error::unknown_field(other, &["enabled", "size"]));
                }
            }
        }
        let enabled = enabled.ok_or_else(|| serde::de::Error::missing_field("enabled"))?;
        Ok(if enabled {
            SamplingConfig::Enabled {
                size: size.unwrap_or(DEFAULT_SAMPLING_SIZE),
            }
        } else {
            if size.is_some() {
                return Err(serde::de::Error::custom(
                    "validation.sampling.size is meaningless when enabled = false",
                ));
            }
            SamplingConfig::Disabled
        })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[derive(Debug, Deserialize)]
    struct Wrapper {
        #[serde(default)]
        validation: ValidationConfig,
    }

    fn parse(toml_str: &str) -> Wrapper {
        toml::from_str(toml_str).unwrap()
    }

    #[test]
    fn default_is_unset() {
        let cfg = parse("");
        assert_eq!(cfg.validation.sampling, SamplingConfig::Unset);
    }

    #[test]
    fn bool_form_true() {
        let cfg = parse("validation = { sampling = true }");
        assert_eq!(
            cfg.validation.sampling,
            SamplingConfig::Enabled { size: 100 }
        );
    }

    #[test]
    fn bool_form_false() {
        let cfg = parse("validation = { sampling = false }");
        assert_eq!(cfg.validation.sampling, SamplingConfig::Disabled);
    }

    #[test]
    fn table_form_default_size() {
        let cfg = parse("[validation.sampling]\nenabled = true");
        assert_eq!(
            cfg.validation.sampling,
            SamplingConfig::Enabled { size: 100 }
        );
    }

    #[test]
    fn table_form_custom_size() {
        let cfg = parse("[validation.sampling]\nenabled = true\nsize = 250");
        assert_eq!(
            cfg.validation.sampling,
            SamplingConfig::Enabled { size: 250 }
        );
    }

    #[test]
    fn size_with_disabled_is_rejected() {
        let r: Result<Wrapper, _> =
            toml::from_str("[validation.sampling]\nenabled = false\nsize = 50");
        assert!(r.is_err());
    }

    #[test]
    fn zero_size_rejected() {
        let r: Result<Wrapper, _> =
            toml::from_str("[validation.sampling]\nenabled = true\nsize = 0");
        assert!(r.is_err());
    }

    #[test]
    fn unknown_field_rejected() {
        let r: Result<Wrapper, _> = toml::from_str("validation = { sampling = true, foo = 1 }");
        assert!(r.is_err());
    }

    #[test]
    fn flags_default_true() {
        let cfg = parse("");
        assert!(cfg.validation.access);
        assert!(cfg.validation.fields);
        assert!(cfg.validation.inserts);
    }

    #[test]
    fn flags_can_be_disabled_individually() {
        let cfg = parse(
            r#"
            [validation]
            access = false
            fields = false
            inserts = false
        "#,
        );
        assert!(!cfg.validation.access);
        assert!(!cfg.validation.fields);
        assert!(!cfg.validation.inserts);
    }

    #[test]
    fn resolve_keeps_explicit_choice() {
        assert_eq!(
            SamplingConfig::Disabled.resolve(SamplingConfig::enabled_default()),
            SamplingConfig::Disabled
        );
        assert_eq!(
            SamplingConfig::Enabled { size: 7 }.resolve(SamplingConfig::Disabled),
            SamplingConfig::Enabled { size: 7 }
        );
        assert_eq!(
            SamplingConfig::Unset.resolve(SamplingConfig::enabled_default()),
            SamplingConfig::Enabled { size: 100 }
        );
    }
}
