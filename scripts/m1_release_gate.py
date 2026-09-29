#!/usr/bin/env python3
"""Run Steve's fixed M1 evidence set in a browser or headlessly."""

import argparse
from datetime import datetime, timezone
import hashlib
import importlib.metadata
import json
import os
from pathlib import Path
import platform
import secrets
import select
import socket
import signal
import stat
import subprocess
import sys
import tempfile
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import webbrowser

from qualify_m1_mutations import validate_result as validate_mutations


OUTPUT_LIMIT = 64 * 1024
DEFAULT_STEP_TIMEOUT = 180
COMMAND_TIMEOUT = 30
REQUIRED_HOSTED_CHECKS = {"mutation", "m1-mutation"}
TARGET_PROBE = "steve-m1-target-v1"
TARGET_CHECKS = {
    "exclusive_lock",
    "atomic_publication",
    "append_visibility",
    "file_sync",
    "directory_sync",
    "revisioned_publication",
    "process_death_lock_release",
}


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def captured_command(command, cwd, timeout=COMMAND_TIMEOUT, env=None, accepted=(0,), owner=None):
    if owner is not None and owner._cancelled.is_set():
        raise KeyboardInterrupt
    process = subprocess.Popen(
        [str(part) for part in command],
        cwd=cwd,
        env=env,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        start_new_session=os.name == "posix",
    )
    if owner is not None:
        owner._process_started(process)
    try:
        stdout, stderr = process.communicate(timeout=timeout)
    except subprocess.TimeoutExpired as error:
        stop_process_group(process)
        try:
            process.communicate(timeout=2)
        except subprocess.TimeoutExpired:
            pass
        raise RuntimeError(f"command timed out after {timeout}s: {command}") from error
    finally:
        try:
            stop_process_group(process)
        finally:
            if owner is not None:
                owner._process_finished(process)
    if owner is not None and owner._cancelled.is_set():
        raise KeyboardInterrupt
    require(
        process.returncode in accepted,
        stderr.strip() or stdout.strip() or f"command failed: {command}",
    )
    return stdout


def command_output(command, cwd, timeout=COMMAND_TIMEOUT, env=None, owner=None):
    return captured_command(command, cwd, timeout, env, owner=owner).strip()


def candidate_evidence(repo, expected_sha, owner=None):
    source_sha = command_output(["git", "rev-parse", "HEAD"], repo, owner=owner)
    require(source_sha == expected_sha, f"candidate SHA {source_sha} does not match {expected_sha}")
    dirty = command_output(
        ["git", "status", "--porcelain", "--untracked-files=all"], repo, owner=owner
    )
    require(not dirty, "candidate source must be clean")
    captured_command(
        ["git", "ls-files", "--error-unmatch", "Cargo.lock"], repo, owner=owner
    )
    return {
        "source_sha": source_sha,
        "source_dirty": False,
        "lock_tracked": True,
    }


def stop_process_group(process):
    if os.name == "posix":
        try:
            os.killpg(process.pid, signal.SIGTERM)
        except OSError:
            pass
        if process.poll() is None:
            try:
                process.wait(timeout=2)
            except subprocess.TimeoutExpired:
                os.killpg(process.pid, signal.SIGKILL)
                try:
                    process.wait(timeout=2)
                except subprocess.TimeoutExpired as error:
                    raise RuntimeError("failed to reap process after SIGKILL") from error
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except OSError:
            pass
        return
    if process.poll() is None:
        process.terminate()
        try:
            process.wait(timeout=2)
        except subprocess.TimeoutExpired:
            process.kill()
            try:
                process.wait(timeout=2)
            except subprocess.TimeoutExpired as error:
                raise RuntimeError("failed to reap process after kill") from error


def run_step(name, command, cwd, timeout, env, process_started=None, process_finished=None):
    started = time.monotonic()
    process = subprocess.Popen(
        [str(part) for part in command],
        cwd=cwd,
        env=env,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        start_new_session=os.name == "posix",
    )
    if process_started:
        process_started(process)
    timed_out = False
    try:
        output, _ = process.communicate(timeout=timeout)
    except subprocess.TimeoutExpired as error:
        timed_out = True
        stop_process_group(process)
        output = error.output or ""
        try:
            output, _ = process.communicate(timeout=2)
        except subprocess.TimeoutExpired as cleanup_error:
            output = cleanup_error.output or output
    finally:
        try:
            stop_process_group(process)
        finally:
            if process_finished:
                process_finished(process)
    selected_nothing = (
        "test" in [str(part) for part in command[1:]]
        and "--exact" in [str(part) for part in command]
        and "running 0 tests" in output
    )
    result = {
        "name": name,
        "command": [str(part) for part in command],
        "status": "PASS"
        if process.returncode == 0 and not timed_out and not selected_nothing
        else "FAIL",
        "exit_code": process.returncode,
        "timed_out": timed_out,
        "duration_seconds": round(time.monotonic() - started, 3),
        "output": output[-OUTPUT_LIMIT:],
    }
    if selected_nothing:
        result["error"] = "exact filter selected no tests"
    return result


def json_command(command, cwd, env=None, accepted=(0,), timeout=COMMAND_TIMEOUT, owner=None):
    return json.loads(captured_command(command, cwd, timeout, env, accepted, owner))


def hosted_qualification(repo, source_sha, env=None, owner=None):
    url = None
    pull_request = head_after = None
    try:
        pull_request = json_command(
            [
                "gh",
                "pr",
                "view",
                "619",
                "--repo",
                "djh00t/steve",
                "--json",
                "headRefOid,url",
            ],
            repo,
            env,
            owner=owner,
        )
        require(isinstance(pull_request, dict), "PR head response must be an object")
        head = pull_request["headRefOid"]
        url = pull_request["url"]
    except (OSError, RuntimeError, KeyError, json.JSONDecodeError) as error:
        return {
            "status": "UNKNOWN",
            "source_sha": source_sha,
            "pull_request": 619,
            "url": url,
            "head_before": pull_request,
            "head_after": head_after,
            "error": str(error),
        }
    if head != source_sha:
        return {
            "status": "FAIL",
            "source_sha": head,
            "expected_sha": source_sha,
            "pull_request": 619,
            "url": url,
            "head_before": pull_request,
            "head_after": head_after,
            "error": "PR #619 head does not match the candidate",
        }
    try:
        checks = json_command(
            [
                "gh",
                "pr",
                "checks",
                "619",
                "--repo",
                "djh00t/steve",
                "--required",
                "--json",
                "bucket,name,state,link,workflow",
            ],
            repo,
            env,
            (0, 1, 8),
            owner=owner,
        )
    except (OSError, RuntimeError, KeyError, json.JSONDecodeError) as error:
        return {
            "status": "UNKNOWN",
            "source_sha": source_sha,
            "pull_request": 619,
            "url": url,
            "head_before": pull_request,
            "head_after": head_after,
            "error": str(error),
        }
    try:
        head_after = json_command(
            ["gh", "pr", "view", "619", "--repo", "djh00t/steve", "--json", "headRefOid,url"],
            repo, env, owner=owner,
        )
        require(isinstance(head_after, dict), "PR head response must be an object")
        if head_after.get("headRefOid") != head:
            return {
                "status": "FAIL", "source_sha": source_sha, "pull_request": 619,
                "url": url, "head_before": pull_request, "head_after": head_after,
                "required_checks": checks, "error": "PR head changed while reading checks",
            }
    except (OSError, RuntimeError, KeyError, json.JSONDecodeError) as error:
        return {
            "status": "UNKNOWN", "source_sha": source_sha, "pull_request": 619,
            "url": url, "head_before": pull_request, "head_after": head_after,
            "error": str(error),
        }
    try:
        require(
            isinstance(checks, list) and all(isinstance(check, dict) for check in checks),
            "required checks response must be an array of objects",
        )
    except RuntimeError as error:
        return {
            "status": "UNKNOWN",
            "source_sha": source_sha,
            "pull_request": 619,
            "url": url,
            "head_before": pull_request,
            "head_after": head_after,
            "error": str(error),
        }
    missing = sorted(REQUIRED_HOSTED_CHECKS - {check.get("name") for check in checks})
    if not checks or missing:
        status = "UNKNOWN"
    elif all(check.get("bucket") == "pass" for check in checks):
        status = "PASS"
    elif any(check.get("bucket") in {"fail", "cancel"} for check in checks):
        status = "FAIL"
    else:
        status = "UNKNOWN"
    return {
        "status": status,
        "source_sha": head,
        "pull_request": 619,
        "url": url,
        "required_checks": checks,
        "head_before": pull_request,
        "head_after": head_after,
        "missing_required_checks": missing,
    }


def filesystem_type(path):
    if sys.platform == "darwin":
        device = path.stat().st_dev
        for line in command_output(["mount"], path).splitlines():
            mounted, separator, options = line.rpartition(" (")
            _, on, mountpoint = mounted.partition(" on ")
            if not separator or not on:
                continue
            try:
                same_device = Path(mountpoint).stat().st_dev == device
            except OSError:
                continue
            if same_device:
                return options.rstrip(")").split(",", 1)[0]
        raise RuntimeError(f"cannot identify filesystem for {path}")
    return command_output(["stat", "-f", "-c", "%T", str(path)], path)


def sync_directory(path):
    flags = os.O_RDONLY | getattr(os, "O_DIRECTORY", 0)
    descriptor = os.open(path, flags)
    try:
        os.fsync(descriptor)
    finally:
        os.close(descriptor)


def normalized_deployment_path(deployment_parent, deployment_path):
    require(deployment_path.is_absolute(), "intended deployment path must be absolute")
    normalized = Path(os.path.normpath(str(deployment_path)))
    try:
        relative = normalized.relative_to(deployment_parent)
    except ValueError as error:
        raise RuntimeError("intended deployment path must be beneath target parent") from error
    require(relative.parts, "intended deployment path must be beneath target parent")
    return normalized


def qualify_target(deployment_parent, deployment_path, source_sha, binary):
    require(deployment_parent.is_absolute(), "target deployment parent must be absolute")
    requested_parent = Path(os.path.normpath(str(deployment_parent)))
    deployment_path = normalized_deployment_path(requested_parent, deployment_path)
    deployment_parent = requested_parent.resolve(strict=True)
    require(
        deployment_parent.is_dir(),
        f"target deployment parent is not a directory: {deployment_parent}",
    )
    require(binary.is_file(), f"candidate binary is missing: {binary}")
    fs_type = filesystem_type(deployment_parent).lower()
    require(
        not any(name in fs_type for name in ("nfs", "smb", "cifs", "afp", "sshfs", "9p")),
        f"shared/network filesystem is unsupported: {fs_type}",
    )
    checks = {}
    processes = {}
    target_device = deployment_parent.stat().st_dev
    relative_path = deployment_path.relative_to(requested_parent)
    canonical_deployment_path = deployment_parent / relative_path
    current = deployment_parent
    intended_device = target_device
    for index, part in enumerate(relative_path.parts):
        current /= part
        try:
            metadata = current.lstat()
        except FileNotFoundError:
            break
        require(not stat.S_ISLNK(metadata.st_mode), f"intended deployment path contains symlink: {current}")
        require(metadata.st_dev == target_device, "intended deployment path crosses filesystems")
        require(
            stat.S_ISDIR(metadata.st_mode),
            f"intended deployment path component is not a directory: {current}",
        )
        if index == len(relative_path.parts) - 1:
            intended_device = metadata.st_dev
    with tempfile.TemporaryDirectory(
        prefix="steve-m1-target-probe-", dir=deployment_parent
    ) as temp:
        probe = Path(temp)
        require(probe != deployment_path, "probe path must differ from intended deployment path")
        require(not any(probe.iterdir()), "target probe directory must start empty")
        probe_device = probe.stat().st_dev
        require(probe_device == target_device, "target probe is not on the target filesystem")
        lock_path = probe / "owner.lock"
        import fcntl

        with lock_path.open("w") as owner, lock_path.open("w") as contender:
            fcntl.flock(owner, fcntl.LOCK_EX | fcntl.LOCK_NB)
            try:
                fcntl.flock(contender, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except BlockingIOError:
                checks["exclusive_lock"] = "PASS"
            else:
                raise RuntimeError("exclusive lock admitted a second owner")

        staging = probe / "state.tmp"
        published = probe / "state.json"
        with staging.open("w", encoding="utf-8") as output:
            json.dump({"revision": 1, "coverage": [["generation-1", 1, "digest-1"]]}, output)
            output.flush()
            os.fsync(output.fileno())
        checks["file_sync"] = "PASS"
        os.replace(staging, published)
        checks["atomic_publication"] = "PASS"
        sync_directory(probe)
        checks["directory_sync"] = "PASS"

        journal = probe / "journal"
        with journal.open("ab") as output:
            output.write(b"frame-1\n")
            output.flush()
            os.fsync(output.fileno())
        require(journal.read_bytes() == b"frame-1\n", "append was not visible")
        checks["append_visibility"] = "PASS"

        stale = json.loads(published.read_text(encoding="utf-8"))
        replacement = probe / "state.next"
        with replacement.open("w", encoding="utf-8") as output:
            json.dump({"revision": 2, "coverage": [["generation-1", 2, "digest-2"]]}, output)
            output.flush()
            os.fsync(output.fileno())
        os.replace(replacement, published)
        sync_directory(probe)
        current = json.loads(published.read_text(encoding="utf-8"))
        require(stale["revision"] == 1 and current["revision"] == 2, "revision publication failed")
        require(stale["coverage"] != current["coverage"], "coverage publication did not change")
        checks["revisioned_publication"] = "PASS"

        ready = probe / "child.ready"
        child_code = (
            "import fcntl, pathlib, sys, time; "
            "f=open(sys.argv[1],'w'); fcntl.flock(f,fcntl.LOCK_EX); "
            "pathlib.Path(sys.argv[2]).write_text('ready'); time.sleep(60)"
        )
        child = subprocess.Popen(
            [sys.executable, "-c", child_code, str(lock_path), str(ready)],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            start_new_session=True,
        )
        try:
            deadline = time.monotonic() + 5
            while not ready.exists():
                require(child.poll() is None, "lock probe child exited before readiness")
                require(time.monotonic() < deadline, "lock probe child readiness timed out")
                time.sleep(0.02)
        finally:
            stop_process_group(child)
        processes["lock_holder_process_death"] = {
            "status": "PASS" if child.poll() is not None else "FAIL",
            "exit_code": child.returncode,
            "terminated_by_probe": True,
        }
        with lock_path.open("w") as successor:
            fcntl.flock(successor, fcntl.LOCK_EX | fcntl.LOCK_NB)
        checks["process_death_lock_release"] = "PASS"

    require(set(checks) == TARGET_CHECKS, "target probe did not execute every check")
    return {
        "probe": TARGET_PROBE,
        "status": "PASS",
        "source_sha": source_sha,
        "binary_path": str(binary.resolve()),
        "binary_sha256": sha256(binary),
        "deployment_path": str(deployment_path),
        "canonical_deployment_path": str(canonical_deployment_path),
        "requested_target_parent": str(requested_parent),
        "target_parent": str(deployment_parent),
        "probe_path": str(probe),
        "observed_at": datetime.now(timezone.utc).isoformat(),
        "platform": platform.platform(),
        "filesystem": fs_type,
        "target_device": target_device,
        "intended_device": intended_device,
        "probe_device": probe_device,
        "tool_versions": {
            "python": sys.version,
            "rustc": command_output(["rustc", "-Vv"], deployment_parent),
            "cargo": command_output(
                [os.environ.get("CARGO", "cargo"), "--version"], deployment_parent
            ),
        },
        "processes": processes,
        "checks": checks,
    }


def load_target_qualification(path, source_sha, binary_hash, deployment_path):
    with path.open(encoding="utf-8") as source:
        value = json.load(source)
    require(isinstance(value, dict), f"qualification must be an object: {path}")
    require(value.get("probe") == TARGET_PROBE, "target qualification probe is missing or unknown")
    for name in ("source_sha", "binary_sha256", "binary_path", "deployment_path",
                 "canonical_deployment_path", "requested_target_parent", "target_parent",
                 "probe_path", "platform", "filesystem"):
        require(isinstance(value.get(name), str) and value[name].strip(),
                f"target qualification {name} must be a nonempty string")
    for name in ("binary_path", "deployment_path", "canonical_deployment_path",
                 "requested_target_parent", "target_parent", "probe_path"):
        require(Path(value[name]).is_absolute(), f"target qualification {name} must be absolute")
    for name in ("target_device", "intended_device", "probe_device"):
        require(type(value.get(name)) is int and value[name] >= 0,
                f"target qualification {name} must be a device identity")
    require(len(value["binary_sha256"]) == 64
            and all(c in "0123456789abcdef" for c in value["binary_sha256"]),
            "target qualification binary hash is invalid")
    require(value.get("source_sha") == source_sha, "target qualification SHA does not match")
    require(value.get("binary_sha256") == binary_hash, "target qualification binary does not match")
    require(
        value.get("deployment_path") == str(deployment_path),
        "target qualification deployment path does not match",
    )
    require(
        value.get("target_device")
        == value.get("intended_device")
        == value.get("probe_device"),
        "target qualification probe is on a different filesystem",
    )
    require(
        value.get("probe_path") not in {str(deployment_path), value.get("canonical_deployment_path"), value.get("target_parent")},
        "target qualification used the deployment path as its probe",
    )
    require(
        isinstance(value.get("observed_at"), str) and value["observed_at"],
        "target qualification timestamp is missing",
    )
    try:
        datetime.fromisoformat(value["observed_at"].replace("Z", "+00:00"))
    except ValueError as error:
        raise RuntimeError("target qualification timestamp is invalid") from error
    tools = value.get("tool_versions")
    require(
        isinstance(tools, dict)
        and all(isinstance(tools.get(name), str) and tools[name] for name in ("python", "rustc", "cargo")),
        "target qualification tool versions are incomplete",
    )
    processes = value.get("processes")
    require(
        isinstance(processes, dict)
        and processes.get("lock_holder_process_death", {}).get("status") == "PASS",
        "target qualification process evidence is incomplete",
    )
    target_parent = Path(value.get("target_parent", ""))
    requested_parent = Path(value.get("requested_target_parent", ""))
    probe_path = Path(value.get("probe_path", ""))
    try:
        probe_path.relative_to(target_parent)
        relative_path = deployment_path.relative_to(requested_parent)
    except ValueError as error:
        raise RuntimeError("target qualification paths are not bound to the target parent") from error
    require(
        value.get("canonical_deployment_path") == str(target_parent / relative_path),
        "target qualification canonical deployment path does not match",
    )
    checks = value.get("checks")
    require(isinstance(checks, dict) and set(checks) == TARGET_CHECKS, "target qualification probe checks are incomplete")
    require(all(result == "PASS" for result in checks.values()), "target qualification contains a failed check")
    require(value.get("status") == "PASS", "target qualification did not pass")
    return value


def verdicts(steps, hosted, target, source_sha, adoption=None, mutations=None):
    functional = "PASS" if steps and all(step["status"] == "PASS" for step in steps) else "FAIL"
    eligible = functional == "PASS" and all(
        evidence is not None
        and evidence.get("status") == "PASS"
        and evidence.get("source_sha") == source_sha
        for evidence in (hosted, target, mutations)
    )
    if adoption and adoption.get("status") == "PRESENT":
        eligible = eligible and adoption.get("external_stop_verified") is True
    return functional, eligible


def load_mutation_qualification(path, source_sha):
    """Load only complete, clean, exact-candidate M1 mutation outcomes."""
    value = json.loads(path.read_text(encoding="utf-8"))
    require(isinstance(value, dict), "M1 mutation evidence must be an object")
    validate_mutations(value, source_sha)
    return {"status": "PASS", "source_sha": source_sha, "outcomes": value}


def write_evidence(path, evidence):
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_suffix(path.suffix + ".tmp")
    temporary.write_text(json.dumps(evidence, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    temporary.replace(path)


def sha256(path):
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def version(command, cwd, owner=None):
    return command_output(command, cwd, owner=owner)


def installed_sdks():
    return {
        package: importlib.metadata.version(package)
        for package in ("anthropic", "openai")
    }


def candidate_environment(binary):
    env = os.environ.copy()
    env["PYTHONOPTIMIZE"] = "1"
    env["STEVE_TEST_BINARY"] = str(binary.resolve())
    return env


def postgres_prerequisite(env):
    if env.get("STEVE_TEST_POSTGRES_URL", "").strip():
        return None
    return {
        "name": "postgres_replay_prerequisite",
        "status": "UNKNOWN",
        "error": "STEVE_TEST_POSTGRES_URL is required for PostgreSQL qualification",
    }


def adoption_evidence(deployment_path):
    """Discover Rust adoption evidence or fail closed when explicitly required."""
    configured = os.environ.get("M1_ADOPTION_MANIFEST")
    path = Path(configured) if configured else deployment_path / "adoption.json"
    # A retained digest is also evidence that adoption is expected, even if JSON vanished.
    if configured or path.exists() or path.is_symlink() or path.with_suffix(".json.sha256").exists():
        return load_adoption_manifest(path)
    return {"status": "NOT_APPLICABLE"}


def load_adoption_manifest(path):
    """Verify the persisted Rust manifest and its unauthenticated assertion."""
    data = path.read_bytes()
    require(path.with_suffix(".json.sha256").read_text().strip() == hashlib.sha256(data).hexdigest(),
            "adoption manifest digest does not match")
    manifest = json.loads(data)
    require(isinstance(manifest, dict), "adoption manifest must be an object")
    fields = manifest.get("maintenance_assertion")
    require(isinstance(fields, dict), "maintenance_assertion must be an object")
    required = ("workload_identity", "host", "source", "observed_at", "command_or_exported_status")
    for name in (*required, "path", "digest", "assertion_kind"):
        require(isinstance(fields.get(name), str) and fields[name].strip(),
                f"maintenance assertion is missing {name}")
    require(fields["assertion_kind"] == "operator_supplied_unauthenticated", "unknown assertion kind")
    require(fields.get("stopped") is True and fields.get("restart_disabled") is True,
            "maintenance assertion stop/disable claim is incomplete")
    require(manifest.get("source") == fields["source"], "maintenance assertion source does not match")
    try:
        observed_at = datetime.fromisoformat(fields["observed_at"].replace("Z", "+00:00"))
    except ValueError as error:
        raise RuntimeError("maintenance assertion observed_at is invalid") from error
    require(observed_at.tzinfo is not None, "maintenance assertion observed_at requires a timezone")
    digest = fields["digest"]
    require(len(digest) == 64 and all(c in "0123456789abcdef" for c in digest),
            "maintenance assertion digest is invalid")
    assertion_path = Path(fields["path"])
    require(assertion_path.is_absolute(), "maintenance assertion path must be absolute")
    assertion_bytes = assertion_path.read_bytes()
    require(hashlib.sha256(assertion_bytes).hexdigest() == digest,
            "maintenance assertion digest does not match")
    assertion_fields = json.loads(assertion_bytes)
    names = (*required, "stopped", "restart_disabled")
    require(isinstance(assertion_fields, dict)
            and all(type(assertion_fields.get(name)) is type(fields[name])
                    and assertion_fields[name] == fields[name] for name in names),
            "maintenance assertion manifest fields do not match the digested assertion")
    return {
        "status": "PRESENT", "manifest_path": str(path.resolve()),
        "assertion_path": str(assertion_path), "digest": digest,
        "fields": {name: fields[name] for name in names}, "source": "operator_supplied",
        "external_stop_verified": False,
        "operator_prerequisite": "External legacy-writer stop/disable remains unverified",
    }


def fixed_steps(binary, accounting_root):
    python = sys.executable
    cargo = os.environ.get("CARGO", "cargo")
    steps = [
        (
            "accounting_provision",
            [binary, "accounting", "provision", "--root", accounting_root],
        ),
        (
            "official_sdks",
            [python, "-B", "scripts/sdk_smoke.py", "--provider", "all", "--binary", binary],
        ),
    ]
    accounting = (
        "fresh_provision_and_missing_evidence_fail_closed",
        "same_replica_replacement_preserves_accounting_ownership",
        "legacy_journal_adoption_is_offline_and_resumable",
        "journal_partial_tail_recovery",
        "accounting_reconciles_after_db_recovery",
        "journal_partial_commit_replay",
        "postgres_replay_detects_conflicting_duplicate",
        "accounting_incident_preserves_forwarding_and_restart_evidence",
        "accounting_disposition_is_auditable_and_publicly_redacted",
        "accounting_conflict_recovery_is_offline_and_verified",
        "accounting_shutdown_barriers_complete",
        "accounting_shutdown_timeout_survives_restart",
        "chat_accounting_does_not_delay_response",
    )
    chat = (
        "chat_nonstream_accounting",
        "chat_stream_terminal_accounting",
        "chat_completions_stream_forwards_first_event_before_tail",
        "chat_disconnect_no_replay",
        "inference_saturation_keeps_management_live",
    )
    integrations = (
        ("e2e_accounting", accounting),
        ("e2e", chat),
        ("e2e_responses", ("responses_disconnect_cancels_upstream",)),
        ("e2e_messages", ("messages_disconnect_cancels_upstream",)),
    )
    for target, names in integrations:
        for name in names:
            steps.append(
                (
                    name,
                    [
                        cargo,
                        "test",
                        "--locked",
                        "--all-features",
                        "--test",
                        target,
                        name,
                        "--",
                        "--exact",
                        "--nocapture",
                    ],
                )
            )
    unit = (
        "server::tests::get_v1_models_lists_configured_models",
        "server::tests::get_v1_models_uses_static_catalogue_when_unconfigured",
        "server::tests::provider_health_reports_reachable_closed_and_unconfigured",
        "server::tests::provider_health_reports_fixture_and_unconfigured_as_healthy",
        "server::tests::provider_health_rejects_missing_and_server_error_provider_routes",
        "server::tests::provider_health_times_out_without_blocking_other_management_routes",
        "server::tests::provider_health_returns_busy_while_another_probe_is_running",
    )
    for name in unit:
        steps.append(
            (
                name,
                [
                    cargo,
                    "test",
                    "--locked",
                    "--all-features",
                    "--bin",
                    "steve",
                    name,
                    "--",
                    "--exact",
                    "--nocapture",
                ],
            )
        )
    return tuple(steps)


class GateRun:
    def __init__(self, repo, expected_sha, output, timeout, deployment_path):
        self.repo = repo
        self.expected_sha = expected_sha
        self.output = output
        self.timeout = timeout
        self.deployment_path = deployment_path
        self._lock = threading.Lock()
        self._started = False
        self._cancelled = threading.Event()
        self._active_process = None
        self.result = None

    def cancel(self):
        self._cancelled.set()
        with self._lock:
            process = self._active_process
            self._active_process = None
        if process is not None:
            stop_process_group(process)

    def _process_started(self, process):
        with self._lock:
            cancelled = self._cancelled.is_set()
            if not cancelled:
                self._active_process = process
        if cancelled:
            stop_process_group(process)

    def _process_finished(self, process):
        with self._lock:
            if self._active_process is process:
                self._active_process = None

    def _run_step(self, name, command, env):
        result = run_step(
            name,
            command,
            self.repo,
            self.timeout,
            env,
            self._process_started,
            self._process_finished,
        )
        if self._cancelled.is_set():
            raise KeyboardInterrupt
        return result

    def run_once(self):
        with self._lock:
            if self._started:
                raise RuntimeError("M1 gate already ran")
            self._started = True
        try:
            self.result = self._run()
            if self._cancelled.is_set():
                raise KeyboardInterrupt
            return self.result
        except KeyboardInterrupt:
            self.result = failure_evidence(self.expected_sha, "M1 release gate interrupted")
            write_evidence(self.output, self.result)
            raise
        except Exception as error:
            self.result = failure_evidence(self.expected_sha, str(error))
            write_evidence(self.output, self.result)
            return self.result

    def _run(self):
        evidence = {
            "source_sha": self.expected_sha,
            "functional": "FAIL",
            "release_eligible": False,
            "operator_acceptance_required": True,
            "observed_at": datetime.now(timezone.utc).isoformat(),
            "steps": [],
            "limits": {
                "process_crash": "covered by the integrated scenario suite",
                "os_crash": "unknown",
                "power_loss": "unknown",
            },
        }
        env = os.environ.copy()
        env["PYTHONOPTIMIZE"] = "1"
        try:
            evidence.update(candidate_evidence(self.repo, self.expected_sha, owner=self))
            target_dir = self.repo / "target" / f"m1-candidate-{self.expected_sha[:12]}"
            binary = target_dir / "debug" / ("steve.exe" if os.name == "nt" else "steve")
            build = self._run_step(
                "build_candidate",
                [
                    os.environ.get("CARGO", "cargo"),
                    "build",
                    "--locked",
                    "--all-features",
                    "--target-dir",
                    target_dir,
                ],
                env,
            )
            evidence["steps"].append(build)
            require(build["status"] == "PASS", "candidate build failed")
            require(binary.is_file(), f"candidate binary missing: {binary}")
            candidate_evidence(self.repo, self.expected_sha, owner=self)
            env = candidate_environment(binary)
            postgres = postgres_prerequisite(env)
            if postgres:
                evidence["steps"].append(postgres)
                raise RuntimeError(postgres["error"])
            evidence.update(
                {
                    "binary_path": str(binary.resolve()),
                    "binary_sha256": sha256(binary),
                    "rustc": version(["rustc", "-Vv"], self.repo, owner=self),
                    "cargo": version([os.environ.get("CARGO", "cargo"), "--version"], self.repo, owner=self),
                    "python": sys.version,
                    "sdks": installed_sdks(),
                }
            )
            evidence["legacy_adoption"] = adoption_evidence(self.deployment_path)
            with tempfile.TemporaryDirectory(prefix="steve-m1-accounting-") as temp:
                accounting_root = Path(temp).resolve() / "accounting"
                for name, command in fixed_steps(binary, accounting_root):
                    evidence["steps"].append(self._run_step(name, command, env))
            evidence["provisioning"] = next(
                step for step in evidence["steps"] if step["name"] == "accounting_provision"
            )
        except Exception as error:
            evidence["steps"].append(
                {"name": "gate_preflight", "status": "FAIL", "error": str(error)}
            )

        hosted = hosted_qualification(self.repo, self.expected_sha, env, owner=self)
        target_path = self.repo / "target" / "m1-target-qualification.json"
        try:
            target = load_target_qualification(
                target_path,
                self.expected_sha,
                evidence.get("binary_sha256", ""),
                self.deployment_path,
            )
        except (OSError, RuntimeError, ValueError, json.JSONDecodeError) as error:
            target = {
                "status": "UNKNOWN",
                "source_sha": self.expected_sha,
                "path": str(target_path),
                "error": str(error),
            }
        mutation_path = self.repo / "target" / "m1-mutation-result.json"
        try:
            mutations = load_mutation_qualification(mutation_path, self.expected_sha)
        except (OSError, RuntimeError, ValueError, TypeError) as error:
            mutations = {"status": "UNKNOWN", "source_sha": self.expected_sha,
                         "path": str(mutation_path), "error": str(error)}
        functional, eligible = verdicts(
            evidence["steps"],
            hosted,
            target,
            self.expected_sha,
            evidence.get("legacy_adoption"),
            mutations,
        )
        evidence.update(
            {
                "functional": functional,
                "release_eligible": eligible,
                "hosted_evidence": hosted,
                "mutation_qualification": mutations,
                "target_qualification": target,
                "operator_acceptance_required": True,
            }
        )
        write_evidence(self.output, evidence)
        return evidence


def page_html(token, state):
    del state
    token_json = json.dumps(token)
    return f"""<!doctype html>
<html lang="en"><meta charset="utf-8"><meta name="viewport" content="width=device-width">
<title>Steve M1 release gate</title>
<style>body{{font:16px system-ui;max-width:72rem;margin:2rem auto;padding:0 1rem}}button{{padding:.7rem 1rem}}pre{{white-space:pre-wrap;background:#111;color:#eee;padding:1rem}}</style>
<h1>Steve M1 release gate</h1>
<p>Runs the fixed local candidate checks once. The result separates functional behavior from release eligibility.</p>
<button id="run" type="button">Run M1 gate</button><pre id="output">Ready.</pre>
<script>
const output = document.getElementById('output');
document.getElementById('run').addEventListener('click', async () => {{
  output.textContent = 'Running…';
  const response = await fetch('/run', {{method:'POST', headers:{{'X-M1-Capability':{token_json}, 'Content-Type':'application/json'}}, body:'{{}}'}});
  const result = await response.json();
  output.textContent = JSON.stringify(result, null, 2);
}});
</script></html>"""


def create_server(gate_run):
    token = secrets.token_urlsafe(32)
    run_lock = threading.Lock()
    run_started = False

    class Handler(BaseHTTPRequestHandler):
        server_version = "SteveM1Gate/1"

        def log_message(self, fmt, *args):
            return

        def reject(self, status, message):
            payload = json.dumps({"error": message}).encode()
            self.send_response(status)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)

        def valid_host(self):
            expected = f"127.0.0.1:{self.server.server_port}"
            return self.headers.get("Host") == expected

        def do_GET(self):
            if not self.valid_host():
                self.reject(403, "foreign host")
                return
            if self.path == "/run":
                self.reject(405, "state changes require POST")
                return
            if self.path != "/":
                self.reject(404, "not found")
                return
            payload = page_html(token, None).encode()
            self.send_response(200)
            self.send_header("Content-Type", "text/html; charset=utf-8")
            self.send_header("Content-Security-Policy", "default-src 'self'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; connect-src 'self'")
            self.send_header("X-Content-Type-Options", "nosniff")
            self.send_header("Content-Length", str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)

        def do_POST(self):
            nonlocal run_started
            host = f"127.0.0.1:{self.server.server_port}"
            if (
                self.path != "/run"
                or not self.valid_host()
                or self.headers.get("Origin") != f"http://{host}"
                or not secrets.compare_digest(self.headers.get("X-M1-Capability", ""), token)
            ):
                self.reject(403, "foreign request")
                return
            try:
                length = int(self.headers.get("Content-Length", "0"))
            except ValueError:
                self.reject(400, "invalid content length")
                return
            if length > 64:
                self.reject(400, "request body too large")
                return
            try:
                require(json.loads(self.rfile.read(length) or b"{}") == {}, "body must be empty")
            except (RuntimeError, ValueError) as error:
                self.reject(400, str(error))
                return
            with run_lock:
                if run_started:
                    self.reject(409, "M1 gate already ran")
                    return
                run_started = True
            finished = threading.Event()

            def watch_disconnect():
                while not finished.wait(0.1):
                    try:
                        readable, _, _ = select.select([self.connection], [], [], 0)
                        if readable and not self.connection.recv(1, socket.MSG_PEEK):
                            gate_run.cancel()
                            return
                    except OSError:
                        gate_run.cancel()
                        return

            watcher = threading.Thread(target=watch_disconnect, daemon=True)
            watcher.start()
            try:
                result = gate_run.run_once()
                payload = json.dumps(result).encode()
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(payload)))
                self.end_headers()
                self.wfile.write(payload)
            except (KeyboardInterrupt, BrokenPipeError, ConnectionResetError):
                gate_run.cancel()
                self.close_connection = True
            finally:
                finished.set()
                watcher.join()

    return ThreadingHTTPServer(("127.0.0.1", 0), Handler), token


def option_value(argv, name):
    for index, value in enumerate(argv):
        if value == name and index + 1 < len(argv):
            return argv[index + 1]
        if value.startswith(f"{name}="):
            return value.split("=", 1)[1]
    return None


def failure_evidence(source_sha, error):
    return {
        "source_sha": source_sha,
        "functional": "FAIL",
        "release_eligible": False,
        "operator_acceptance_required": True,
        "observed_at": datetime.now(timezone.utc).isoformat(),
        "error": error,
    }


class EvidenceArgumentParser(argparse.ArgumentParser):
    def error(self, message):
        argv = sys.argv[1:]
        repo = Path(option_value(argv, "--repo") or Path(__file__).resolve().parents[1])
        output = option_value(argv, "--output")
        if output is None:
            name = (
                "m1-target-qualification.json"
                if any(value == "--qualify-target" or value.startswith("--qualify-target=") for value in argv)
                else "m1-release-gate.json"
            )
            output = repo / "target" / name
        try:
            write_evidence(
                Path(output).resolve(),
                failure_evidence(
                    option_value(argv, "--expected-sha") or os.environ.get("M1_EXPECTED_SHA"),
                    message,
                ),
            )
        except OSError:
            pass
        super().error(message)


def parse_args():
    parser = EvidenceArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--guided", action="store_true")
    mode.add_argument("--headless", action="store_true")
    mode.add_argument("--qualify-target", type=Path)
    parser.add_argument("--repo", type=Path, default=Path(__file__).resolve().parents[1])
    parser.add_argument("--expected-sha", default=os.environ.get("M1_EXPECTED_SHA"))
    parser.add_argument("--output", type=Path)
    parser.add_argument("--candidate-binary", type=Path)
    parser.add_argument(
        "--deployment-path",
        type=Path,
        default=os.environ.get("M1_DEPLOYMENT_PATH"),
    )
    parser.add_argument("--step-timeout", type=int, default=DEFAULT_STEP_TIMEOUT)
    args = parser.parse_args()
    args.repo = args.repo.resolve()
    if args.output is None:
        name = "m1-target-qualification.json" if args.qualify_target else "m1-release-gate.json"
        args.output = args.repo / "target" / name
    return args


def main():
    args = parse_args()
    if args.expected_sha is None:
        try:
            args.expected_sha = command_output(["git", "rev-parse", "HEAD"], args.repo)
        except Exception as error:
            result = {
                "source_sha": None,
                "functional": "FAIL",
                "release_eligible": False,
                "operator_acceptance_required": True,
                "observed_at": datetime.now(timezone.utc).isoformat(),
                "error": str(error),
            }
            write_evidence(args.output.resolve(), result)
            print(json.dumps(result, indent=2, sort_keys=True))
            return 1
    if args.deployment_path is None or not args.deployment_path.is_absolute():
        message = (
            "M1_DEPLOYMENT_PATH is required"
            if args.deployment_path is None
            else "M1_DEPLOYMENT_PATH must be absolute"
        )
        result = {
            "source_sha": args.expected_sha,
            "functional": "FAIL",
            "release_eligible": False,
            "operator_acceptance_required": True,
            "observed_at": datetime.now(timezone.utc).isoformat(),
            "error": message,
        }
        write_evidence(args.output.resolve(), result)
        print(json.dumps(result, indent=2, sort_keys=True))
        return 1
    args.deployment_path = Path(os.path.normpath(str(args.deployment_path)))
    if args.step_timeout <= 0:
        result = {
            "source_sha": args.expected_sha,
            "functional": "FAIL",
            "release_eligible": False,
            "operator_acceptance_required": True,
            "observed_at": datetime.now(timezone.utc).isoformat(),
            "error": "step timeout must be positive",
        }
        write_evidence(args.output.resolve(), result)
        print(json.dumps(result, indent=2, sort_keys=True))
        return 1
    if args.qualify_target:
        try:
            require(args.candidate_binary is not None, "--candidate-binary is required")
            candidate_evidence(args.repo, args.expected_sha)
            result = qualify_target(
                args.qualify_target,
                args.deployment_path,
                args.expected_sha,
                args.candidate_binary.resolve(),
            )
        except Exception as error:
            result = {
                "source_sha": args.expected_sha,
                "deployment_path": str(args.deployment_path),
                "status": "FAIL",
                "functional": "FAIL",
                "release_eligible": False,
                "observed_at": datetime.now(timezone.utc).isoformat(),
                "error": str(error),
            }
            write_evidence(args.output.resolve(), result)
            print(json.dumps(result, indent=2, sort_keys=True))
            return 1
        write_evidence(args.output.resolve(), result)
        print(json.dumps(result, indent=2, sort_keys=True))
        return 0
    gate_run = GateRun(
        args.repo,
        args.expected_sha,
        args.output.resolve(),
        args.step_timeout,
        args.deployment_path,
    )
    if args.headless:
        result = gate_run.run_once()
        print(json.dumps(result, indent=2, sort_keys=True))
        return 0 if result["functional"] == "PASS" else 1

    server, _ = create_server(gate_run)
    url = f"http://127.0.0.1:{server.server_port}/"
    print(f"M1 gate ready: {url}")
    webbrowser.open(url)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        gate_run.cancel()
    finally:
        server.server_close()
    return 0 if gate_run.result and gate_run.result["functional"] == "PASS" else 1


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (OSError, RuntimeError, subprocess.SubprocessError, ValueError) as error:
        print(f"M1 release gate failed: {error}", file=sys.stderr)
        sys.exit(1)
