# Agent instructions

## Working style

- Always use the `caveman` skill at its default full intensity.
- For coding tasks, always use the `ponytail` skill at its default full intensity.
- Keep both active until the user explicitly asks to stop or use normal mode.
- Use subagents for independent, bounded work when useful; avoid them for simple or overlapping edits. Before spawning, report model and reasoning mode if the tool exposes them. Report each subagent's role, purpose, and result or status when possible.

## Build and runtime

- Do not build or deliver standalone `.exe` files, or place executable build output elsewhere for the user.
- The user can run the Rust app from source with Cargo. If runtime behavior needs checking, tell the user what `cargo run` command to run; do not compile an `.exe` for them.

## Repository hygiene

- Keep generated firmware build trees, logs, binaries, caches, and temporary artifacts under `firmware/build/`, separated by purpose; do not create new `firmware/build-*` directories.
- Keep source, scripts, tests, documentation, and intentional evidence outside generated build directories.
- Add new artifact patterns to the relevant ignore file before running builds that create them.
- Do not commit generated build output, local-path CMake caches, ELF files, static libraries, or standalone executable artifacts.

## Git

- Never infer permission to commit from earlier commits or requests. Leave requested changes uncommitted unless the user explicitly asks to commit in the current turn.
- Treat each commit request as one-time authorization. Do not commit later fixes, review changes, formatting, or tests without a new request.
- Before committing, ensure changes are scoped to the current request; never include unrelated existing changes.
- `cpfc` or “checkpoint for change” means commit current changes. Keep subsequent changes uncommitted unless the user confirms them.
- “Verify merge” means confirm the merge, safely switch to the default branch, run `git pull --ff-only`, and verify the change is present. Stop if uncommitted work makes switching unsafe. Do not merge or commit unless separately authorized.
