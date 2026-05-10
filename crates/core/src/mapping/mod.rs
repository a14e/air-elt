pub mod column;
pub mod expand;
pub mod path;
pub mod shorthand;

pub use column::{ColumnMapping, build};
pub use expand::{
    Body, DirectMapping, ExpandedMapping, ROOT_BODY_TARGET, WILDCARD_UNIVERSE_CAP, expand,
};
pub use path::{FieldPath, FieldPathError};
