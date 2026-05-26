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
| Random | `randomUuid`, `randomInt`, `randomFloat`, `randomAlphanumeric`, `randomHex`, `randomBytes`, `randomChoice` |
| Bitwise | `bitAnd`, `bitOr`, `bitXor`, `bitNot`, `bitShiftLeft`, `bitShiftRight`, `bitCount` |
| Env | `env` (1 or 2 args) |
| Misc | `typeof` |

## Crate structure

```
crates/expr/
  types/   NullableExprType, int_bound arithmetic, DataType limits
  funcs/   ExprFunction trait, FunctionRegistry, all builtins
  expr/    Lexer, Token, Parser, AST, Evaluator, TypeCheck, detection
```

- `crates/expr/types` -- type algebra (bounds, promotion rules, conversion).
- `crates/expr/funcs` -- `ExprFunction` trait in `signature.rs`, `FunctionRegistry` in `registry.rs`, builtins in `builtins/` (one file per category).
- `crates/expr/expr` -- parser pipeline: `detect.rs` (is_expression / has_interpolation), `lexer.rs` (tokenizer), `token.rs` (Token enum), `parser.rs` (AST builder), `evaluator.rs` (runtime), `type_check.rs` (compile-time).

## Integration points

- Config loader calls `detect::is_expression` / `detect::has_interpolation` to classify mapping values.
- `ExpressionContext` on `AssembledFlow` holds the compiled program.
- Evaluation happens during **assemble** (no I/O), not validate.
- `EvalContext` provides `env_resolver`, `file_resolver`, `now` timestamp, and `base_dir`.
- Default values are computed through `ExprValue.eval()` (there is no separate default_value module).
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
