# Project guidelines

## Mandatory skills

(Mandatory) When exploring the project, read `air-elt-overview`.
(Mandatory) When writing code or planning, also use `rust-guidelines` and `project-conventions`.
(Mandatory) Before writing or modifying any test (unit, integration, e2e), load `testing-guidelines`.
If you need to work with configs, use `config-format`.
Save context using `context-saving` (load it before complex tasks and project research).
Use the skill tools rather than reading skill files directly.

If you add a new shared utility — document it in `project-conventions`.

(Mandatory) When editing skills, use the `skill-authoring` skill.
Keep skill bodies small. When a section grows too large, move it into `references/<topic>.md` next to `SKILL.md`; in `SKILL.md` leave a short brief and a relative link to the reference.

When updating configs, also update `config-format`.

After a compaction, assume skills are not loaded — load them again before starting the corresponding work.

## Working rules

* Every change (except tests and metrics) must come with tests.
* Always run tests after the work is done.
* All library versions must be pinned explicitly.
* Avoid git operations on update.
* Human-facing instructions live in `README.md`.
* Agent tasks live in `agent_tasks/`. (Only human can write to this dir. You can write there only if user asks you)
* If Docker is unavailable, check podman; prefer podman.
* Delete every temporary file you create.
* After every change, run `just fmt` then `just lint` (or manually: `cargo fmt` and
  `cargo clippy --all-targets --workspace -- -D warnings`). MANDATORY after completing each task —
  after local workspace tests you must run full test suit.
* You may skip plan steps only with the user's consent.
* **(Mandatory)** After completing every task, run validator agents — this is non-negotiable, not opt-in. Skipping is allowed only with the user's explicit consent for the specific task.
* **(Mandatory)** Pick as many validator agents as possible; exclude only those clearly untouched. If touched indirectly — invoke them. Run independent agents in parallel.
* Add files to `.gitignore` if they are temporary or do not belong in git.
* Run the Rust, project-structure, and project-conventions skills before tasks (agents must read them too).
* When you need to read a skill — use the skill tools instead of reading the file directly.
* Talk to the user in their language. In code, strictly English.
* Do not use code edits as a way to ask the user a question (auto edit may be on).
  If you have a question, either request a question form or stop and wait for an answer.
* Prefer standard / typical components over hand-rolled code.
* Do not delete comments without reason.
* If you find a bug in the code via tests, fix it (even if it was outside the task scope). Tests exist precisely to catch and fix bugs.
* perform reasoning in English
* **No future-proofing config fields.** Do not introduce config keys, struct fields, or enum variants whose only purpose is "we might want this later". Add the field together with the feature that consumes it. Reserved-for-future fields rot, drift, and create misleading docs. If a feature is on the near horizon, file an `agent_tasks/` ticket — don't pre-wire the surface.
* **Bump the workspace patch version after each task** via `just bump-patch`. Minor version — only for backward-incompatible changes. Major version — only by the user's decision.
* **Adding a new workspace crate.** Source/sink/storage crates are auto-covered by the structural linter (dep-graph, registry, traits checks). For any other crate type, ask the user which self-lint rules should apply and update `crates/self-lint/src/checks/dep_graph.rs` with the new category and constraints.
* **Non-English text.** If the structural linter flags non-Latin characters, translate the offending text to English before committing.

## Development commands

The `Justfile` is the primary interface for building, linting, and testing.

* `just lint` — fmt check + clippy + cargo-deny + structural lint
* `just test` — auto-fmt + tests (nextest if available, otherwise cargo test)
* `just build` — release build

See the `Justfile` for the full recipe list.

## On `(Mandatory)`

`(Mandatory)` marks top-priority rules. Every other rule in this file is also obligatory — the tag only flags the priority short-list, not optional-vs-required.