use air_elt_types::DataType;

/// Expression type with nullability and optional integer width bound.
/// `int_bound` tracks significant bits for integers (via leading_zeros).
/// This allows precise arithmetic: `1 * 1 * 1` stays small instead of
/// exploding to BigInt.
#[derive(Debug, Clone, PartialEq)]
pub struct NullableExprType {
    pub data_type: DataType,
    pub nullable: bool,
    /// For integer types: number of significant bits (1-64).
    /// Computed via `64 - value.leading_zeros()` for literals.
    /// Arithmetic: add -> max(a,b)+1, multiply -> a+b, divide -> a, modulo -> min(a,b).
    /// When > 64 -> promotes to BigInt.
    /// None for non-integer types.
    pub int_bound: Option<u8>,
}

impl NullableExprType {
    pub fn new(data_type: DataType, nullable: bool) -> Self {
        Self {
            data_type,
            nullable,
            int_bound: None,
        }
    }

    pub fn non_null(data_type: DataType) -> Self {
        Self {
            data_type,
            nullable: false,
            int_bound: None,
        }
    }

    pub fn nullable(data_type: DataType) -> Self {
        Self {
            data_type,
            nullable: true,
            int_bound: None,
        }
    }

    pub fn int_with_bound(data_type: DataType, bound: u8) -> Self {
        Self {
            data_type,
            nullable: false,
            int_bound: Some(bound),
        }
    }

    /// Build an array type. `element` is the (optional) element data type —
    /// `None` for an empty/unknown element (`[]`) — and `element_nullable`
    /// records whether elements may be `Null`. Both live inside
    /// [`DataType::Array`], so they survive materialization (the element
    /// type is erased only at the expr/`int_bound` layer, never on the
    /// `DataType`). `nullable` is whether the array value itself may be null.
    pub fn array(element: Option<DataType>, element_nullable: bool, nullable: bool) -> Self {
        Self {
            data_type: DataType::Array {
                element: element.map(Box::new),
                element_nullable,
            },
            nullable,
            int_bound: None,
        }
    }

    pub fn display_name(&self) -> String {
        let name = format!("{}", self.data_type);
        if self.nullable {
            format!("Nullable({name})")
        } else {
            name
        }
    }

    /// Materialize the int_bound into the smallest fitting DataType.
    pub fn materialized_data_type(&self) -> DataType {
        match self.int_bound {
            Some(bits) if bits <= 8 => DataType::Int8,
            Some(bits) if bits <= 16 => DataType::Int16,
            Some(bits) if bits <= 32 => DataType::Int32,
            Some(bits) if bits <= 64 => DataType::Int64,
            Some(bits) => DataType::BigInt {
                width: Some(((bits as f64) * std::f64::consts::LOG10_2).ceil() as u32 + 1),
            },
            None => self.data_type.clone(),
        }
    }
}

impl From<DataType> for NullableExprType {
    fn from(dt: DataType) -> Self {
        Self::non_null(dt)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn array_constructor_carries_element_in_data_type() {
        let t = NullableExprType::array(Some(DataType::Int64), true, false);
        assert!(!t.nullable);
        assert_eq!(t.int_bound, None);
        assert_eq!(
            t.data_type,
            DataType::Array {
                element: Some(Box::new(DataType::Int64)),
                element_nullable: true,
            }
        );
    }

    #[test]
    fn array_materializes_to_itself_element_preserved() {
        // `int_bound` is None for arrays, so materialization clones the
        // `DataType::Array` — the element type survives to the sink.
        let t = NullableExprType::array(Some(DataType::Float64), false, false);
        assert_eq!(t.materialized_data_type(), t.data_type);
    }

    #[test]
    fn empty_array_has_no_element() {
        let t = NullableExprType::array(None, false, false);
        assert_eq!(
            t.data_type,
            DataType::Array {
                element: None,
                element_nullable: false,
            }
        );
    }
}
