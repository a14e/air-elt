# Overall task description

Add a sink for QuestDB.

# File layout
1. First add a section in commons for QuestDB.
2. Then add a section in sinks.
3. Then register it in app.

# Test structure and tests in general
1. Add tests for every type we describe as exportable.
2. Add as many types as possible to e2e tests in app.
3. Test invocation hierarchy:
   1. local first
   2. then global
4. At the end always invoke the global tests (mandatory).

# Data types
1. Before anything else, run a separate agent that investigates the protocol structure and data-type specifics for this task.
2. Support as many data types from the possible set as we can.
3. Based on the report, list which non-standard data types we can add as custom types and which the sink can use natively.

# Containers
1. Add a dedicated test container for QuestDB.
2. Add it to CI.
3. Require the container name to match between CI and local tests.

# Deletes and conflicts
Behave like ClickHouse.
Also require validation of the time column.

# Pool settings
1. Before the researcher runs, launch a separate agent to investigate thread pools and which pool / pool-config parameters are valid.
2. Return an error for unavailable fields if specified.

# Other requirements
1. Before starting implementation, translate this file into English and replace the contents of this file with the English version.
2. After completion, run a separate agent to validate corner cases that were not covered.
3. Then run the validator agents and address their findings.
4. Then run test coverage and fill the uncovered parts with tests; invoke a dedicated agent to find scenarios and recommendations.
5. The core part must be touched as little as possible — ideally not at all.
6. Other databases must not be touched. If you find a bug, report it.
