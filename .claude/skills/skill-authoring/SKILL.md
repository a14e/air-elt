---
name: skill-authoring
description: Rules and requirements for writing and maintaining skills in this repository. Read before creating a new skill, editing an existing one, or reviewing skill content. Defines frontmatter shape, tone, structure, length, and what a skill must never contain.
---
*Written by a human*

# General requirements for skills
1. Skills are written exclusively in English.
2. Skills must be understandable and readable both for agents and for people.
3. If a skill is written by a human, when refactoring or translating it keep it as close to the original as possible (but fix formatting and spelling).
4. Avoid typical AI slop.
5. When writing skills, avoid duplicating the same text.
6. Avoid text for the sake of text — every part must add meaning.
7. Be concise. Try to express thoughts briefly.
8. Be specific. Use enough words and text to be unambiguous.
9. Before lists of requirements, always add context (except when the requirements are unrelated).
10. Before writing a skill, think the plan through in detail and write according to the plan.
11. Each next stage or topic of a skill must follow from the previous one, so the text stays clear and logical.
12. When condensing text for consistency, keep the high-level intent and philosophy rather than technical details. Technical details can always be looked up in the code; the reasoning behind them has to be interpreted.
    (this refers to calls to specific methods and interfaces; what's important is where to find them and where they are
    located)
13. Do not modify if there are strict code constraints (e.g., fixed version) -- this is important
14. Also, if we want to describe some structure and requirements for it -- this is also important

If your text matches one of the following types, use the corresponding template (for the content).

# Template for skills describing a process
Use this when your skill is a process.
```md
# Title
Description of the process and why it is needed + rationale and context if any.

# Main steps
1. step 1
2. step 2
   2.1 substep
3. ...

# Step 1.
General description.
## Stages of the step
1. stage 1
2. stage 2
.....

## Requirements for step 1
1. requirement 1
2. requirement 2

.... (after all steps)

# General requirements
1. requirement 1
2. requirement 2
```

# Template for requirements
Use this when the skill has a set of consistent requirements (if the requirements are unrelated and there is no context, just list them as bullets).

```md
## Requirements for thing 1
Context and rationale for the requirements.
1. requirement 1
2. requirement 2
```
