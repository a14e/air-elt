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

Operators (in precedence order, low to high; binary operators are left-associative unless noted):
- `||` logical OR
- `&&` logical AND
- `|` bitwise OR
- `^` bitwise XOR
- `&` bitwise AND
- `==`, `!=`, `<`, `>`, `<=`, `>=` comparison — **non-associative** (`a < b < c` is a parse error; parenthesize)
- `<<`, `>>` shift
- `+`, `-` additive
- `*`, `/`, `%` multiplicative
- `**` power — **right-associative**
- `!`, `~`, unary `-` (prefix)

The parser is precedence-climbing (one `PrattOperator` table in `pratt_operator.rs`), so deeply nested expressions stay shallow on the native stack and the depth guard (`MAX_EXPR_DEPTH`) returns a clean error instead of overflowing.

Function calls: `name(arg1, arg2, ...)`.
Object literals: `{ "key" = expr, "other" = expr }`.
String interpolation: `"prefix {expr} suffix"`.

Comments: `#` starts a line comment that runs to the end of the line (a trailing comment on a statement or a whole comment line). The newline is preserved, so a comment never merges two statements. A `#` inside a string literal (`'a # b'`, `"#fff"`) is a literal character, not a comment — only top-level expression source (and `{...}` interpolation segments) treats `#` as a comment.

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

Type algebra fixes a function's output `DataType` at type-check time (before evaluation), in `crates/expr/funcs/src/arithmetic_utils.rs` and each `resolve_type`. The load-bearing rules:

- **Arithmetic** promotes by `int_bound` bit-width (add/sub `max+1`, mul `a+b`, div `a`, mod `min`), staying `Int64` until bits exceed 64 then `BigInt{width}`; mixed int/float → `Float64`, anything with `Decimal` → `Decimal`. `negate`/`abs` preserve type; `ceil`/`floor`/`round`/`sign` → `Int64`; `power`/`sqrt` → `Float64`.
- **String functions** return `Text` (`length`/`indexOf` → `Int64`; `startsWith`/`endsWith`/`contains` → `Bool`) and are **strict** — a non-text argument is a `TypeMismatch`, never silently stringified (`trim(1)`, `concat(x, 5)` are errors). Stringify explicitly with `toString`; interpolation (`"{expr}"`) renders any type via `value_to_string`, not through `concat`. The optimizer turns `concat(x, "")` / `concat(x)` into a `TypeAssert{String}`.
- **Comparisons** return total **non-null `Bool`**: `==`/`!=` treat null as a value (`null==null`→true, so `x==null` is a real null test); `<`/`>`/`<=`/`>=` return `false` on any null operand. They never leak null into `&&`/`||`/`if`.
- **Casts** return their target type. **Conditionals** (`if`/`multiIf`/`ifNull`/`nullIf`, resolved in `type_resolver.rs`) take the first/value branch's type with per-form nullability; an `if`/`else if` chain folds to a flat `multiIf` at parse time (bounded by `MAX_AST_NODES`, not depth), and a large equality `multiIf` over 1–2 pure keys lowers to an O(1) `Switch`.

Full promotion matrix, DataType fallbacks, size-aware `concat`, and exhaustive per-function rules: [references/type-algebra.md](references/type-algebra.md).

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
