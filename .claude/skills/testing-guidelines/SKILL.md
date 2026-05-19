---
name: testing-guidelines
description: Requirements to follow when writing or modifying any test (unit, integration, e2e, manual). Load before authoring tests.
user-invocable: false
---

# Testing rules

The rules in this skill take precedence over any testing rules stated in other skills.

## Core rules

1. Any changes to types and converters must be covered by unit tests via property tests.
2. Any new types must be reflected in e2e tests.
3. For unit tests we use `mockall::automock`. Preference must be given to attribute-style macros rather than `!`-macros.
4. Test dependencies and utilities must not leak into the production build.

## Testing chain

The testing chain for any change is:

unit → integration (the ones inside sinks and sources) → e2e

The full test suite must be run after every completed task.

## Test runner

We use `cargo nextest` instead of `cargo test`:

1. Full task-completion run: `cargo nextest run --no-fail-fast` (covers the whole workspace; `--no-fail-fast` reports every failure rather than stopping at the first).
2. Regular dev runs (single suite, single test, narrow scope): `cargo nextest run …` without `--no-fail-fast`.

## When adding a new sink / source / storage

1. Write detailed tests for it in its folder.
2. Mandatorily register it in `app` (app::registry::build_registry).
3. Mandatorily add a test for it in `app` as an e2e test.
   (There is an exception for storage, since many storages are duplicates and one of them can be swapped in.)
4. For new integrations, mandatorily add test containers.
5. When adding a new test that uses test containers, give them reproducible names so they can be reused between runs, and mandatorily register them in ryuk.

## Additional requirements

1. All changes must be covered by tests.
2. All resources in tests must be released automatically (sooner or later — e.g. with ryuk they are released asynchronously).

## Test layout

All test directories of the form `/tests` must contain a root `all.rs` file, with the project files configured via:

```toml
[[test]]
name = "all"
path = "tests/all.rs"
```

## E2e driver-specific rules

Requirements for working with e2e tests (drivers not listed here are not covered):

1. For `sqlx` — MANDATORY to close the pool at the end of the test.
2. For `mongo` —  MANDATORY to NOT call Client::shutdown() at the end of the test (deadlocks on live Arc clones).

## Time in tests

Do not use delays in tests. Avoid `sleep` as much as possible. Introduce delays via:

```rust
#[tokio::test(start_paused = true)]
```

For CDC scenarios, instead of `sleep`, match on batch size to avoid waiting.

## Additional requirements and specifics for running manual tests

This section refers to tests in the `/manual-tests` folder.

They are not run in CI; they must be run and tested only when the user asks. Their run scripts include simulating realistic load via Python scripts (we use `uv`). The tests are expected to be long-running, e.g. 5–10 minutes.

### What to check after starting

1. We verify right away that everything started and do not wait for the script to finish.
2. We confirm that validation and the processes have reached steady-state mode and that there are no errors.

### What to look at after the test

1. That all container resources we allocated have been torn down.
2. That there were no errors (if there are, investigate the cause).
3. After stopping the load, confirm that lag drops strictly to 0.
4. Also assess that the load's RPS during transfer matched or was close to the expected value.
5. Also mandatorily look at the validate time.
