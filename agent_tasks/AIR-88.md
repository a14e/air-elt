# Task description

We need to add IPv4 and IPv6 types.

# What we do
1. Add the types to core
2. Add conversions for the core types. At minimum: conversion to strings (string size is known) and v4 -> v6
3. Add support in every backend that can carry them
4. Run a separate agent to investigate possible type conversions

# Requirements
1. During planning, first run agents to verify support at every available backend (separate agent)
2. Before starting execution of the plan, translate this file into English (do not load it into context — overwrite it in place)
3. At the end, validate with agents
4. Extend the test suite over the type set (both e2e and the sinks/sources where it is added)
5. Also do not forget the unit tests
