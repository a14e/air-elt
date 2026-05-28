//! Generic arena with u16 indexing for compact program layout.
pub mod arena;

pub use arena::{Arena, ArenaOverflow, ArenaRef, ArenaSlice};
