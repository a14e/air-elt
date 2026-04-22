---
name: Cargo version-pinning reality vs AGENTS.md rule
description: AGENTS.md requires explicit library versions; workspace Cargo.toml satisfies this with caret ranges, and the lockfile drifts within semver — don't flag resolved-lock drift as a rule violation unless explicit `=X.Y.Z` pins are the project intent
type: project
---

Workspace `Cargo.toml` declares caret-range pins (e.g. `chrono = "0.4.42"`,
`tokio = "1.47.1"`, `uuid = "1.18.1"`, `sqlx = "0.8.6"`). `Cargo.lock`
resolves to newer patch versions (chrono 0.4.44, tokio 1.52.1, uuid 1.23.1)
because caret permits minor+patch updates within the same major.

**Why:** AGENTS.md rule "Все версии библиотек всегда должны быть явно указаны"
reads literally as "always declare the version", and caret pins do declare
a baseline. Requiring `=` pins across a workspace of ~15 deps would be
maintenance-heavy. Treat the rule as "no unversioned dependencies" unless
the user clarifies they want `=` pins.

**How to apply:** In validation reports, note caret vs `=` as an
observation, not a blocker. Flag only *missing* version specs or deps
pulled transitively with no direct pin where policy demands one. Do NOT
cite "lockfile is 0.4.44 while Cargo.toml says 0.4.42" as a violation.

sqlx is pinned `0.8.6` and resolves `0.8.6` — that one really is exact
(caret on a 0.x version only allows the same minor).
