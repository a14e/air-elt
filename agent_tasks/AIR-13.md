# Task description

We are adding type narrowing and a way to bridge nullability incompatibilities.

# How we are solving it

1. Add two new fields to the column-mapping description:
   - `truncate` — boolean — to truncate types: ints, strings, etc.
     (`truncate` cannot be set on types that don't support it — e.g. `bool`, or
     conversions where it makes no sense like `json → json`.)
   - `default` — separate field. Specifies a fallback value when the source
     is `null`. Lets us map nullable sources into `NOT NULL` sink columns.
     Must error if applied to a NOT-NULL source (the default would never
     fire). For binary types a special syntax is supported:
     `hex:0x1234567890abcdef`, `base64:base64string`, `utf8:utf8string`,
     `bin:01234`. Only these four variants. This typed-prefix grammar is
     allowed only on `Bytes` columns (other types don't need it).

2. Add the full set of binary text/blob types (blob, clob, etc.).

3. Add truncation in JSON variants that cuts the JSON from the start
   (e.g. `json → string`).

4. Add validation that when truncating strings we must cut at the last
   complete UTF-8 codepoint, so the output is never invalid UTF-8.

5. Remove every parameter from the configs and the skill descriptions that
   does not currently exist. Do not leave anything for the future. (Codify
   this rule in the project conventions and the Rust skill.)

6. For `binary → string` only the UTF-8 form is supported.

# Testing

1. Write tests, run formatting, etc.
2. After everything is done, run all validator agents and apply fixes.

# Other

1. Translate this task to English at the top.
