#!/usr/bin/env python3
"""Qualify the three reviewed M1 faults in one disposable source copy."""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile

import qualify_buffering as runner

FAULTS = {
    "early-terminal": (
        "--test",
        "e2e",
        "chat_stream_terminal_accounting",
        "pending stream must not be accounted",
    ),
    "incident-admission": (
        "--bin",
        "steve",
        "accounting::tests::failed_incident_publication_still_blocks_local_admission",
        "local admission reopened during durable publication retry",
    ),
    "premature-retirement": (
        "--test",
        "e2e_accounting",
        "journal_partial_tail_recovery",
        "retained journal",
    ),
}


def validate_result(result: dict, expected_sha: str) -> None:
    """Reject absent, dirty, mismatched, or unqualified fault outcomes."""
    if (
        result.get("source_head") != expected_sha
        or result.get("source_dirty") is not False
        or result.get("status") != "caught"
        or set(result.get("faults", {})) != set(FAULTS)
    ):
        raise runner.QualificationError(
            "M1 mutations require clean exact-head outcomes for all three faults"
        )
    for name, (_, _, test, assertion) in FAULTS.items():
        evidence = result["faults"][name]
        if not isinstance(evidence, dict) or not (
            evidence.get("status") == "caught"
            and evidence.get("test") == test
            and evidence.get("assertion") == assertion
            and evidence.get("baseline_exit_code") == 0
            and evidence.get("baseline_selected_tests") == 1
            and evidence.get("fault_build_exit_code") == 0
            and evidence.get("fault_exit_code") == 101
            and evidence.get("fault_selected_tests") == 1
            and evidence.get("expected_assertion_seen") is True
        ):
            raise runner.QualificationError(
                f"M1 mutation {name} lacks its named passing baseline and caught fault"
            )


def qualify(output: Path, expected_sha: str, check_patches: bool = False) -> int:
    """Run bounded baselines and faults; patch-only runs never qualify evidence."""
    repo = Path.cwd().resolve()
    output = output.resolve()
    if os.name != "posix" or output == repo or repo in output.parents:
        raise runner.QualificationError(
            "M1 mutation output must be outside the Unix checkout"
        )
    output.mkdir(parents=True, exist_ok=True)
    head, dirty = runner._source_identity(repo)
    result = {
        "source_head": head,
        "source_dirty": dirty,
        "status": "error",
        "faults": {},
    }
    try:
        if head != expected_sha or (dirty and not check_patches):
            raise runner.QualificationError(
                "M1 mutations require clean exact-head source"
            )
        with tempfile.TemporaryDirectory(
            prefix="steve-m1-mutations-", dir=output
        ) as temp:
            source = Path(temp) / "source"
            runner._copy_source(repo, source)
            env = os.environ.copy()
            env.pop("STEVE_TEST_BINARY", None)
            env.update(
                CARGO_TARGET_DIR=str(Path(temp) / "target"),
                CARGO_BUILD_JOBS="2",
                CARGO_NET_OFFLINE="true",
                CARGO_TERM_COLOR="never",
                NO_COLOR="1",
            )
            for name, (selector, target, test, assertion) in FAULTS.items():
                directory = output / name
                directory.mkdir(exist_ok=True)
                evidence = {"status": "not_run", "test": test, "assertion": assertion}
                result["faults"][name] = evidence
                patch = source / "tests" / "mutations" / f"m1-{name}.patch"
                if (
                    runner._check_patch(source, patch, directory / "patch-check.log")
                    != 0
                ):
                    raise runner.QualificationError(
                        f"{name}: reviewed patch does not apply"
                    )
                if check_patches:
                    continue
                command = [
                    "cargo",
                    "test",
                    "--locked",
                    "--offline",
                    "--all-features",
                    selector,
                    target,
                    test,
                    "--",
                    "--exact",
                    "--nocapture",
                ]
                baseline = runner._run(command, source, env, directory / "baseline.log")
                selected = runner._selected_test_count(directory / "baseline.log")
                evidence.update(
                    baseline_exit_code=baseline, baseline_selected_tests=selected
                )
                if baseline != 0 or selected != 1:
                    raise runner.QualificationError(
                        f"{name}: baseline did not pass exactly one test"
                    )
                if (
                    runner._run(
                        ["git", "apply", str(patch)],
                        source,
                        env,
                        directory / "apply.log",
                    )
                    != 0
                ):
                    raise runner.QualificationError(f"{name}: patch application failed")
                try:
                    build = runner._run(
                        [
                            "cargo",
                            "build",
                            "--locked",
                            "--offline",
                            "--all-features",
                            "--bin",
                            "steve",
                        ],
                        source,
                        env,
                        directory / "build.log",
                    )
                    evidence["fault_build_exit_code"] = build
                    if build != 0:
                        evidence["status"] = "unviable"
                        raise runner.QualificationError(
                            f"{name}: mutant did not compile"
                        )
                    code = runner._run(command, source, env, directory / "fault.log")
                    selected = runner._selected_test_count(directory / "fault.log")
                    log = (directory / "fault.log").read_text(errors="replace")
                    seen = (
                        assertion in log
                        and f"test {test} ... FAILED" in log
                        and "test result: FAILED" in log
                    )
                    evidence.update(
                        fault_exit_code=code,
                        fault_selected_tests=selected,
                        expected_assertion_seen=seen,
                    )
                    evidence["status"] = (
                        "caught"
                        if code == 101 and selected == 1 and seen
                        else "survived"
                        if code == 0 and selected == 1
                        else "error"
                    )
                    if evidence["status"] != "caught":
                        raise runner.QualificationError(
                            f"{name}: named fault was not caught by its assertion"
                        )
                finally:
                    if (
                        runner._run(
                            ["git", "apply", "--reverse", str(patch)],
                            source,
                            env,
                            directory / "restore.log",
                        )
                        != 0
                    ):
                        raise runner.QualificationError(
                            f"{name}: failed to restore disposable baseline"
                        )
        if runner._source_identity(repo) != (head, dirty):
            raise runner.QualificationError(
                "source identity changed during qualification"
            )
        result["status"] = "not_run" if check_patches else "caught"
        if not check_patches:
            validate_result(result, expected_sha)
        return 0
    except (runner.QualificationError, OSError, subprocess.SubprocessError) as error:
        result["error"] = str(error)
        result["status"] = "timeout" if "timed out" in str(error) else "error"
        if result["faults"]:
            current = next(reversed(result["faults"].values()))
            if current["status"] == "not_run":
                current["status"] = result["status"]
        print(str(error), file=sys.stderr)
        return 1
    finally:
        runner._write_result(output / "result.json", result)


def main() -> int:
    """Parse fixed qualification inputs or verify a retained exact-head artifact."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path)
    parser.add_argument("--expected-sha", required=True)
    parser.add_argument("--check-patches", action="store_true")
    parser.add_argument("--verify", type=Path)
    args = parser.parse_args()
    signal.signal(signal.SIGTERM, runner._handle_termination)
    if args.verify:
        validate_result(json.loads(args.verify.read_text()), args.expected_sha)
        return 0
    if args.output is None:
        parser.error("--output is required")
    return qualify(args.output, args.expected_sha, args.check_patches)


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (runner.QualificationError, OSError, ValueError) as error:
        print(f"M1 mutation qualification failed: {error}", file=sys.stderr)
        raise SystemExit(1)
