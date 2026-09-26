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

## Checks before you push

Enable the repo hooks once:

```sh
make hooks
```

`pre-commit` runs `make check`. `pre-push` runs `make quality-gates`.

Run the same gates by hand:

```sh
make check
make quality-gates
```

`make check` is:

- `cargo fmt --all -- --check`
- `cargo clippy --all-targets --all-features -- -D warnings`
- `cargo check --all-targets --all-features`

`make quality-gates` runs `make check`, `make test` (`cargo test --all-features`), then `cargo build --release --all-features`.

Other Makefile targets: `make help`.

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

M0 (clean foundation) is on `main`. Next work is M1 (real proxy hot path), issues [#6](https://github.com/djh00t/steve/issues/6)–[#10](https://github.com/djh00t/steve/issues/10). Do not start M1 inside an unrelated hygiene change.
