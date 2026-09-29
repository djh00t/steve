# STV-M7-01 — TUI toolkit and test approach (proposal)

**Status:** Proposed for David/Cos review; not accepted. This note records one toolkit/test decision only.

## Decision

Use **Ratatui** with its default **Crossterm backend**. No current direct dependency provides a TUI: Cargo.toml has CLI/HTTP/runtime crates, but no Ratatui, Crossterm, or other terminal UI crate. Do not hand-build full-screen layout, raw input, and terminal restoration from ANSI/std I/O for the 14 planned screens. Ratatui supplies the renderer and TestBackend; its default backend supplies terminal input/modes. Ratatui documents Crossterm as the common backend and supports testing render output with TestBackend ([backend overview](https://ratatui.rs/concepts/backends/), [Ratatui testing](https://ratatui.rs/recipes/testing/snapshots/)).

Use the backend through Ratatui's `ratatui::crossterm` re-export initially, so the proposal adds one direct crate rather than two; Ratatui documents this re-export ([installation](https://ratatui.rs/installation/)). Select and qualify the version only through the repository Dependency Advisor workflow before implementation. No dependency is added by this decision package.

## Interaction and ownership

Use one conventional keyboard map: Up/Down move selection, Enter opens/activates, Esc goes back/cancels, `?` shows help, and `q` quits outside active text entry. Text-entry widgets consume ordinary characters; Esc cancels editing before navigating back. Use the planned `crates/steve-tui/` workspace member from #293. Its shell owner maintains `src/main.rs`, `src/app.rs` and `src/navigation.rs` within that crate; each screen owner has a disjoint `src/screens/<screen>.rs` file and its focused tests. The shell producer owns its crate manifest; root workspace registration and shared screen-module registration are serialized. Keep the TUI separate from the daemon binary.

The TUI calls the existing management HTTP API using the repository-selected `reqwest` version, qualified for the new crate through Dependency Advisor before addition. It does not bind to daemon internals. M7 screens that need management routes not yet present must wait for those route contracts.

## Test strategy

Use Ratatui TestBackend and Rust unit tests for rendered output and key-event handling. After the management client and its API contracts are implemented, the remote UI acceptance scenario is one focused test: serve the existing `/api/v1/system/version` response shape from a local HTTP fixture, send Down then Enter through the app's key handler, refresh through the real reqwest client, render to TestBackend, and assert the selected screen and returned version are visible. This verifies the UI-to-HTTP-to-render path without terminal-specific output snapshots. The existing management E2E suite separately verifies the live daemon route. Also require one bounded Linux PTY smoke check for the real binary: use Python standard-library `pty`/`termios` facilities, wait for the initial screen, send a real quit key, and verify clean exit plus restoration of the original terminal attributes. Exercise interrupted exit in the same check and assert restoration of the original terminal attributes after that run too. This covers input/mode cleanup that TestBackend cannot prove; it is not a second screen-snapshot suite. The producer must qualify the actual CLI launch and local HTTP fixture setup against the implemented command, then wire the smoke check into Ubuntu CI.

**Future PTY command:** `python3 scripts/tui_terminal_smoke.py`. This script does not exist and is not runnable evidence yet; its implementation belongs to the TUI shell producer, using stdlib only.

**Future shell command:** `cargo test -p steve-tui stv_m7_03_acceptance`, introduced and qualified by #293 with its workspace member. It is unavailable on this base.

**Future composed command:** `cargo test -p steve-tui remote_dashboard_renders_system_version`. It is **unavailable until implementation adds that test/module**; it must not be treated as passing evidence on this base. On `main` pushes, the hosted Ubuntu quality job runs `make quality-gates`, which includes `make test`; on PRs it runs `make check`. When #293 introduces the workspace member, it must include both the root crate and `crates/steve-tui` in `workspace.default-members` so those default Cargo commands actually exercise the new crate; verify the named tests execute. No separate test framework is proposed.

## Evidence and boundary

Reviewed `docs/plans/2026-09-26-steve-mvp.md#M7`, `docs/specs/2026-09-26-steve-gateway.md`, `Cargo.toml`, `Makefile`, `.github/workflows/ci.yml`, current `src/main.rs`, and `tests/support/process.rs`. That process helper is available only to integration tests and does not expose the future TUI crate; the proposed focused unit test therefore uses an HTTP fixture, while current E2E route proof remains separate. The plan requires all daemon configuration/reporting and remote operation; this note does not claim those screens or their API contracts already exist.


## Consumer handoff

#293 owns the navigation shell and real-terminal smoke check after this decision is accepted. It can establish the shell without a management client or remote-dashboard test. #292 owns management-client errors, and #297 owns the authenticated HTTP client with its existing prerequisites. The proposed remote-dashboard test belongs to that later composed UI/client slice; it must not become a prerequisite of #293. #357 also consumes this decision alongside its macOS and client prerequisites. All remain blocked until the responsible authority accepts an exact artifact revision and each brief is qualified against its own actual source seams.
