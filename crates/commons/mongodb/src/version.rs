//! Mongo server version detection.
//!
//! `Client::bulk_write` is only available on MongoDB server 8.0+; on
//! older deployments we fall back to a per-row `replace_one` loop. The
//! sink decides which path to take by inspecting the connected
//! server's `buildInfo.versionArray` once at `connect()` time and
//! caching the answer.

use bson::{Bson, doc};
use mongodb::Client;

use air_elt_core::error::{RuntimeError, RuntimeResult};

/// Major/minor pair from the server's reported version. Patch is
/// dropped — none of the feature-gated paths depend on patch level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MongoVersion {
    pub major: u8,
    pub minor: u8,
}

impl MongoVersion {
    /// `Client::bulk_write` requires server 8.0+. The driver itself
    /// will surface a "command not found" error on older servers — we
    /// pre-check so the sink branches cleanly without paying a failed
    /// round-trip per batch.
    pub fn supports_bulk_write(self) -> bool {
        self.major >= 8
    }
}

/// Probe the connected deployment via `db.runCommand({ buildInfo: 1 })`
/// against the `admin` database. The reply carries `versionArray:
/// [major, minor, patch, …]`, which is the canonical machine-readable
/// form (the human-string `version` field is intentionally avoided —
/// it can carry suffixes like `-rc0`).
pub async fn detect(client: &Client) -> RuntimeResult<MongoVersion> {
    let raw = client
        .database("admin")
        .run_command(doc! { "buildInfo": 1 })
        .await
        .map_err(RuntimeError::backend)?;
    let arr = raw
        .get_array("versionArray")
        .map_err(|e| RuntimeError::Other(format!("buildInfo missing versionArray: {e}")))?;
    if arr.len() < 2 {
        return Err(RuntimeError::Other(format!(
            "buildInfo.versionArray too short: {arr:?}"
        )));
    }
    let component = |b: &Bson| -> Option<u8> {
        match b {
            Bson::Int32(n) => u8::try_from(*n).ok(),
            Bson::Int64(n) => u8::try_from(*n).ok(),
            _ => None,
        }
    };
    let major = component(&arr[0]).ok_or_else(|| {
        RuntimeError::Other(format!(
            "buildInfo.versionArray[0] not numeric: {:?}",
            arr[0]
        ))
    })?;
    let minor = component(&arr[1]).ok_or_else(|| {
        RuntimeError::Other(format!(
            "buildInfo.versionArray[1] not numeric: {:?}",
            arr[1]
        ))
    })?;
    Ok(MongoVersion { major, minor })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bulk_write_gate() {
        assert!(MongoVersion { major: 8, minor: 0 }.supports_bulk_write());
        assert!(MongoVersion { major: 9, minor: 0 }.supports_bulk_write());
        assert!(!MongoVersion { major: 7, minor: 0 }.supports_bulk_write());
        assert!(!MongoVersion { major: 6, minor: 0 }.supports_bulk_write());
    }
}
