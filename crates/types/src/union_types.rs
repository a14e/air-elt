//! Union-type folding for switch / sampling / schemaless-derivation
//! paths.
//!
//! A multiset of observed `DataType`s collapses into a single "wider"
//! type whenever the matrix's widening rules permit; otherwise the
//! result is a [`DataType::Union`] of the unique observed members.
//!
//! Used today by the switch lowering for schemaless sinks (Mongo) —
//! the sink column type is derived from observed RHS literal types
//! rather than declared upfront. The function lives in its own module
//! so future inference paths (sampling-derived schemas, multi-source
//! flows) can reach for the same primitive without depending on
//! `matrix`'s compatibility internals.

use crate::data_type::DataType;

/// Collapse a multiset of observed `DataType`s into a single "wider"
/// `DataType` when widening rules permit; otherwise return
/// `DataType::Union(unique members)`.
///
/// Rules mirror [`crate::matrix::is_compatible`]:
/// * Int8..Int64 widen to the max width seen.
/// * UInt8..UInt64 widen to the max width seen.
/// * Float32 + Float64 → Float64.
/// * Text(a) + Text(b) → Text(max(a, b)) (unbounded wins).
/// * Bytes(a) + Bytes(b) → Bytes(max(a, b)).
/// * Nested `Union(inner)` inputs are flattened — inner members are
///   treated as if they had been emitted directly by the iterator.
/// * Any heterogeneous mix that does not match the above → `Union(...)`.
///
/// The hot path (single observed type, or a homogeneous family that
/// widens) is zero-alloc: no `Vec` is allocated unless the input is
/// genuinely heterogeneous and we have to fall back to a `Union`
/// outcome. The empty-iterator case preserves the pathological
/// `DataType::Union(Vec::new())` result of the previous implementation.
pub fn collapse_union<I>(types: I) -> DataType
where
    I: IntoIterator<Item = DataType>,
{
    let mut leaves = types.into_iter().flat_map(into_leaves);
    let Some(mut acc) = leaves.next() else {
        return DataType::Union(Vec::new());
    };
    loop {
        let Some(next) = leaves.next() else {
            return acc;
        };
        match try_widen(acc, next) {
            Ok(widened) => acc = widened,
            Err((acc_back, offending)) => return fallback(acc_back, offending, leaves),
        }
    }
}

/// Allocate the `Union` fallback vector and drain whatever remains.
///
/// Constructs `DataType::Union(...)` directly (bypassing
/// [`DataType::union`]) to avoid re-entering `collapse_union` — which
/// is what `DataType::union` itself dispatches to. The result is
/// sorted and dedup'd here to preserve `DataType::union`'s
/// observation-order-independent equality guarantee, and a 1-element
/// collapse is honoured for the case where two heterogeneous-kind
/// inputs reduce to a single variant after dedup.
///
/// After sort+dedup the same-kind families become adjacent (derived
/// `Ord` groups discriminants together), so a single pairwise pass
/// finishes any widening the left-to-right outer loop missed because
/// of input ordering. Example: `[Int16, UInt8, Int8]` hits fallback at
/// `Int16 + UInt8` with `[Int8]` still pending; without the pass the
/// result would be `Union([Int8, Int16, UInt8])` which still contains
/// `Int8 + Int16` — exactly the situation widening rules are supposed
/// to collapse. The pairwise pass turns that into `Union([Int16, UInt8])`,
/// making `collapse_union` idempotent on the first re-feed.
fn fallback<I>(acc: DataType, offending: DataType, rest: I) -> DataType
where
    I: IntoIterator<Item = DataType>,
{
    let mut collected: Vec<DataType> = std::iter::once(acc)
        .chain(std::iter::once(offending))
        .chain(rest)
        .flat_map(into_leaves)
        .collect();
    // Why: derived `Ord` is allocation-free (lexicographic on the
    // discriminant + fields), unlike a `format!`-based sort key.
    collected.sort();
    collected.dedup();

    // Walk the sorted set as a stack: try to widen the top against
    // each incoming element. Bounded by `collected.len()` — every
    // successful widen drops one element, every failure pushes both
    // back and moves on. Termination is trivially `O(n)`.
    let mut reduced: Vec<DataType> = Vec::with_capacity(collected.len());
    for next in collected {
        match reduced.pop() {
            None => reduced.push(next),
            Some(top) => match try_widen(top, next) {
                Ok(widened) => reduced.push(widened),
                Err((top_back, next_back)) => {
                    reduced.push(top_back);
                    reduced.push(next_back);
                }
            },
        }
    }

    if reduced.len() == 1 {
        return reduced.into_iter().next().expect("len==1");
    }
    DataType::Union(reduced)
}

/// Flatten one level of `Union(inner)` into a leaf iterator. Returns
/// either the original element wrapped in `Once`, or the inner Vec's
/// own `IntoIter`. Two-arm enum (not `Box<dyn Iterator>`) keeps the
/// hot path allocation-free.
fn into_leaves(t: DataType) -> LeafIter {
    match t {
        DataType::Union(inner) => LeafIter::Inner(inner.into_iter()),
        other => LeafIter::One(std::iter::once(other)),
    }
}

enum LeafIter {
    One(std::iter::Once<DataType>),
    Inner(std::vec::IntoIter<DataType>),
}

impl Iterator for LeafIter {
    type Item = DataType;
    fn next(&mut self) -> Option<DataType> {
        match self {
            LeafIter::One(it) => it.next(),
            LeafIter::Inner(it) => it.next(),
        }
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        match self {
            LeafIter::One(it) => it.size_hint(),
            LeafIter::Inner(it) => it.size_hint(),
        }
    }
}

/// Attempt to widen `acc` by absorbing `incoming`.
///
/// Returns `Ok(widened)` when the widening rules apply (same kind
/// family or identical types). Returns `Err((acc, incoming))` when
/// the kinds are heterogeneous, handing both values back so the
/// caller can fall back to the union-packing branch without
/// re-evaluating the rules.
///
/// `Union(inner)` on either side is NOT handled here — flattening is
/// the caller's job via `into_leaves`. By the time `try_widen` runs
/// both sides are leaves of the outer iterator.
fn try_widen(acc: DataType, incoming: DataType) -> Result<DataType, (DataType, DataType)> {
    use DataType::*;

    if acc == incoming {
        return Ok(acc);
    }

    match (&acc, &incoming) {
        (Int8 | Int16 | Int32 | Int64, Int8 | Int16 | Int32 | Int64) => {
            let widened = widen_int(int_width(&acc).max(int_width(&incoming)));
            Ok(widened)
        }
        (UInt8 | UInt16 | UInt32 | UInt64, UInt8 | UInt16 | UInt32 | UInt64) => {
            let widened = widen_uint(uint_width(&acc).max(uint_width(&incoming)));
            Ok(widened)
        }
        (Float32 | Float64, Float32 | Float64) => {
            if matches!(acc, Float64) || matches!(incoming, Float64) {
                Ok(Float64)
            } else {
                Ok(Float32)
            }
        }
        (Text { size: a }, Text { size: b }) => {
            let size = match (a, b) {
                (None, _) | (_, None) => None,
                (Some(x), Some(y)) => Some(*x.max(y)),
            };
            Ok(Text { size })
        }
        (Bytes { size: a }, Bytes { size: b }) => {
            let size = match (a, b) {
                (None, _) | (_, None) => None,
                (Some(x), Some(y)) => Some(*x.max(y)),
            };
            Ok(Bytes { size })
        }
        _ => Err((acc, incoming)),
    }
}

fn widen_int(width: u8) -> DataType {
    match width {
        1 => DataType::Int8,
        2 => DataType::Int16,
        4 => DataType::Int32,
        _ => DataType::Int64,
    }
}

fn widen_uint(width: u8) -> DataType {
    match width {
        1 => DataType::UInt8,
        2 => DataType::UInt16,
        4 => DataType::UInt32,
        _ => DataType::UInt64,
    }
}

fn int_width(t: &DataType) -> u8 {
    match t {
        DataType::Int8 => 1,
        DataType::Int16 => 2,
        DataType::Int32 => 4,
        DataType::Int64 => 8,
        _ => 0,
    }
}

fn uint_width(t: &DataType) -> u8 {
    match t {
        DataType::UInt8 => 1,
        DataType::UInt16 => 2,
        DataType::UInt32 => 4,
        DataType::UInt64 => 8,
        _ => 0,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn single_type_returns_itself() {
        assert_eq!(collapse_union(vec![DataType::Int32]), DataType::Int32);
    }

    #[test]
    fn int_family_widens_to_max() {
        let r = collapse_union(vec![DataType::Int8, DataType::Int16, DataType::Int32]);
        assert_eq!(r, DataType::Int32);
    }

    #[test]
    fn uint_family_widens_to_max() {
        let r = collapse_union(vec![DataType::UInt8, DataType::UInt32]);
        assert_eq!(r, DataType::UInt32);
    }

    #[test]
    fn float_family_widens_to_f64_when_any_f64() {
        let r = collapse_union(vec![DataType::Float32, DataType::Float64]);
        assert_eq!(r, DataType::Float64);
    }

    /// Same-family same-width preservation: two `Float32`s must NOT
    /// promote to `Float64`. Without this assertion the `assert ==
    /// Float64` test above would have left a regression hole — a
    /// faulty `try_widen` that always returned `Float64` for any
    /// float pair would pass.
    #[test]
    fn float_family_preserves_f32_when_no_f64() {
        let r = collapse_union(vec![DataType::Float32, DataType::Float32]);
        assert_eq!(r, DataType::Float32);
    }

    /// Empty input is degenerate but legal — pin the shape so a future
    /// "return None on empty" refactor flags as a behaviour change.
    #[test]
    fn empty_input_returns_empty_union() {
        let r = collapse_union(Vec::<DataType>::new());
        assert_eq!(r, DataType::Union(Vec::new()));
    }

    /// Bytes widening mirrors Text: max declared size wins, unbounded
    /// dominates. Symmetric to the Text test directly below; both
    /// exercise the `Bytes`/`Text` arms of `try_widen`.
    #[test]
    fn bytes_family_takes_max_size_or_unbounded() {
        let r = collapse_union(vec![
            DataType::Bytes { size: Some(5) },
            DataType::Bytes { size: Some(9) },
        ]);
        assert_eq!(r, DataType::Bytes { size: Some(9) });
        let r = collapse_union(vec![
            DataType::Bytes { size: Some(5) },
            DataType::Bytes { size: None },
        ]);
        assert_eq!(r, DataType::Bytes { size: None });
    }

    #[test]
    fn text_family_takes_max_size_or_unbounded() {
        let r = collapse_union(vec![
            DataType::Text { size: Some(5) },
            DataType::Text { size: Some(9) },
        ]);
        assert_eq!(r, DataType::Text { size: Some(9) });
        let r = collapse_union(vec![
            DataType::Text { size: Some(5) },
            DataType::Text { size: None },
        ]);
        assert_eq!(r, DataType::Text { size: None });
    }

    #[test]
    fn heterogeneous_kinds_yield_union() {
        let r = collapse_union(vec![DataType::Int32, DataType::Text { size: None }]);
        assert!(matches!(r, DataType::Union(_)));
    }

    #[test]
    fn nested_union_flattens() {
        let r = collapse_union(vec![
            DataType::Union(vec![DataType::Int8, DataType::Int16]),
            DataType::Int32,
        ]);
        assert_eq!(r, DataType::Int32);
    }

    // ---- Property-based tests --------------------------------------

    use proptest::prelude::*;

    /// Strategy yielding any of the four signed integer kinds along
    /// with the bit width that distinguishes them.
    fn signed_int_with_width() -> impl Strategy<Value = (DataType, u32)> {
        prop_oneof![
            Just((DataType::Int8, 8u32)),
            Just((DataType::Int16, 16)),
            Just((DataType::Int32, 32)),
            Just((DataType::Int64, 64)),
        ]
    }

    /// Map a numeric width back to the matching signed `DataType` arm.
    fn signed_for_width(w: u32) -> DataType {
        match w {
            8 => DataType::Int8,
            16 => DataType::Int16,
            32 => DataType::Int32,
            _ => DataType::Int64,
        }
    }

    #[test_strategy::proptest(ProptestConfig::with_cases(128))]
    fn collapse_int_family_yields_max_width(
        #[strategy(prop::collection::vec(signed_int_with_width(), 1..=8))] bag: Vec<(
            DataType,
            u32,
        )>,
    ) {
        let max_width = bag.iter().map(|(_, w)| *w).max().unwrap();
        let types: Vec<DataType> = bag.iter().map(|(t, _)| t.clone()).collect();
        let collapsed = collapse_union(types);
        prop_assert_eq!(collapsed, signed_for_width(max_width));
    }

    /// Strategy yielding mixed inputs across kinds, used to test
    /// idempotence of `collapse_union` regardless of the outcome shape.
    fn any_collapse_input() -> impl Strategy<Value = DataType> {
        prop_oneof![
            Just(DataType::Int8),
            Just(DataType::Int16),
            Just(DataType::Int32),
            Just(DataType::Int64),
            Just(DataType::UInt8),
            Just(DataType::UInt16),
            Just(DataType::UInt32),
            Just(DataType::UInt64),
            Just(DataType::Float32),
            Just(DataType::Float64),
            Just(DataType::Bool),
            prop::option::of(1u32..=64).prop_map(|sz| DataType::Text { size: sz }),
            prop::option::of(1u32..=64).prop_map(|sz| DataType::Bytes { size: sz }),
        ]
    }

    /// `collapse_union` is idempotent: re-feeding its own result returns
    /// the same value. The fallback path's post-sort pairwise widening
    /// pass guarantees that even pathological input orderings (e.g.
    /// `[Int16, UInt8, Int8]`) reach a fixed point on the first pass.
    #[test_strategy::proptest(ProptestConfig::with_cases(128))]
    fn collapse_idempotent(
        #[strategy(prop::collection::vec(any_collapse_input(), 1..=6))] xs: Vec<DataType>,
    ) {
        let once = collapse_union(xs.clone());
        let twice = collapse_union(vec![once.clone()]);
        prop_assert_eq!(once, twice);
    }

    /// Regression for the `fallback` widening-order bug: `[Int16, UInt8, Int8]`
    /// hits the heterogeneous arm at `Int16 + UInt8` with `Int8` still
    /// pending, but the post-sort pairwise pass must still absorb
    /// `Int8 + Int16 → Int16` before union-packing.
    #[test]
    fn fallback_reabsorbs_same_kind_after_sort() {
        let r = collapse_union(vec![DataType::Int16, DataType::UInt8, DataType::Int8]);
        assert_eq!(r, DataType::Union(vec![DataType::Int16, DataType::UInt8]));
    }
}
