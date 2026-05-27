# Task description

We are creating an expression language. First task — a static scripting language.

# What are we building?
1. A static expression language
2. Simplifying work with float/int types by replacing them with bounded types
3. Integrating into key places

# Folder structure
1. Add an `expr-and-types` folder under `crates`
2. Place a `types` folder inside it
3. Next to it place `expr` — the main expression language — and `funcs` — function tables
4. In `expr` put parsing and the execution engine. Most likely `expr` depends on `types` and `funcs`, while `funcs` depends on `types`. Nullability will probably need to be added as a wrapper around the type and refactored in many places — currently it lives in fields, but in expressions there are scenarios where there is no field name but nullability is still needed
5. In `funcs` there will be a type registry assembled from multiple registries declared in mongo, in funcs, and elsewhere — it will be composed in app

# Language features
The language is a simple language with minimal branching and ClickHouse-like functions.
No loops, custom functions, etc.
Branching uses functions:
if(cond, then, else)
multiIf(c1, t1, c2, t2, ..., else)
ifNull(x, alt) / nullIf(x, y)
isNull(...) / isNotNull
max(...)
min(...)

Variable declarations like `a := 1` are plain literals that are inlined without mutability.

The language supports function overloading (for example `env("KEY", "default")` returns not-null while `env("KEY")` returns nullable).

String interpolation must work in all strings: `"{1 + 1}"`.

We build a common function registry with the ability to register functions.

For example, MongoDB has ObjectId. `ObjectId("asv....")` must be implemented for hex, and the main interpreter should not know about it.
`seconds(objectId)` extracts seconds from an objectId.

All expressions return an explicit type; `eval(1 + 1)` returns `int(2)` because 2 is the upper bound of known literal types.

Currently implemented as separate parsing into a tree, followed by expression evaluation through a separate engine.

# Type inference and type conversions
1. String, integer, and floating-point types store bounds. For example `varchar(10) + varchar(10) = varchar(20)` or `int64 + int64 = int65`.
2. We are redesigning int and float types: integers will come in 2 kinds (int64 and BigInt, one transitions to the other on overflow); floats are stored as float64 with specified precision.
3. If a type cannot be inferred from an operation — default to the lower bound.
4. All operators (only basic ones) have function equivalents and are automatically converted to functions.
5. All functions have known input and output parameters. For each function we implement a trait with 5 fields:
   1. Name
   2. Argument count
   3. Argument types (types as a slice; may be optional — we build our own wrapper around the type)
   4. Result computation based on input
   5. Function to compute the resulting type based on input types
6. Simple literals are parsed with size and bounds immediately.

# Where do we apply it?
1. In all secrets, URLs, and database connections
2. In default values
3. In secrets
4. In switch expression values

# What about environment variables?
The `${ENV}` syntax remains as preprocessing.

# R&D
1. During plan formulation, conduct research on possible functions to implement. Done in 2 steps:
   2. First the agent selects domains in which to look for possible functions
   3. Then launch researcher agents in parallel to compile lists of typical functions worth implementing
      (interested in typical functions for numbers, strings, hashes, bytes, time, encodings, switch analog, jspath,
      slice and index access, and type casting (Go-style); for encoding, hashes, and encryption we make both specialized
      and generic functions where a parameter can be passed)

# What side effects are currently possible?
1. env(...)
2. file("path_to_file")
3. now()

# Object parsing

In `default` values and in `switch` it is allowed to use objects. An expression can parse not just a string but an object of a special kind.
This is an internal object representing a nested type of a special form.

For example, you can write something like:

[flow.myflow.mapping]
myfield = {
   form = "field"
   default = {
      "key1" = env("value1")
      "value" = "value"
      "parse" = eval(1 + 1)
      "interpolated" = "hello, world {'!'}"
   }
}
It works by parsing the string values and assembling an object that can be created. Currently it is completely static.
For this we need to create an Object type that can convert to bson/json and extend their allowed fields. The difference with Object is that it can accept values of arbitrary kinds.

# String parsing specifics
Since in yml and toml it is not always clear what is a string, the logic is:
if the expression after trim starts with `myname(....)` then it is treated as a function;
if not — it is a string. Also, if you want to compute something you can specify the `eval(...)` function at the root to avoid string complications and return a number.
Strings at the root always allow string interpolation.

# Requirements
1. At the start of implementation, translate this task to English
2. Try to minimize changes to custom types except frequently used ones
3. Add a linter rule that types, functions, and expressions can only depend on commons and each other, not on database commons
4. Also add a requirement that core cannot depend on database commons
5. Discuss the necessary parsing mechanism during planning
6. After implementation, run validator agents
7. The implementation must be strictly backward-compatible with current configs
8. Do not forget to run validators via just (probably need to bump version via `just bump-patch` or it will not build)
9. Create a skill for teaching the expression language. Add to agents.md (in 1-2 short lines) that when extending the expression language and related things, the skill should be updated. In the skill itself describe that references should be used if any section is too large. For example, sections listing functions may be heavy. In the skill itself write that it should be read only when working with expressions, and in the config skill describe the fields where expressions work.
10. Update e2e tests with these expressions
11. Need many unit tests for various scenarios
12. Launch a separate agent to validate expression corner cases
13. The current script is limited to compile-time execution only when assembling flows. A separate task will cover runtime scripts. During execution the entire assembly will be converted to understandable types.
14. No bytecode — use tree walking
