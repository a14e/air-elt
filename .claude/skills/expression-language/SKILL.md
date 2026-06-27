---
name: expression-language
description: Syntax, type system, and function registry for the Air Elt expression language. Load when working with expressions in configs, adding new expression functions, or debugging expression evaluation.
user-invocable: false
---

# Expression language

Air Elt configs use an expression language for computed column values, defaults, and string interpolation. Expressions are pure (no I/O at evaluation time).

Two evaluation contexts:
- **Assemble-time (once per flow):** `default = ...`, switch values, component-config and `${VAR}` interpolation. No row access — `field()` / `fields()` are rejected here (`create_comptime` grammar).
- **Per-row (in the Transform):** `compute = "<expr>"` mapping columns and the `[flow.<name>.compute-mapping]` shorthand. These are the **only** place runtime scripts run; they may read source columns via `field("col")` / backtick `` `col` ``, the named pack `fields("a,b")`, or the whole-row pack `fields("*")` (which projects the entire source schema). Compiled once to a `RuntimeProgram`, then run per row. A script that const-folds becomes a literal column; a bare `field("x")` becomes an identity rename; otherwise it runs as a per-row `Compute` op coerced to the sink type. See the `config-format` skill ("Compute columns").

**`now()` / `today()` batch semantics:** referentially transparent (`is_pure() == true`) but NOT compile-time-foldable (`purity() == false`) — pinned to one batch clock at program init, so every row in one write batch shares the same timestamp (SQL `NOW()` / `CURRENT_TIMESTAMP`). Never const-folded (the clock is unknown at compile time). When adding a runtime-reading-but-stable builtin, set `is_pure` by determinism and override `purity` → false; do not gate folding on `is_pure`.

## Syntax

Literals: integer (`42`), float (`3.14`), bool (`true`/`false`), null (`null`), double-quoted string with interpolation (`"hello {expr}"`), single-quoted raw string (`'no interpolation'`), duration (`10s`, `500ms`, `1h30m`, ISO-8601 `PT1H30M`).

Duration literals yield `Value::Interval` (a `std::time::Duration`), type `DataType::Interval`. The lexer triggers on a digit immediately followed by a unit (no space — `3 days` is not a literal) or an ISO `P`/`p` prefix, and parses via `air_elt_commons::interval` (the workspace duration parser; no hand-rolled grammar). `Interval` is **minimal by design**: identity-only in the type matrix (no conversions to/from other types), never a cursor/switch key, never JSON-encoded. It exists to type the Redis sink `ttl` column (the sink reads the `Duration` directly); arithmetic over `Interval` is not defined.

Variables: `x = expr` (assignment via `=`, separated by `;` or newlines).

If-expressions: `if (cond) value else other` — JS/Kotlin-style expression form alongside the function form `if(cond, value, other)`. Always an expression yielding a value: `else` is mandatory (there is no statement-if) and the condition parens are required. `else if` chains fold to a flat `multiIf` at parse time, exactly like nested legacy `if(...)` chains; for brace-less branches the AST is byte-identical to the legacy form, and the two forms mix freely in one chain (`if (a) 1 else if(b, 2, 3)`, `if(a, 1, if (b) 2 else 3)`). The `else` branch is greedy — `1 + if (c) 2 else 3 + 4` ≡ `1 + (if (c) 2 else (3 + 4))` — and an if-expression is itself a valid operand (`1 + if (c) 2 else 3`). `if` and `else` are reserved keywords (`RESERVED_CONTROL_FLOW_NAMES`): `if = 5` and `else = 1` are parse errors.

Branch blocks: a branch may be a brace block `{ x = expr; y = expr; result }` — zero or more `name = expr` bindings followed by a mandatory trailing result expression (`{ x = 1; }` is a parse error; there is no `return` keyword). A binding evaluates once at its binding point (same as top-level statements, including impure calls like `randomInt()`), is visible only inside its block and nested blocks, may shadow an outer name (the outer binding is unaffected after the block), and counts toward the program-wide `MAX_VARIABLES` limit (64). A not-taken branch — including its bindings and their failures — never evaluates. Block vs object literal is disambiguated **in branch position only**: `{}` → empty object; `{ "key" = ... }` (string-literal key) → object literal; anything else (identifier binding or expression) → block. Elsewhere `{` is always an object literal. A block whose result is an object needs nested braces: `if (c) { { "k" = 1 } } else ...`. Internals: blocks lower to the `OptExpr::Block` / `OptNode::Bind` IR, and branches containing blocks are excluded from Switch lowering (blocks are never cloned).

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
Object literals: `{ "key" = expr, "other" = expr }`. An object literal evaluates to a `Value::Object` (ordered key/value list, type `DataType::Object`) — the only producer of `Value::Object` in the language, and what the `object*` functions consume. Values keep their canonical types (an `Int64` field stays `Int64`, not a JSON number). A repeated key is a **compile-time error** (rejected in the optimizer's converter, before const-folding can collapse the literal). Arrays have no `Value` variant — a nested array value is carried as `Value::Json(Array)`.
String interpolation: `"prefix {expr} suffix"`.

Comments: `#` starts a line comment that runs to the end of the line (a trailing comment on a statement or a whole comment line). The newline is preserved, so a comment never merges two statements. A `#` inside a string literal (`'a # b'`, `"#fff"`) is a literal character, not a comment — only top-level expression source (and `{...}` interpolation segments) treats `#` as a comment.

## Detection rules

The config loader determines what is an expression vs a plain string:
- String starting with `identifier(` = full expression (parsed and evaluated).
- String starting with `if (` = full expression (the if-expression form). Pre-existing limitation unchanged: an operator-first value like `1 + if (...)` is not auto-detected.
- String containing unescaped `{expr}` = interpolation (each `{...}` segment is parsed).
- `{{` escapes to a literal `{` in interpolation.
- `$$` escapes `$` in `env_expand` (secret resolution layer, runs before expressions).

Config format matters for expression quoting: in YAML, expressions need no outer quotes (YAML handles strings natively). In TOML, outer quotes are required because bare values are typed. Multiline scripts (e.g. if-expressions with blocks) work via TOML `"""..."""` strings or YAML block scalars — newlines already act as statement separators.

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

- **Arithmetic** promotes by `int_bound` bit-width (add/sub `max+1`, mul `a+b`, div `a`, mod `min`), staying `Int64` until bits exceed 64 then `BigInt{width}`; mixed int/float → `Float64`, anything with `Decimal` → `Decimal`. `negate`/`abs` preserve type; `ceil`/`floor`/`round`/`sign` → `Int64`; `power`/`sqrt` → `Float64`. `power` (`**`) always evaluates as `Float64`; the optimizer reduces only the exact float identities `x ** 1 → x` and `x ** 0 → 1.0` (the latter gated on a non-null, infallible base), and deliberately leaves `x ** 2`/`x ** 3` as `power` calls because `powf` is not portably bit-equal to repeated multiplication.
- **String functions** return `Text` (`length`/`indexOf` → `Int64`; `startsWith`/`endsWith`/`contains` → `Bool`) and are **strict** — a non-text argument is a `TypeMismatch`, never silently stringified (`trim(1)`, `concat(x, 5)` are errors). Stringify explicitly with `toString`; interpolation (`"{expr}"`) renders any type via `value_to_string`, not through `concat`. The optimizer turns `concat(x, "")` / `concat(x)` into a `TypeAssert{String}`.
- **Comparisons** return total **non-null `Bool`**: `==`/`!=` treat null as a value (`null==null`→true, so `x==null` is a real null test); `<`/`>`/`<=`/`>=` return `false` on any null operand. They never leak null into `&&`/`||`/`if`.
- **`min`/`max`** skip NULL arguments (SQL semantics) and yield NULL only when *every* argument is NULL — so the result is nullable exactly when every argument is nullable (one non-null argument forces a non-null result). The optimizer exploits this: it drops NULL-literal arguments (`max(a, null, b)` → `max(a, b)`) and saturates against an integer operand's type bound (`max(x, TYPE_MAX)` → `TYPE_MAX`, `min(x, TYPE_MIN)` → `TYPE_MIN`).
- **Casts** return their target type. **Conditionals** (`if`/`multiIf`/`ifNull`/`nullIf`, resolved in `type_resolver.rs`) take the first/value branch's type with per-form nullability and **drop `int_bound`** (branch merge calls `NullableExprType::new`, so integer arithmetic on a conditional's result falls back to DataType-level bits — e.g. `if(...)+1` over Int64 promotes to BigInt); an `if`/`else if` chain — in either surface form, nested `if(...)` calls or `if (c) ... else if ...` expressions — folds to a flat `multiIf` at parse time (bounded by `MAX_AST_NODES`, not depth), and a large equality `multiIf` over 1–2 pure keys lowers to an O(1) `Switch`.

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
