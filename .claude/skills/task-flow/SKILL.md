---
name: task-flow
description: Mandatory planning and execution flow for any non-trivial task. Load before planning starts and before any code change is made.
---
*Written by a human*

# Planning and iteration flow.

For any non-trivial task, enter plan mode.

Gather aggregations and important information through separate agents so the main context does not get cluttered.

When the task introduces a new database, always launch a researcher agent first to map out the contract specifics — protocol quirks, type system edges, isolation defaults, migration semantics, retry/error codes — before planning the implementation.


# Execution graph

As part of planning, work out which stages can run in parallel and launch parallel agents for them.
In other words, the planning stage must produce an ordering graph.
For trivial stages that don't need the full context, also launch separate agents.



# Test order during development

Tests are slow, so first narrow down to the specific tests tied to the current task,
then the full test set.
If you have local tests, start with them. E2E tests come last.
And the full test set is the very last call.
In other words, we have a testing funnel to optimise test time.


# Test writing order.
The choice between the TDD path and the code-first path must be made during planning, not improvised mid-implementation.

When the flow and API are clear — approach the task in a TDD style.
Launch a separate agent to research all edge cases, then write the tests.
Spell out what is being tested and why. Then write the implementation.

If it isn't clear what to put in the tests — start with the interface, then write the tests.

For cases with non-trivial business logic, write the code first and then the tests — when the rules aren't yet defined,
the code is what matters, not the contract, and locking the contract early would freeze a half-formed shape.

After writing the tests, launch 2 agents
1. Validate any missed test cases
2. After it fixes them, launch a separate agent that validates redundancy.



# User assignments.
When you're asked to translate something or to author a skill and you want to look at how similar examples are done.
Launch an agent so it inspects them, mirrors the same approach as in the examples, and reports back to you.


# Composition of the rules above
The rules work hierarchically.
That is, if local agents collect and validate local tests, they test only their own module. Larger tests
and more serious validations are done by higher-level agents

# Reporting

All agents must return a concise report of the work done.
It should not be bloated and must contain the necessary meaning.