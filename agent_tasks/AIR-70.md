# Task description

This task is a small refactor plus a new syntax addition at the transform level.

# Changes

## Syntax change

The mapping structure changes from
```toml

mapping = [
  { from = "field1",   to = "field2", truncate = true },
  { from = "field3",   to = "field3", default = 2 },
  "field4",
    "*:obj"
]
```
to a table-based form

```toml
[mapping]
field2 = {from = "field1", truncate = true}
field3 = {from = "field3", default = 2}
field4 = "field4"
obj = "*"
```

That is, we invert the mapping (sink columns become keys).

The wildcard changes from
```toml

mapping = ["*"]
```

to
```toml
[mapping]
"*" = "*"
```

We also drop half of the shorthand forms that cannot be remapped under the new shape, such as `"*"`, `"field1"`, or `"field1=field2"`.

## Switch expression

We add the ability to pick one of several values

```toml
[mapping]
field1 = {
    from = "status",
    switch = {
        "ACTIVE" = "active",
        "FINISHED" = "finished"
    },
    default = "unknown"
}
```

At the transform layer this maps the left-hand value to the right-hand value.
If the value is not found in the table — pick `default`.
If the switch returned null and a `default` is given — also pick `default`.

When validating the schema we require the left-hand types to be compatible with the source-expression types, and the right-hand types to be compatible with the sink-column types.

Probably the best approach is to push the types through transform first and then compare against the right-hand types. For this the transform layer needs a type mapping. Importantly, the left side can be a string, an integer, or a bool — and we can only tell which when we know the types. (I think the optimal implementation for this is to build a map containing every variant representation of each value — i.e. `1` as a string and `1` as a number, etc., and dispatch through that.)

For strings the output type is `Text` sized to the widest string; for ints `Int` of the narrowest fitting width.
Objects and other standard types are also allowed on the right-hand side.


## Other refactoring
1. In core we currently have
```rust
  batch.rows.retain(|r| r.op != RowOp::Delete);
```
this is a pointless filter pass because sinks that do not support deletes already filter deletes themselves. So let's remove the delete branch.

2. Let's drop the `RawRow` entity and merge it with `Row`, and remove the extra runner branches that just pass objects through. Instead, route directly through transform and into the sink (transform will need a dedicated case for this).
And a root object will, in most cases, ship to the sink as-is. Without modifications, when we did not add extra fields.
(For typed sinks, the presence of an unmapped field in the response must explicitly raise an error.)

The idea is that after this optimisation the identity mapping should fly through with almost no copies.

# Misc
1. Update all tests and examples that depend on this.
2. Before starting, translate this file's text into English.
3. Start the task by running validator agents (except protocol-specific ones) to analyse the task and gather requirements and questions.
4. Then assemble a plan.
5. Then execute.
6. Mostly unit tests for this task; the remaining tests must keep working correctly.
7. In e2e tests just add 1 switch in each edge case
(unstructured-in, unstructured-out, struct-to-struct, plus string, bool, and int keys).
8. Verify with test coverage that we did not miss anything.
9. After completing the task, run the validator agents.
