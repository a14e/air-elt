# Type algebra (full reference)

Type algebra fixes the output `DataType` at type-check time (before evaluation). Rules live in `crates/expr/funcs/src/arithmetic_utils.rs` (arithmetic) and each function's `resolve_type` method. The SKILL body carries the load-bearing principles; this file is the exhaustive table.

## Arithmetic promotion (`bounds::arithmetic_result_type` + `bounds::scalar_arithmetic`)

When both operands carry `int_bound`, bit-level rules apply: add/subtract = `max(a,b)+1` bits, multiply = `a+b` bits, divide = `a` bits, modulo = `min(a,b)` bits. Result stays `Int64` with the computed bound while bits ≤ 64; above 64 it promotes to `BigInt{width}`. Without `int_bound`, the DataType-level fallback is used:

| Left | Right | Result |
|------|-------|--------|
| IntN | IntN | next wider int (8→16→32→64→BigInt) per bit rules |
| Float32 | Float32 | Float32 |
| Float32/64 | Float64/32 | Float64 |
| any int | any float | Float64 |
| BigInt | BigInt | BigInt (wider width, capped at MAX_BIGINT_WIDTH) |
| any int | BigInt | BigInt |
| Decimal | any numeric | Decimal{precision:None, scale:None} |
| Text | Text | `concat_result_type`: sums sizes when both bounded, else unbounded |

Unary: `negate`/`abs` preserve the input type. `ceil`/`floor`/`round`/`sign` return `Int64`. `power`/`sqrt` return `Float64`.

## String functions

All return `Text{size:None}` (unbounded). Exceptions: `length`/`indexOf` return `Int64`; `startsWith`/`endsWith`/`contains` return `Bool`. Size-aware algebra (e.g. `concat(Text(5), Text(5))` returning `Text(10)`) exists in `concat_result_type` but is not wired through `ConcatFunc.resolve_type` yet.

**Strictness — no implicit string coercion.** String functions reject non-text arguments with a `TypeMismatch`; they do **not** stringify silently. `trim(1)`, `concat(x, 5)` are type errors. `concat` is strict in both `resolve_type` and `evaluate` (text args only; null still propagates to `Null`). To turn a non-text value into a string, call `toString` explicitly. String interpolation (`"{expr}"`) is the one stringify-everywhere context: each segment renders via the canonical `value_to_string` regardless of type — it does not route through `concat`. A consequence the optimizer exploits: `concat(x, "")` / `concat(x)` is a pure string type-check on `x`, lowered to a `TypeAssert{String}`.

## Comparison functions

Always return **non-null `Bool`** — all six are total. `==`/`!=` treat null as a value (`null==null` → true, `null==x` → false), so `x==null` is a real null test that matches `values_equal`/`Key`; the ordering operators (`<`/`>`/`<=`/`>=`) return `false` on any null operand (null is unordered, mirroring SQL filtering and deliberately unlike `==`). Because they never produce null, comparisons do not propagate operand nullability into `&&`/`||`/`if`.

## Cast functions

Return the target type (`toInt64` returns `Int64`, `toBigInt` returns `BigInt{width:None}`, `toDecimal` returns `Decimal{precision:None, scale:None}`, etc.).

## Conditional functions (parsed as AST nodes, resolved in `type_resolver.rs`)

- `if(cond, then, else)`: returns `then`'s data type; nullable if either branch is nullable. An `if`/`else if` chain (`if(c1, v1, if(c2, v2, …, default))`) folds at parse time into a flat `multiIf` — identical meaning, parsed/evaluated iteratively, so a long ladder costs no nesting depth. Use a flat `multiIf` (or a chain) rather than deep nesting for many cases (`MAX_AST_NODES`, not `MAX_EXPR_DEPTH`, is the bound — thousands of branches are fine).
- `multiIf(c1,v1,...,default)`: returns the first branch's data type; nullable if any branch is nullable. A large equality `multiIf` over one or two pure keys lowers to an O(1) `Switch` (see the optimizer).
- `ifNull(value, alt)`: returns `value`'s data type; nullable = `alt.nullable` (the value itself is non-null after the check).
- `nullIf(value, sentinel)`: returns `value`'s data type; always nullable (can produce null).
