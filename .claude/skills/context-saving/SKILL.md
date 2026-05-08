---
name: context-saving
description: Materials and recommendations for saving context window — invoke when context conservation is needed
user-invocable: false
---

# Follow these rules to save context

Basic recommendations:
1. Don't read large files in full — read them in segments. Load files into context only if they are reasonably sized.
2. When running terminal queries, use limits and don't read the entire output at once.
3. When debugging, start from the noisiest log errors and move to more local ones (top-down via the 80/20 Pareto principle).
4. Whenever it's reasonable (and it usually is), use the tools described below.
5. Watch out for commands with large output (file greps, linter/compiler output, app launches, tests).
6. Avoid redundant commands. For example, don't `cd` into the project directory while already inside it.
7. For small bulk changes, use bash instead of editing files one by one.
   Before applying — try a dry run and check that nothing is broken.
8. For internet searches, spawn a separate agent so the analysis doesn't pollute context.
9. For routine code search and exploration, spawn agents to keep context clean (except trivial cases done inline).

If the tools below are not installed and the user does not want to install them (offer it explicitly), ignore their instructions.
Install ONLY if the user agrees.


# Compact CLI output

For compact CLI output, use `rtk`:
https://github.com/rtk-ai/rtk

Installation, if missing:
```
brew install rtk
```
```
cargo install --git https://github.com/rtk-ai/rtk
```

It works as a proxy for CLI commands and produces compact output.
(If you suspect the output is incorrect, run the original command.)

Use it for read-only operations only. Do not use it on write operations.
Do not install it as a CLI override — invoke it explicitly.

Existence check:
```
rtk --version
```

## How to use

There are two invocation modes:

1. **`rtk <cmd>`** — if `<cmd>` has built-in special handling (see list below), domain-specific compression is applied. Otherwise it's a regular passthrough with tracking.
2. **Generic filters** over any command:
   - `rtk test <cmd>` — only test failures
   - `rtk err <cmd>` — only errors/warnings
   - `rtk proxy <cmd>` — passthrough without filtering (with tracking)
   - `rtk summary <cmd>` — heuristic compression

Flags: `-u` (ultra-compact), `-v/-vv/-vvv` (verbosity)

## Full list of commands with special handling

**Files**: `ls`, `read` (with `-l aggressive` — signatures only), `smart`, `find`, `grep`, `diff`

**Git**: `git status/log/diff/add/commit/push/pull`
(NOTE: for short stats, `git diff --stat` is more compact than `rtk git diff`.)

**GitHub CLI**: `gh` (pr list, issue list, run list)

**Tests**: `cargo test`, `pytest`, `vitest`, `playwright`, `go test`, `rake test`, `rspec`

**Build/Lint**: `cargo`, `tsc`, `lint` (ESLint/biome), `ruff`, `rubocop`, `next`, `prettier --check`

**Package managers**: `pnpm`, `pip` (list/outdated), `bundle`, `prisma`

**Infrastructure**: `aws`, `docker`, `kubectl`

**Data**: `json` (structure without values), `deps`, `env -f <filter>`, `log` (dedup), `curl` (auto JSON schema), `wget` (no progress bars)

**Analytics**: `gain`, `discover`, `session`, `summary`
