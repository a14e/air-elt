pub mod column;
pub mod expand;
pub mod path;

pub use column::{ColumnMapping, SwitchCase, build};
pub use expand::{
    Body, DirectMapping, ExpandedMapping, ROOT_BODY_TARGET, SwitchSpec, WILDCARD_UNIVERSE_CAP,
    expand,
};
pub use path::{FieldPath, FieldPathError};
