use air_elt_types::DataType;

use crate::expr_type::ExprType;

/// Convert ExprType to the canonical DataType for storage/wire use.
pub fn to_data_type(expr_type: &ExprType) -> DataType {
    match expr_type {
        ExprType::Scalar(dt) => dt.clone(),
        ExprType::Object(_) => DataType::Json,
    }
}
