#!/usr/bin/env python3
"""Qualify the reviewed upstream buffering fault on Unix."""

from __future__ import annotations

import argparse
import json
import os
import re
import shutil
import signal
import subprocess
import sys
import tempfile
import time
from pathlib import Path
TIMEOUT_SECONDS = 600
TEST_NAME = "provider_fixture_controls"
ASSERTION = "response.created was not forwarded"
PATCH_PATH = Path("tests/mutations/buffer-until-eof.patch")


class QualificationError(RuntimeError):
    """Describe a qualification stage that did not meet its contract."""


def _stop_process_group(
    process: subprocess.Popen[bytes], grace_seconds: float = 2.0
) -> None:
    """Stop any children left in a command's process group."""
    try:
        os.killpg(process.pid, signal.SIGTERM)
    except ProcessLookupError:
        return

    deadline = time.monotonic() + grace_seconds
    while time.monotonic() < deadline:
        try:
            os.killpg(process.pid, 0)
        except ProcessLookupError:
            return
        time.sleep(0.05)

    try:
        os.killpg(process.pid, signal.SIGKILL)
    except ProcessLookupError:
        pass


def _run(
    command: list[str], cwd: Path, env: dict[str, str], log_path: Path
) -> int:
    """Run one bounded command, preserving combined output in its log."""
    with log_path.open("w", encoding="utf-8") as log_file:
        process = subprocess.Popen(
            command,
            cwd=cwd,
            env=env,
            stdout=log_file,
            stderr=subprocess.STDOUT,
            start_new_session=True,
        )
        timed_out = False
        try:
            try:
                return_code = process.wait(timeout=TIMEOUT_SECONDS)
            except subprocess.TimeoutExpired:
                timed_out = True
                _stop_process_group(process)
                return_code = process.wait()
        finally:
            _stop_process_group(process)
            process.wait()
        if timed_out:
            raise QualificationError(
                f"command timed out after {TIMEOUT_SECONDS} seconds: {command[0]}"
            )
        return return_code


def _ignore_source_entries(names: list[str]) -> set[str]:
    """Exclude generated or private paths from the disposable source copy."""
    return {
        name
        for name in names
        if name in {".git", ".cache", "target", "__pycache__"}
    }


def _copy_source(repo_root: Path, destination: Path) -> None:
    """Copy the checkout while excluding Git, build/cache and evidence data."""
    def ignore(directory: str, names: list[str]) -> set[str]:
        del directory
        return _ignore_source_entries(names)

    shutil.copytree(repo_root, destination, ignore=ignore)


def _check_patch(source_dir: Path, patch_path: Path, log_path: Path) -> int:
    """Check that the reviewed fault patch applies to the disposable source."""
    return _run(
        ["git", "apply", "--check", str(patch_path)],
        source_dir,
        os.environ.copy(),
        log_path,
    )


def _selected_test_count(log_path: Path) -> int:
    """Return Cargo's reported selected-test count, or zero if absent."""
    content = log_path.read_text(encoding="utf-8", errors="replace")
    matches = re.findall(r"^running (\d+) tests?$", content, flags=re.MULTILINE)
    return max((int(match) for match in matches), default=0)


def _source_identity(repo_root: Path) -> tuple[str, bool]:
    """Return the source HEAD and whether tracked or untracked changes exist."""
    head = subprocess.run(
        ["git", "rev-parse", "HEAD"],
        cwd=repo_root,
        check=True,
        capture_output=True,
        text=True,
        timeout=TIMEOUT_SECONDS,
    ).stdout.strip()
    dirty = bool(
        subprocess.run(
            ["git", "status", "--porcelain"],
            cwd=repo_root,
            check=True,
            capture_output=True,
            text=True,
            timeout=TIMEOUT_SECONDS,
        ).stdout
    )
    return head, dirty


def _write_result(path: Path, result: dict[str, object]) -> None:
    """Write the compact machine-readable qualification result."""
    path.write_text(json.dumps(result, sort_keys=True, separators=(",", ":")) + "\n")


def qualify(output_dir: Path) -> int:
    """Run the baseline and buffering-fault qualification."""
    if os.name != "posix":
        raise QualificationError("buffering qualification requires Unix")

    repo_root = Path.cwd().resolve()
    output_dir = output_dir.expanduser().resolve()
    if output_dir == repo_root or repo_root in output_dir.parents:
        raise QualificationError("--output must be outside the source checkout")
    output_dir.mkdir(parents=True, exist_ok=True)
    result_path = output_dir / "result.json"
    head, dirty = _source_identity(repo_root)
    result: dict[str, object] = {
        "source_head": head,
        "source_dirty": dirty,
        "status": "error",
    }

    with tempfile.TemporaryDirectory(prefix="steve-buffering-", dir=output_dir) as temp:
        run_root = Path(temp)
        source_dir = run_root / "source"
        target_dir = run_root / "target"
        _copy_source(repo_root, source_dir)
        patch_path = source_dir / PATCH_PATH
        cargo_env = os.environ.copy()
        cargo_env.update(
            {
                "CARGO_BUILD_JOBS": "2",
                "CARGO_NET_OFFLINE": "true",
                "CARGO_TARGET_DIR": str(target_dir),
                "CARGO_TERM_COLOR": "never",
                "NO_COLOR": "1",
                "TERM": "dumb",
            }
        )
        test_command = [
            "cargo",
            "test",
            "--offline",
            "--test",
            "e2e_provider_fixture",
            TEST_NAME,
            "--",
            "--exact",
            "--nocapture",
        ]

        try:
            patch_status = _check_patch(
                source_dir, patch_path, output_dir / "patch-check.log"
            )
            result["patch_check_exit_code"] = patch_status
            if patch_status != 0:
                raise QualificationError(
                    "reviewed buffering patch did not match source"
                )

            baseline_status = _run(
                test_command, source_dir, cargo_env, output_dir / "baseline.log"
            )
            baseline_selected = _selected_test_count(output_dir / "baseline.log")
            result["baseline_exit_code"] = baseline_status
            result["baseline_selected_tests"] = baseline_selected
            if baseline_status != 0 or baseline_selected != 1:
                raise QualificationError(
                    "unmodified baseline failed or did not select exactly one test"
                )

            apply_status = _run(
                ["git", "apply", str(patch_path)],
                source_dir,
                cargo_env,
                output_dir / "patch-apply.log",
            )
            result["patch_apply_exit_code"] = apply_status
            if apply_status != 0:
                raise QualificationError(
                    "reviewed buffering patch could not be applied"
                )

            build_status = _run(
                ["cargo", "build", "--offline", "--bin", "steve"],
                source_dir,
                cargo_env,
                output_dir / "fault-build.log",
            )
            result["fault_build_exit_code"] = build_status
            if build_status != 0:
                raise QualificationError("Steve binary rebuild failed after patch")

            fault_status = _run(
                test_command, source_dir, cargo_env, output_dir / "fault.log"
            )
            fault_selected = _selected_test_count(output_dir / "fault.log")
            fault_log = (output_dir / "fault.log").read_text(
                encoding="utf-8", errors="replace"
            )
            result["fault_exit_code"] = fault_status
            result["fault_selected_tests"] = fault_selected
            result["expected_assertion_seen"] = ASSERTION in fault_log
            if (
                fault_status != 101
                or fault_selected != 1
                or f"test {TEST_NAME} ... FAILED" not in fault_log
                or "test result: FAILED" not in fault_log
                or ASSERTION not in fault_log
            ):
                raise QualificationError(
                    "buffering fault did not produce the expected selected-test failure"
                )

            result["status"] = "caught"
            result["message"] = ASSERTION
            _write_result(result_path, result)
            return 0
        except (QualificationError, OSError) as error:
            result["error"] = str(error)
            _write_result(result_path, result)
            print(f"buffering qualification failed: {error}", file=sys.stderr)
            return 1


def main(argv: list[str] | None = None) -> int:
    """Parse the command line and run qualification."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--output",
        required=True,
        type=Path,
        help="external directory for retained evidence",
    )
    args = parser.parse_args(argv)
    try:
        return qualify(args.output)
    except (QualificationError, OSError, subprocess.SubprocessError) as error:
        print(f"buffering qualification failed: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
