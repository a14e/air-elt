---
name: expression-language
description: Syntax, type system, and function registry for the Air Elt expression language. Load when working with expressions in configs, adding new expression functions, or debugging expression evaluation.
user-invocable: false
---

# Expression language

Air Elt configs use an expression language for computed column values, defaults, and string interpolation. Expressions are pure (no I/O at evaluation time) and evaluated during the **assemble** phase of the validation pipeline.

## Syntax

Literals: integer (`42`), float (`3.14`), bool (`true`/`false`), null (`null`), double-quoted string with interpolation (`"hello {expr}"`), single-quoted raw string (`'no interpolation'`).

Variables: `x = expr` (assignment via `=`, separated by `;`).

Operators (in precedence order, low to high):
- `||` logical OR
- `&&` logical AND
- `==`, `!=` equality
- `<`, `>`, `<=`, `>=` comparison
- `|` bitwise OR
- `^` bitwise XOR
- `&` bitwise AND
- `<<`, `>>` shift
- `+`, `-` additive
- `*`, `/`, `%` multiplicative
- `!`, `~`, unary `-` (prefix)

Function calls: `name(arg1, arg2, ...)`.
Object literals: `{ "key" = expr, "other" = expr }`.
String interpolation: `"prefix {expr} suffix"`.

## Detection rules

The config loader determines what is an expression vs a plain string:
- String starting with `identifier(` = full expression (parsed and evaluated).
- String containing unescaped `{expr}` = interpolation (each `{...}` segment is parsed).
- `{{` escapes to a literal `{` in interpolation.
- `$$` escapes `$` in `env_expand` (secret resolution layer, runs before expressions).

Config format matters for expression quoting: in YAML, expressions need no outer quotes (YAML handles strings natively). In TOML, outer quotes are required because bare values are typed.

## Type system

`NullableExprType { data_type: DataType, nullable: bool, int_bound: Option<u8> }`

- `int_bound` tracks significant bits (1-64) for integer values. Computed as `64 - value.leading_zeros()` for literals.
- Bounded arithmetic rules: add/subtract = `max(a,b) + 1`, multiply = `a + b`, divide = `a`, modulo = `min(a,b)`.
- When result bits exceed 64, the type promotes to `BigInt { width }`.
- Non-integer types carry `int_bound = None`.
- Materialization picks the smallest `DataType` that fits the bound (Int8/16/32/64 or BigInt).

`Value` implements cross-numeric `PartialEq` and `PartialOrd` via `compare::values_equal` and `compare::compare_values` -- `Int8(5) == Int64(5)` is true, `Int64(3) < BigInt(10)` works. Null==Null returns true for equality. Json/Object use structural equality. Ipv4 and Ipv6 support cross-comparison (v4 maps to v6). NaN comparisons follow IEEE 754 (NaN != NaN).

`Key` newtype (in `crates/types/src/key.rs`) wraps `SmallVec<[Value; 2]>` for switch dispatch, batch dedup, and cursor comparison. Unlike `Value`, `Key` has total ordering (`Ord`) and total equality (`Eq`) -- NaN == NaN by design so keys are deterministic. Construction rejects Null/Json/Object and canonicalises representation (small ints promote to Int64, Float32 widens to Float64).

## Type algebra

Type algebra determines the output `DataType` at type-check time (before evaluation). Rules live in `crates/expr/funcs/src/arithmetic_utils.rs` (arithmetic) and each function's `resolve_type` method.

**Arithmetic promotion** (`bounds::arithmetic_result_type` + `bounds::scalar_arithmetic`):

When both operands carry `int_bound`, bit-level rules apply: add/subtract = `max(a,b)+1` bits, multiply = `a+b` bits, divide = `a` bits, modulo = `min(a,b)` bits. Result stays `Int64` with the computed bound while bits <= 64; above 64 it promotes to `BigInt{width}`. Without `int_bound`, the DataType-level fallback is used:

| Left | Right | Result |
|------|-------|--------|
| IntN | IntN | next wider int (8->16->32->64->BigInt) per bit rules |
| Float32 | Float32 | Float32 |
| Float32/64 | Float64/32 | Float64 |
| any int | any float | Float64 |
| BigInt | BigInt | BigInt (wider width, capped at MAX_BIGINT_WIDTH) |
| any int | BigInt | BigInt |
| Decimal | any numeric | Decimal{precision:None, scale:None} |
| Text | Text | `concat_result_type`: sums sizes when both bounded, else unbounded |

Unary: `negate`/`abs` preserve the input type. `ceil`/`floor`/`round`/`sign` return `Int64`. `power`/`sqrt` return `Float64`.

**String functions:** all return `Text{size:None}` (unbounded). Exception: `length`/`indexOf` return `Int64`; `startsWith`/`endsWith`/`contains` return `Bool`. Size-aware algebra (e.g. `concat(Text(5), Text(5))` returning `Text(10)`) exists in `concat_result_type` but is not wired through `ConcatFunc.resolve_type` yet.

**Comparison functions:** always return `Bool`.

**Cast functions:** return the target type (`toInt64` returns `Int64`, `toBigInt` returns `BigInt{width:None}`, `toDecimal` returns `Decimal{precision:None, scale:None}`, etc.).

**Conditional functions** (parsed as AST nodes, resolved in `type_resolver.rs`):
- `if(cond, then, else)`: returns `then`'s data type; nullable if either branch is nullable.
- `multiIf(c1,v1,...,default)`: returns the first branch's data type; nullable if any branch is nullable.
- `ifNull(value, alt)`: returns `value`'s data type; nullable = `alt.nullable` (the value itself is non-null after the check).
- `nullIf(value, sentinel)`: returns `value`'s data type; always nullable (can produce null).

## Function categories

All functions are registered as static items implementing `ExprFunction`. Brief category summary (full list in [references/functions.md](references/functions.md)):

| Category | Functions |
|---|---|
| Conditional | `if`, `multiIf`, `ifNull`, `nullIf`, `coalesce`, `isNull`, `isNotNull` |
| String | `concat`, `length`, `substring`, `charAt`, `upper`, `lower`, `trim`, `replace`, `startsWith`, `endsWith`, `contains`, `indexOf`, `format`, `toString`, `reverse`, `repeat`, `leftPad`, `rightPad` |
| Arithmetic | `add`, `subtract`, `multiply`, `divide`, `modulo`, `negate`, `abs`, `ceil`, `floor`, `round`, `min`, `max`, `sign`, `power`, `sqrt` |
| Math | trig, hyperbolic, logarithmic, constants, special (`erf`, `gamma`, `lambertW`, `beta`) |
| Comparison | `equals`, `notEquals`, `greater`, `less`, `greaterOrEquals`, `lessOrEquals` |
| Logical | `and`, `or`, `not` |
| Cast | `toStringCast`, `toInt8`..`toInt64`, `toUInt8`..`toUInt64`, `toFloat32`, `toFloat64`, `toBool`, `toDate`, `toTimestamp`, `toUuid`, `toBigInt`, `toDecimal` |
| Datetime | `now`, `today`, extraction (`second`..`year`, `dayOfWeek`, `dayOfYear`), conversion (`toSeconds`, `toMillis`, `fromSeconds`, `fromMillis`), arithmetic (`addDays`..`addYears`, `subtractDays`..`subtractYears`), `dateDiff`, `formatDateTime` |
| Bytes | `byteLength`, `byteAt`, `byteSlice`, `bytesFromHex`, `bytesFromBase64`, `bytesFromUtf8`, `hex`, `base64`, `base64Url`, `bytesEqual`, `urlEncode`, `urlDecode` |
| Encoding | `encode`, `decode`, `encodeText`, `decodeText` |
| Crypto | `md5`, `sha1`, `sha256`, `sha512`, `xxHash64`, `xxHash32`, `cityHash64`, `hmac` |
| JSON | `parseJson`, `toJson`, `jsPath`, `jsPathString`, `jsPathInt`, `jsPathFloat`, `jsPathBool`, `jsonLength` |
| Object | `objectLength`, `objectKeys`, `objectValues`, `objectHasKey`, `objectGet` |
| Regex | `regexMatch`, `regexReplace` |
| Random | `randomUuid`, `randomInt`, `randomFloat`, `randomAlphanumeric`, `randomHex`, `randomBytes`, `randomChoice` |
| Bitwise | `bitAnd`, `bitOr`, `bitXor`, `bitNot`, `bitShiftLeft`, `bitShiftRight`, `bitCount` |
| Env | `env` (1 or 2 args) |
| Misc | `typeof` |

## Crate structure

```
crates/expr/
  types/     NullableExprType, int_bound arithmetic, DataType limits
  funcs/     ExprFunction trait, FunctionRegistry, all builtins, arithmetic_utils
  parse/     Parser struct, AST model (Program, Expr, ...), detection, lexer, token
  runtime/   ExpressionContext, Evaluator, TypeResolver, ConfigExprPatcher
```

- `crates/expr/types` -- type algebra (bounds, promotion rules, conversion).
- `crates/expr/funcs` -- `ExprFunction` trait in `signature.rs`, `FunctionRegistry` in `registry.rs`, builtins in `builtins/` (one file per category), `arithmetic_utils.rs` (bounds helpers).
- `crates/expr/parse` -- `Parser` struct with `create()`, `parse()`, `is_expr()` methods; AST model in `model/` (Program, Expr, ...), `lexer.rs` (tokenizer), `token.rs` (Token enum), detection.
- `crates/expr/runtime` -- `ExpressionContext` (context), `Evaluator` (evaluation), `TypeResolver` (compile-time type resolution), `ConfigExprPatcher` (trie-based TOML patcher).

## Integration points

- `ConfigExprPatcher` walks the config TOML at load time, replacing matched string values with their evaluated `Value` (rendered back into TOML). It calls `Parser::parse` (which internally classifies the string as expression / interpolation template / plain literal) and feeds the resulting `Program` to `Evaluator::evaluate`.
- `ExpressionContext` (from `air_elt_expr_runtime::context`) on `AssembledFlow` holds the compiled program.
- Expression evaluation now uses `Evaluator::create(&context).evaluate(&program)` — no convenience wrappers (`eval_expression`, `eval_interpolated`, `evaluate_interpolated` are all gone). `ExprValue` was deleted entirely.
- `Parser::parse_expression` is the explicit "I already know this is expression source" entry point — used by tests and by internal callers that bypass detection.
- `ConfigExprPatcher` replaces heuristic string detection with path-based TOML matching.
- `ensure_sink_compatible` moved from core to `air_elt_types::sink_compat`.
- `EvalContext` provides `env_resolver`, `file_resolver`, `now` timestamp, and `base_dir`.
- `DynValue` trait methods: `is_equal` (equality), `partial_cmp` (ordering), `hash` (hashing).

## Adding a new function

1. Create a struct implementing `ExprFunction` (in the appropriate `builtins/*.rs` file).
2. Define a `static` item for it.
3. Register it in the module's `register()` function.
4. Implement `name()`, `min_args()`, `max_args()`, `resolve_type()`, `evaluate()`.
5. `resolve_type` validates argument types and returns the output `NullableExprType` (called at compile time).
6. `evaluate` receives owned `Vec<Value>` arguments and returns `Result<Value, FuncError>`.
7. Propagate `Null` -- if any input is null and the function is not null-aware, return `Value::Null`.
8. Add unit tests in the same file's `#[cfg(test)]` module.
9. Update [references/functions.md](references/functions.md) with the new function.
