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
| Array | Array | `array_element_join` on the element types (see below); `+` concatenates |

Unary: `negate`/`abs` preserve the input type. `ceil`/`floor`/`round`/`sign` return `Int64`. `power`/`sqrt` return `Float64`.

## Array element unification (`array_element_join`)

When two arrays combine (`+`, `concat`), the result element type is computed from the two element types in `crates/expr/funcs/src/arithmetic_utils.rs`:

- a `None` side (the unknown element of `[]`) yields the other side; both `None` → `None`;
- identical element types collapse to themselves;
- otherwise the two must be mutually compatible via `air_elt_types::matrix::is_compatible` (numeric widening, UUID↔text/bytes, …) — the wider source type wins; an incompatible pair is a `TypeMismatch`.

A literal `null` element contributes nullability (`element_nullable`) but not a concrete element type. `[]` carries `element = None` (unknown) and unifies with any concrete element. Polymorphic builtins resolve their array output from the element type: `len`/`isEmpty` → `Int64`/`Bool`; `slice`/`reverse` → the same array type; `element`/`arrayGet`/`indexOf`/`contains` → element type / `Int64` / `Bool`; `filterNotNull` → the same element type with `element_nullable = false`. `len` on a scalar `DataType` is a compile-time `TypeMismatch`.

## Array element nullability

`element_nullable` is a separate axis from the element type. A **nullable** source element into a **non-null** sink element is admitted only under `truncate=true`, which drops the `Null` members at conversion (`is_compatible` rejects it; `is_narrowing` reports it so the validator gives the "enable truncate" hint). A non-null element into a nullable sink is always allowed (widening). `filterNotNull(arr)` is the explicit, lossless alternative — it removes the `Null` members and collapses the element type to non-null, so a nullable-element source feeds a non-null-element sink (`Array(Int32)`, QuestDB `DOUBLE[]`) with no `truncate` flag. PostgreSQL always reports array elements as nullable, so these are the two ways its arrays reach a non-null-element sink.

## Array sinks

The element type is erased to a marker at the sink boundary; only the canonical `DataType::Array` reaches the sink, which is why `element_nullable` lives on `DataType` (so `Array(T)` vs `Array(Nullable(T))` survives).

- **PostgreSQL / CockroachDB**: native primitive-element arrays (`int[]`, `text[]`, …) — read **and** write; nested / non-primitive element arrays are rejected by the type matrix.
- **ClickHouse**: native `Array(T)` and `Array(Nullable(T))` write (RowBinary). A non-empty array with no known element type is rejected.
- **QuestDB**: native `DOUBLE[]` write only (Float64 elements; any other element type rejected).
- **MySQL / MongoDB / non-native columns**: arrays fall back to JSON/Text via the Transform — a `DataType::Array` never reaches a MySQL sink. Exception: the **Mongo** sink writes a native `Bson::Array` (each element encoded canonically).
- Arrays are **never** cursor / switch / dedup / conflict keys (`Key::from_value` rejects `Value::Array`).

## String functions

All return `Text{size:None}` (unbounded). Exceptions: `indexOf` returns `Int64`; `startsWith`/`endsWith`/`contains` return `Bool`; `split` returns `Array<Text>`. Size-aware algebra (e.g. `concat(Text(5), Text(5))` returning `Text(10)`) exists in `concat_result_type` but is not wired through `ConcatFunc.resolve_type` yet. (`len` is in the Array category — see below.)

**Strictness — no implicit string coercion.** String functions reject non-text arguments with a `TypeMismatch`; they do **not** stringify silently. `trim(1)`, `concat(x, 5)` are type errors. `concat` is strict in both `resolve_type` and `evaluate` (text args only; null still propagates to `Null`). To turn a non-text value into a string, call `toString` explicitly. String interpolation (`"{expr}"`) is the one stringify-everywhere context: each segment renders via the canonical `value_to_string` regardless of type — it does not route through `concat`. A consequence the optimizer exploits: `concat(x, "")` / `concat(x)` is a pure string type-check on `x`, lowered to a `TypeAssert{String}`.

## Comparison functions

Always return **non-null `Bool`** — all six are total. `==`/`!=` treat null as a value (`null==null` → true, `null==x` → false), so `x==null` is a real null test that matches `values_equal`/`Key`; the ordering operators (`<`/`>`/`<=`/`>=`) return `false` on any null operand (null is unordered, mirroring SQL filtering and deliberately unlike `==`). Because they never produce null, comparisons do not propagate operand nullability into `&&`/`||`/`if`.

## Typed optimizer identities (the `typed/` rewrite pass)

The typed pass folds shapes a static type makes sound. Beyond the existing power reduction (`x ** 1 → x`, `x ** 0 → 1.0`), `min`/`max` saturation, and `x*1`/`x+0`/`x-x` identities:

- **Self-comparison `x ⋈ x`** (`typed/self_compare.rs`). Both operands the same pure, infallible expression. `x > x` / `x < x` → **`false`** for every operand (`NaN > NaN` is false; ordering returns false on null). `x == x` → **`true`**, `x != x` → **`false`** for **non-float** operands (sound even when nullable — `null == null` is true). `x >= x` / `x <= x` → **`true`** for **non-float, non-null** operands (they return false on null). Floats are skipped entirely: `x == x` is the canonical `NaN` test and must keep its meaning. The purity gate is load-bearing — `random() > random()` is not always false.
- **`isNaN(x) → false`** for any integer/`BigInt`; **`isInfinite(x) → false`** for a **fixed-width** integer only (a large `BigInt` overflows `f64` to infinity). Both drop the operand, so it must be non-null + infallible + pure (`isNaN(null)` is null, not false).
- **`abs(x) → x`** for an unsigned-integer operand (always non-negative; `abs` preserves type, so the strip is type-preserving).

Constant operands never reach these rules — `sqrt(1)`, `equals(c, c)`, `abs(1)` already const-fold in the untyped pass.

## Cast functions

Return the target type (`toInt64` returns `Int64`, `toBigInt` returns `BigInt{width:None}`, `toDecimal` returns `Decimal{precision:None, scale:None}`, etc.).

## Conditional functions (parsed as AST nodes, resolved in `type_resolver.rs`)

- `if(cond, then, else)`: returns `then`'s data type; nullable if either branch is nullable. An `if`/`else if` chain (`if(c1, v1, if(c2, v2, …, default))`) folds at parse time into a flat `multiIf` — identical meaning, parsed/evaluated iteratively, so a long ladder costs no nesting depth. Use a flat `multiIf` (or a chain) rather than deep nesting for many cases (`MAX_AST_NODES`, not `MAX_EXPR_DEPTH`, is the bound — thousands of branches are fine).
- `multiIf(c1,v1,...,default)`: returns the first branch's data type; nullable if any branch is nullable. A large equality `multiIf` over one or two pure keys lowers to an O(1) `Switch` (see the optimizer).
- `ifNull(value, alt)`: returns `value`'s data type; nullable = `alt.nullable` (the value itself is non-null after the check).
- `nullIf(value, sentinel)`: returns `value`'s data type; always nullable (can produce null).
