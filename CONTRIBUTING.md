# Contributing to Steve

Steve is a Rust daemon. Changes should stay small, match the commands below, and link the issue they close.

## Prerequisites

- A recent stable Rust toolchain with `rustfmt` and `clippy`:

  ```sh
  rustup toolchain install stable --component rustfmt,clippy
  rustup default stable
  ```

- Cargo (included with Rust).
- Git.

Docker is optional. Use it only for `make docker-build`. The daemon does not need Docker, Swift, or an external telemetry stack to build, test, or run.

CI's S3 integration job starts a compatible object store with `python -m moto.server` (see `.github/workflows/ci.yml`). You do not need moto for `make check`, `make test`, or `make quality-gates`.

## Clone, build, and run

```sh
git clone https://github.com/djh00t/steve.git
cd steve
make build
make doctor
make run
```

`make run` is `cargo run -- serve` and loads `config.toml` unless `STEVE_CONFIG` points elsewhere. `config.example.toml` is the template. Defaults bind inference on `[::]:11435` and management on `[::]:8790` (dual-stack, so `127.0.0.1` works).

Smoke the running daemon:

```sh
curl -sf http://127.0.0.1:8790/health/live
curl -sf http://127.0.0.1:8790/health/ready
curl -sf http://127.0.0.1:8790/api/v1/system/version
```

`make test-upstream` runs the deterministic development upstream on `[::]:18080`. It is a test double, not the M1 proxy.

## Meaningful tests and checks

Follow [the testing policy](docs/testing.md): Given/When/Then acceptance examples,
a red-green development loop, real daemon end-to-end integration as the primary
evidence, and targeted mutation tests for important invariants. Prefer fewer
useful scenarios over duplicate unit suites or coverage targets.

Run `make check` and the test that proves the changed acceptance criterion before
committing and pushing. `make check` runs rustfmt, clippy with warnings denied,
and compilation of all targets/features. `make test` runs the current Cargo tests.
Do not run `make quality-gates` or `make check-full` locally; broad quality and
release checks belong to post-merge `main` CI under the revised policy.

The pre-commit and pre-push hooks both run `make check`; run the affected
acceptance test separately. Existing contributors using `core.hooksPath=.githooks`
pick up the revised hook after pulling this change. `make hooks` enables these
repository hooks for a new checkout.

The current CI workflow still runs the legacy broad gate on push and PR. The
testing rollout package changes those triggers and adds the missing E2E/mutation
commands; those CI and harness changes are not claimed as implemented here.

Other existing commands are listed by `make help`. Future commands in work
packages are gated by their named producer package.

## Style

- Format with rustfmt. CI rejects diffs that fail `cargo fmt --check`.
- Clippy warnings are errors (`-D warnings`).
- Keep the public surface honest: do not document or stub unshipped milestone behavior as if it already works.

## Branches and pull requests

- Branch from `main`.
- Prefer [Conventional Commits](https://www.conventionalcommits.org/) (`feat:`, `fix:`, `docs:`, `chore:`).
- Keep each pull request focused. Link the issue (`Fixes #N` or `Closes #N`) so it closes on merge.
- CI must pass. On Ubuntu that is `make quality-gates`, plus PostgreSQL `steve doctor`, the moto S3 doctor, and `docker build`. macOS runs `make check`. Windows runs the same fmt, clippy, and cargo check commands.

## Project documents

- [Architecture and product specification](docs/specs/2026-09-26-steve-gateway.md)
- [MVP plan and backlog](docs/plans/2026-09-26-steve-mvp.md)

Use the [reviewed backlog index](docs/backlog-index.md) to select work. Follow the [readiness and ownership rules](docs/work-packages.md); milestone membership alone does not make two packages safe to implement in parallel. GitHub issues hold the detailed briefs.
