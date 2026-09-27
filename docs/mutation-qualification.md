# Targeted mutation qualification

STV-TST-03 (#74), qualified 2026-09-27 against source commit
`2f8df419af390ef8cee57f9a0bab144872782b11` on macOS. This qualifies the
runner and named faults; CI wiring is #79. It is not a repository-wide mutation
score or an accounting-durability qualification.

## Pinned tool and bounds

Dependency Advisor recommended **cargo-mutants 27.1.0** for Rust under the
**conservative** policy (minimum release age **720 hours**). The installed CLI
reported that exact version. Install it as an external test tool, never a Steve
runtime dependency:

```sh
mutation_tools=$(mktemp -d)
cargo install cargo-mutants --version 27.1.0 --locked --root "$mutation_tools" --jobs 2
export PATH="$mutation_tools/bin:$PATH"
cargo mutants --version
```

Remove that temporary tool directory when qualification is finished. Cargo's
normal dependency cache may be reused. Run `make check` first to fetch/build
Steve's dependencies before the offline commands below.

`cargo-mutants` uses disposable source copies by default. Do not pass
`--in-place` or `--leak-dirs`. These verified commands select exactly one mutant
each, run an unmodified baseline, allow one mutant worker/two build tasks, and
bound each build to 600 seconds and each test command to 30 seconds. A process
bound expiring is **timeout**, not caught. A test's explicit five-second body
assertion failing is caught only when the log proves the intended fault.

```sh
CARGO_NET_OFFLINE=true CARGO_BUILD_JOBS=2 cargo mutants --no-config \
  --file src/proxy/stream.rs --re '<impl Drop for DisconnectStream>::drop' \
  --all-features --jobs 1 --jobserver-tasks 2 --build-timeout 600 --timeout 30 \
  --copy-target false --output /tmp/steve-mutation-cancel \
  -- --test e2e_responses --test e2e_messages

CARGO_NET_OFFLINE=true CARGO_BUILD_JOBS=2 cargo mutants --no-config \
  --file src/proxy/stream.rs --re 'ReplayGate::ensure_can_attempt' \
  --all-features --jobs 1 --jobserver-tasks 2 --build-timeout 600 --timeout 30 \
  --copy-target false --cargo-arg=--bin --cargo-arg=steve \
  --output /tmp/steve-mutation-replay \
  -- proxy::stream::tests::no_replay_after_output_begun
```

Use distinct output directories for concurrent runs. Retain `mutants.out` and its
logs with the tested SHA until outcomes are reviewed. Check `--list` with the
same file/regex before running on changed source; zero selected mutants is not
qualification. Check nonzero test counts in baseline and mutant logs. Steve is
a binary crate: `--lib` is not a valid test target.

## Reviewed buffering fault

The runner does not generate the required buffering transformation. The
[reviewed patch](../tests/mutations/buffer-until-eof.patch) collects upstream
chunks and delays sending them until the read loop ends. It is deliberately
wrong code, used only in a disposable copy. Its qualification covers delayed
first output, not its other termination behavior.

Run the bounded Unix runner from the repository root after `make check` has
resolved dependencies. Give it an evidence directory outside the checkout:

```sh
evidence_dir=$(mktemp -d)
python3 scripts/qualify_buffering.py --output "$evidence_dir"
```

The runner copies current source files to a temporary directory, excluding
Git metadata, `target`, caches, and output while preserving `Cargo.lock` when
present. It uses an isolated target directory, two Cargo build jobs, offline
Cargo, no color, and a 600-second bound for each command. Each command runs in
its own Unix process group; the group is stopped after command exit, timeout, or
SIGTERM, and the leader is reaped before temporary source and target data are
removed.

It first checks that the reviewed patch matches the disposable source, then
runs the exact `provider_fixture_controls` baseline and requires a successful
exit with exactly one selected test. It applies the patch, rebuilds the Steve
executable, and runs that same test. A qualification succeeds only when exactly
one test is selected, Cargo exits 101, the named test and test result are
reported as failed, and the log contains `response.created was not forwarded`.
Build errors, baseline failures, patch mismatches, timeouts, bind failures, and
other test failures remain errors.
`result.json` records the source HEAD, dirty-tree flag, stage exit codes and
selection counts; `baseline.log`, `fault.log`, and patch/build logs are retained.
A dirty source tree is recorded as such and does not represent an exact-head
claim. Run `python3 scripts/test_qualify_buffering.py` to verify SIGTERM cleanup
with a child process. It checks that the error result and logs remain while
processes and temporary source/target data are removed. #79 owns CI
orchestration and invokes the qualification command on Ubuntu.

## Observed evidence and limits

| Fault | Selected assertion | Result |
| --- | --- | --- |
| Remove `DisconnectStream::drop` cancellation | Real-daemon `messages_disconnect_cancels_upstream`: upstream body must drop within five seconds of client disconnect | Caught; one runner mutant; both protocol baselines passed. Cargo stops after the first failing target, so this runner result proves Messages detection. The separate #464/#465 disposable run used `--no-fail-fast` and proved both protocol assertions. |
| Return `Ok(())` from `ReplayGate::ensure_can_attempt` | `proxy::stream::tests::no_replay_after_output_begun`: `gate.ensure_can_attempt().is_err()` | Caught; one baseline and one mutant test selected. This is a focused shared-pump contract test, not a real-daemon replay scenario. |
| Buffer upstream until the read loop ends | Real-daemon `provider_fixture_controls`: `response.created was not forwarded: deadline has elapsed; received: ""` | Caught; compiled and selected one test. The held tail prevents EOF, so first-frame forwarding must happen before it is released. |

The combined unmodified main baseline selected and passed one test in each of
`e2e_provider_fixture`, `e2e_responses`, and `e2e_messages`. The replay baseline
selected one test (87 filtered out). Neither a first buffering attempt rejected
by sandbox loopback permissions nor an invalid `--lib` baseline is counted as a
caught mutant. No unviable or process-timeout result is counted as test strength.

Current endpoint flows do not attempt a second upstream after streaming output.
Their request-count assertions show one observed request, but do not qualify a
future retry implementation. If streaming retries/failover are introduced, that
producer must add a real-daemon attempted-replay scenario; the focused gate test
alone is insufficient for that changed behavior. No duplicate endpoint or test
framework is added just to manufacture a replay path today.
