# Task Description

We are adding MS SQL integration.

# Scope of Work
* Add commons (following the pattern of pg/mysql)
* Add sink (following the pattern of pg/mysql)
* Add source (following the pattern of pg/mysql)
* Add storage (following the pattern of pg/mysql)

# Types
1. Aim to support as many types from the standard type tree as possible
2. Add as many custom types as feasible (MS SQL → MS SQL round-trip scenarios only)
3. For type collection, separately launch an Opus agent to produce a report on types and compatibility
4. Skip geo types and time-with-timezone types for now
5. Consult pg and clickhouse implementations for type patterns

# CI
1. Add MS SQL container to CI

# Tests
1. Write tests in all repositories (crates)
2. Comprehensive type tests covering the full type set are mandatory
3. Investigate edge cases when working with the database
4. Add e2e tests in app — minimum 2: one with a sink and one with a source
5. Also add MS SQL storage to one of the existing tests (while keeping existing database coverage intact)

# Requirements
1. Before starting work, translate the task in this file to English and update this file
2. Container versions in CI and tests must match
3. After completing the work, run validator agents with the Opus model
4. Core should most likely not be modified. Only change it after agreeing with the user.
5. Don't forget to register all sinks, sources, and storage in the app
6. Keep in mind that tests exist to find real implementation bugs — our goal is not to write code, but to produce a valid implementation
