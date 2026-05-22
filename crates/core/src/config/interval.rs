//! Duration parsing & serde helpers. Implementation lives in
//! `air_elt_commons::interval` so monitoring (which can't depend on
//! core) can reuse the same parser without a workspace cycle. This
//! file is a re-export façade kept for the existing call sites under
//! `core::config::interval::…`.

pub use air_elt_commons::interval::*;
