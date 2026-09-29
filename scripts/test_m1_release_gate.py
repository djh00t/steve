#!/usr/bin/env python3
"""Behavior checks for the M1 release gate."""

import http.client
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from unittest import mock


SCRIPT = Path(__file__).with_name("m1_release_gate.py")
SPEC = importlib.util.spec_from_file_location("m1_release_gate", SCRIPT)
gate = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(gate)
SDK_SPEC = importlib.util.spec_from_file_location("sdk_smoke", SCRIPT.with_name("sdk_smoke.py"))
sdk = importlib.util.module_from_spec(SDK_SPEC)
SDK_SPEC.loader.exec_module(sdk)


class FakeGate:
    def __init__(self):
        self.calls = 0
        self.started = threading.Event()
        self.release = threading.Event()
        self.result = {"functional": "PASS", "release_eligible": False, "steps": []}

    def run_once(self):
        self.calls += 1
        self.started.set()
        self.release.wait(2)
        return self.result


class ReleaseGateTests(unittest.TestCase):
    @unittest.skipUnless(os.name == "posix", "process groups require POSIX")
    def test_process_group_cleanup_never_uses_unbounded_wait(self):
        child = mock.Mock(pid=123, returncode=None)
        child.poll.return_value = None
        child.wait.side_effect = subprocess.TimeoutExpired(["stuck"], 2)
        with (
            mock.patch.object(gate.os, "killpg"),
            self.assertRaisesRegex(RuntimeError, "reap"),
        ):
            gate.stop_process_group(child)
        self.assertEqual(child.wait.call_count, 2)
        self.assertTrue(all(call.kwargs.get("timeout") == 2 for call in child.wait.call_args_list))

    def test_sdk_cleanup_never_uses_unbounded_wait(self):
        child = mock.Mock(returncode=None)
        child.poll.return_value = None
        child.wait.side_effect = subprocess.TimeoutExpired(["stuck"], 5)
        with self.assertRaisesRegex(RuntimeError, "reap"):
            sdk.stop(child)
        self.assertEqual(child.wait.call_count, 2)
        self.assertTrue(all(call.kwargs.get("timeout") == 5 for call in child.wait.call_args_list))

    def test_command_timeout_never_uses_unbounded_communicate(self):
        child = mock.Mock(returncode=None)
        child.communicate.side_effect = [
            subprocess.TimeoutExpired(["stuck"], 1),
            subprocess.TimeoutExpired(["stuck"], 2),
        ]
        with (
            mock.patch.object(gate.subprocess, "Popen", return_value=child),
            mock.patch.object(gate, "stop_process_group"),
            self.assertRaisesRegex(RuntimeError, "timed out"),
        ):
            gate.captured_command(["stuck"], Path("/"), timeout=1)
        self.assertEqual(
            child.communicate.call_args_list,
            [mock.call(timeout=1), mock.call(timeout=2)],
        )

    def test_step_timeout_never_uses_unbounded_communicate(self):
        child = mock.Mock(returncode=None)
        child.communicate.side_effect = [
            subprocess.TimeoutExpired(["stuck"], 1, output="partial"),
            subprocess.TimeoutExpired(["stuck"], 2, output="final"),
        ]
        with (
            mock.patch.object(gate.subprocess, "Popen", return_value=child),
            mock.patch.object(gate, "stop_process_group"),
        ):
            result = gate.run_step("stuck", ["stuck"], Path("/"), 1, {})
        self.assertEqual(result["status"], "FAIL")
        self.assertTrue(result["timed_out"])
        self.assertEqual(
            child.communicate.call_args_list,
            [mock.call(timeout=1), mock.call(timeout=2)],
        )

    def test_fixed_steps_select_every_required_scenario_against_candidate(self):
        binary = Path("/candidate/steve")
        steps = gate.fixed_steps(binary, Path("/accounting"))
        cargo_steps = {
            name: command
            for name, command in steps
            if len(command) > 1 and command[1] == "test"
        }
        self.assertEqual(
            set(cargo_steps),
            {
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
                "chat_nonstream_accounting",
                "chat_stream_terminal_accounting",
                "chat_accounting_does_not_delay_response",
                "chat_completions_stream_forwards_first_event_before_tail",
                "chat_disconnect_no_replay",
                "inference_saturation_keeps_management_live",
                "responses_disconnect_cancels_upstream",
                "messages_disconnect_cancels_upstream",
                "server::tests::get_v1_models_lists_configured_models",
                "server::tests::get_v1_models_uses_static_catalogue_when_unconfigured",
                "server::tests::provider_health_reports_reachable_closed_and_unconfigured",
                "server::tests::provider_health_reports_fixture_and_unconfigured_as_healthy",
                "server::tests::provider_health_rejects_missing_and_server_error_provider_routes",
                "server::tests::provider_health_times_out_without_blocking_other_management_routes",
                "server::tests::provider_health_returns_busy_while_another_probe_is_running",
            },
        )
        for name, command in cargo_steps.items():
            self.assertIn("--locked", command, name)
            self.assertEqual(command[-2:], ["--exact", "--nocapture"], name)
            self.assertIn(name, command, name)

        env = gate.candidate_environment(binary)
        self.assertEqual(env["STEVE_TEST_BINARY"], str(binary.resolve()))

    def test_missing_postgres_is_unknown_and_blocks_qualification(self):
        evidence = gate.postgres_prerequisite({})
        self.assertEqual(evidence["status"], "UNKNOWN")
        functional, eligible = gate.verdicts(
            [evidence],
            {"status": "PASS", "source_sha": "abc"},
            {"status": "PASS", "source_sha": "abc"},
            "abc",
        )
        self.assertEqual(functional, "FAIL")
        self.assertFalse(eligible)

    def test_verdicts_keep_functional_and_release_evidence_separate(self):
        steps = [{"name": "sdk", "status": "PASS"}]
        functional, eligible = gate.verdicts(
            steps,
            {"status": "PASS", "source_sha": "abc"},
            {"status": "PASS", "source_sha": "abc"},
            "abc",
        )
        self.assertEqual(functional, "PASS")
        self.assertTrue(eligible)

        functional, eligible = gate.verdicts(steps, None, None, "abc")
        self.assertEqual(functional, "PASS")
        self.assertFalse(eligible)

        functional, eligible = gate.verdicts(
            [{"name": "sdk", "status": "FAIL"}], None, None, "abc"
        )
        self.assertEqual(functional, "FAIL")
        self.assertFalse(eligible)

        functional, eligible = gate.verdicts(
            steps,
            {"status": "PASS", "source_sha": "abc"},
            {"status": "PASS", "source_sha": "abc"},
            "abc",
            {"status": "PRESENT", "external_stop_verified": False},
        )
        self.assertEqual(functional, "PASS")
        self.assertFalse(eligible)

    def test_page_uses_text_content_and_does_not_embed_command_output(self):
        hostile = "<img src=x onerror=alert(1)>"
        page = gate.page_html("token", {"output": hostile})
        self.assertIn("textContent", page)
        self.assertNotIn(hostile, page)

    def test_http_gate_rejects_foreign_requests_and_allows_one_exact_post(self):
        fake = FakeGate()
        server, token = gate.create_server(fake)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        host = f"127.0.0.1:{server.server_port}"
        origin = f"http://{host}"

        try:
            connection = http.client.HTTPConnection("127.0.0.1", server.server_port)
            connection.request("GET", "/run", headers={"Host": host})
            self.assertEqual(connection.getresponse().status, 405)

            connection.request(
                "POST",
                "/run",
                body=json.dumps({"command": ["rm", "-rf", "/"]}),
                headers={
                    "Host": "foreign.invalid",
                    "Origin": origin,
                    "X-M1-Capability": token,
                },
            )
            self.assertEqual(connection.getresponse().status, 403)

            connection.request(
                "POST",
                "/run",
                body="{}",
                headers={
                    "Host": host,
                    "Origin": origin,
                    "X-M1-Capability": "wrong",
                },
            )
            self.assertEqual(connection.getresponse().status, 403)

            connection.request(
                "POST",
                "/run",
                body=json.dumps({"command": ["echo", "injected"]}),
                headers={
                    "Host": host,
                    "Origin": origin,
                    "X-M1-Capability": token,
                },
            )
            self.assertEqual(connection.getresponse().status, 400)
            self.assertEqual(fake.calls, 0)

            raw = http.client.HTTPConnection("127.0.0.1", server.server_port)
            raw.putrequest("POST", "/run", skip_host=True)
            raw.putheader("Host", host)
            raw.putheader("Origin", origin)
            raw.putheader("X-M1-Capability", token)
            raw.putheader("Content-Length", "invalid")
            raw.endheaders()
            self.assertEqual(raw.getresponse().status, 400)
            self.assertEqual(fake.calls, 0)

            first = http.client.HTTPConnection("127.0.0.1", server.server_port)
            first.request(
                "POST",
                "/run",
                body="{}",
                headers={
                    "Host": host,
                    "Origin": origin,
                    "X-M1-Capability": token,
                },
            )
            self.assertTrue(fake.started.wait(1))

            second = http.client.HTTPConnection("127.0.0.1", server.server_port)
            second.request(
                "POST",
                "/run",
                body="{}",
                headers={
                    "Host": host,
                    "Origin": origin,
                    "X-M1-Capability": token,
                },
            )
            self.assertEqual(second.getresponse().status, 409)
            fake.release.set()
            self.assertEqual(first.getresponse().status, 200)
            self.assertEqual(fake.calls, 1)
        finally:
            fake.release.set()
            server.shutdown()
            server.server_close()
            thread.join(2)

    @unittest.skipUnless(os.name == "posix", "process-group check requires POSIX")
    def test_guided_interrupt_stops_active_request_process_group(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            pid_file = root / "child.pid"
            output = root / "evidence.json"
            run = gate.GateRun(root, "abc", output, 5, Path("/srv/accounting"))
            code = (
                "import pathlib, subprocess, time; "
                "p=subprocess.Popen(['sleep','60']); "
                f"pathlib.Path({str(pid_file)!r}).write_text(str(p.pid)); "
                "time.sleep(60)"
            )
            run._run = lambda: run._run_step(
                "active", [sys.executable, "-c", code], os.environ.copy()
            )
            server, token = gate.create_server(run)
            server_thread = threading.Thread(target=server.serve_forever, daemon=True)
            server_thread.start()
            host = f"127.0.0.1:{server.server_port}"

            def request():
                connection = http.client.HTTPConnection("127.0.0.1", server.server_port)
                connection.request(
                    "POST",
                    "/run",
                    body="{}",
                    headers={
                        "Host": host,
                        "Origin": f"http://{host}",
                        "X-M1-Capability": token,
                    },
                )
                try:
                    connection.getresponse()
                except http.client.RemoteDisconnected:
                    pass

            request_thread = threading.Thread(target=request)
            request_thread.start()
            deadline = time.monotonic() + 2
            while not pid_file.exists() and time.monotonic() < deadline:
                time.sleep(0.02)
            self.assertTrue(pid_file.exists(), "active gate step did not start")
            run.cancel()
            request_thread.join(2)
            self.assertFalse(request_thread.is_alive())
            child_pid = int(pid_file.read_text(encoding="utf-8"))
            with self.assertRaises(ProcessLookupError):
                os.kill(child_pid, 0)
            server.shutdown()
            server.server_close()
            server_thread.join(2)

    @unittest.skipUnless(os.name == "posix", "process-group check requires POSIX")
    def test_timed_out_step_kills_descendants(self):
        with tempfile.TemporaryDirectory() as temp:
            pid_file = Path(temp) / "child.pid"
            code = (
                "import pathlib, subprocess, time; "
                f"p=subprocess.Popen(['sleep','60']); pathlib.Path({str(pid_file)!r}).write_text(str(p.pid)); "
                "time.sleep(60)"
            )
            result = gate.run_step(
                "timeout", [sys.executable, "-c", code], Path(temp), 0.3, os.environ.copy()
            )
            self.assertEqual(result["status"], "FAIL")
            self.assertTrue(result["timed_out"])
            child_pid = int(pid_file.read_text(encoding="utf-8"))
            deadline = time.monotonic() + 2
            while time.monotonic() < deadline:
                try:
                    os.kill(child_pid, 0)
                except ProcessLookupError:
                    break
                time.sleep(0.02)
            else:
                self.fail(f"descendant {child_pid} survived timeout")

    @unittest.skipUnless(os.name == "posix", "process-group check requires POSIX")
    def test_successful_step_cleans_descendants(self):
        with tempfile.TemporaryDirectory() as temp:
            pid_file = Path(temp) / "child.pid"
            code = (
                "import pathlib, subprocess; "
                "p=subprocess.Popen(['sleep','60'], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL); "
                f"pathlib.Path({str(pid_file)!r}).write_text(str(p.pid))"
            )
            result = gate.run_step(
                "cleanup", [sys.executable, "-c", code], Path(temp), 2, os.environ.copy()
            )
            self.assertEqual(result["status"], "PASS")
            child_pid = int(pid_file.read_text(encoding="utf-8"))
            deadline = time.monotonic() + 2
            while time.monotonic() < deadline:
                try:
                    os.kill(child_pid, 0)
                except ProcessLookupError:
                    break
                time.sleep(0.02)
            else:
                self.fail(f"descendant {child_pid} survived successful step")

    def test_candidate_requires_clean_exact_head_and_tracked_lock(self):
        with tempfile.TemporaryDirectory() as temp:
            repo = Path(temp)
            subprocess.run(["git", "init", "-q", str(repo)], check=True)
            subprocess.run(["git", "config", "user.email", "test@example.invalid"], cwd=repo, check=True)
            subprocess.run(["git", "config", "user.name", "Test"], cwd=repo, check=True)
            (repo / "Cargo.lock").write_text("# lock\n", encoding="utf-8")
            subprocess.run(["git", "add", "Cargo.lock"], cwd=repo, check=True)
            subprocess.run(["git", "commit", "-qm", "fixture"], cwd=repo, check=True)
            head = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=repo, text=True).strip()

            evidence = gate.candidate_evidence(repo, head)
            self.assertEqual(evidence["source_sha"], head)
            self.assertFalse(evidence["source_dirty"])
            self.assertTrue(evidence["lock_tracked"])

            (repo / "dirty").write_text("x", encoding="utf-8")
            with self.assertRaisesRegex(RuntimeError, "clean"):
                gate.candidate_evidence(repo, head)

    def test_json_evidence_is_written_even_for_failure(self):
        with tempfile.TemporaryDirectory() as temp:
            output = Path(temp) / "nested" / "evidence.json"
            record = {"functional": "FAIL", "release_eligible": False}
            gate.write_evidence(output, record)
            self.assertEqual(json.loads(output.read_text(encoding="utf-8")), record)

    @unittest.skipUnless(os.name == "posix", "executable fixture requires POSIX")
    def test_exact_test_filter_fails_when_it_selects_nothing(self):
        with tempfile.TemporaryDirectory() as temp:
            cargo = Path(temp) / "cargo"
            cargo.write_text("#!/bin/sh\nprintf 'running 0 tests\\n'\n", encoding="utf-8")
            cargo.chmod(0o755)
            result = gate.run_step(
                "missing",
                [cargo, "test", "missing_name", "--", "--exact"],
                Path(temp),
                2,
                os.environ.copy(),
            )
            self.assertEqual(result["status"], "FAIL")
            self.assertIn("selected no tests", result["error"])

    @unittest.skipUnless(os.name == "posix", "fake gh executable requires POSIX")
    def test_hosted_evidence_comes_from_fixed_pr_and_exact_head(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            gh = root / "gh"
            gh.write_text(
                "#!/bin/sh\n"
                "case \"$*\" in\n"
                "  *'pr view 619'* ) printf '%s\\n' '{\"headRefOid\":\"abc\",\"url\":\"https://github.example/pr/619\"}' ;;\n"
                "  *'pr checks 619'* ) printf '%s\\n' '[{\"bucket\":\"pass\",\"name\":\"quality\",\"state\":\"SUCCESS\"},{\"bucket\":\"pass\",\"name\":\"mutation\",\"state\":\"SUCCESS\"}]' ;;\n"
                "  * ) exit 2 ;;\n"
                "esac\n",
                encoding="utf-8",
            )
            gh.chmod(0o755)
            env = os.environ.copy()
            env["PATH"] = f"{root}:{env['PATH']}"
            evidence = gate.hosted_qualification(root, "abc", env)
            self.assertEqual(evidence["status"], "PASS")
            self.assertEqual(evidence["source_sha"], "abc")
            self.assertEqual(evidence["pull_request"], 619)
            self.assertEqual(evidence["url"], "https://github.example/pr/619")

    @unittest.skipUnless(os.name == "posix", "fake gh executable requires POSIX")
    def test_hosted_evidence_requires_targeted_mutation_check(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            gh = root / "gh"
            gh.write_text(
                "#!/bin/sh\n"
                "case \"$*\" in\n"
                "  *'pr view 619'* ) printf '%s\\n' '{\"headRefOid\":\"abc\",\"url\":\"https://github.example/pr/619\"}' ;;\n"
                "  *'pr checks 619'* ) printf '%s\\n' '[{\"bucket\":\"pass\",\"name\":\"quality\",\"state\":\"SUCCESS\"}]' ;;\n"
                "  * ) exit 2 ;;\n"
                "esac\n",
                encoding="utf-8",
            )
            gh.chmod(0o755)
            env = os.environ.copy()
            env["PATH"] = f"{root}:{env['PATH']}"
            evidence = gate.hosted_qualification(root, "abc", env)
            self.assertEqual(evidence["status"], "UNKNOWN")
            self.assertEqual(evidence["missing_required_checks"], ["mutation"])

    @unittest.skipUnless(os.name == "posix", "fake gh executable requires POSIX")
    def test_hosted_evidence_rejects_wrong_head_before_check_state(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            gh = root / "gh"
            gh.write_text(
                "#!/bin/sh\n"
                "case \"$*\" in\n"
                "  *'pr view 619'* ) printf '%s\\n' '{\"headRefOid\":\"other\",\"url\":\"https://github.example/pr/619\"}' ;;\n"
                "  * ) exit 1 ;;\n"
                "esac\n",
                encoding="utf-8",
            )
            gh.chmod(0o755)
            env = os.environ.copy()
            env["PATH"] = f"{root}:{env['PATH']}"
            evidence = gate.hosted_qualification(root, "abc", env)
            self.assertEqual(evidence["status"], "FAIL")
            self.assertEqual(evidence["source_sha"], "other")

    def test_hosted_evidence_reports_unavailable_pr_as_unknown(self):
        with tempfile.TemporaryDirectory() as temp:
            env = {"PATH": str(Path(temp) / "missing")}
            evidence = gate.hosted_qualification(Path(temp), "abc", env)
            self.assertEqual(evidence["status"], "UNKNOWN")
            self.assertIsNone(evidence["url"])

    @unittest.skipUnless(os.name == "posix", "fake gh executable requires POSIX")
    def test_hosted_evidence_rejects_non_list_checks_as_unknown(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            gh = root / "gh"
            gh.write_text(
                "#!/bin/sh\n"
                "case \"$*\" in\n"
                "  *'pr view 619'* ) printf '%s\\n' '{\"headRefOid\":\"abc\",\"url\":\"https://github.example/pr/619\"}' ;;\n"
                "  *'pr checks 619'* ) printf '%s\\n' '{\"name\":\"mutation\"}' ;;\n"
                "  * ) exit 2 ;;\n"
                "esac\n",
                encoding="utf-8",
            )
            gh.chmod(0o755)
            env = os.environ.copy()
            env["PATH"] = f"{root}:{env['PATH']}"
            evidence = gate.hosted_qualification(root, "abc", env)
            self.assertEqual(evidence["status"], "UNKNOWN")
            self.assertIn("array", evidence["error"])

    @unittest.skipUnless(os.name == "posix", "target lock probe requires POSIX")
    def test_target_probe_binds_real_filesystem_checks_to_source_sha(self):
        with tempfile.TemporaryDirectory() as temp:
            parent = Path(temp).resolve()
            deployment = parent / "accounting" / "replica-a"
            binary = parent / "steve"
            binary.write_bytes(b"candidate")
            evidence = gate.qualify_target(parent, deployment, "abc", binary)
            self.assertEqual(evidence["status"], "PASS")
            self.assertEqual(evidence["source_sha"], "abc")
            self.assertEqual(evidence["target_parent"], str(parent))
            self.assertEqual(evidence["deployment_path"], str(deployment))
            self.assertNotEqual(evidence["probe_path"], str(deployment))
            self.assertEqual(evidence["target_device"], evidence["probe_device"])
            self.assertEqual(evidence["binary_sha256"], gate.sha256(binary))
            self.assertRegex(evidence["observed_at"], r"^\d{4}-\d\d-\d\dT")
            self.assertIn("python", evidence["tool_versions"])
            self.assertNotIn(evidence["filesystem"], {"", "/"})
            self.assertEqual(
                set(evidence["checks"]),
                {
                    "exclusive_lock",
                    "atomic_publication",
                    "append_visibility",
                    "file_sync",
                    "directory_sync",
                    "revisioned_publication",
                    "process_death_lock_release",
                },
            )
            qualification = parent / "qualification.json"
            qualification.write_text(json.dumps(evidence), encoding="utf-8")
            self.assertEqual(
                gate.load_target_qualification(
                    qualification, "abc", gate.sha256(binary), deployment
                )["deployment_path"],
                str(deployment),
            )

            del evidence["tool_versions"]
            qualification.write_text(json.dumps(evidence), encoding="utf-8")
            with self.assertRaisesRegex(RuntimeError, "tool versions"):
                gate.load_target_qualification(
                    qualification, "abc", gate.sha256(binary), deployment
                )

    @unittest.skipUnless(os.name == "posix", "target lock probe requires POSIX")
    def test_target_probe_rejects_paths_outside_parent_without_touching_them(self):
        with tempfile.TemporaryDirectory() as temp:
            parent = Path(temp).resolve()
            binary = parent / "steve"
            binary.write_bytes(b"candidate")
            outside = parent.parent / "outside-accounting-root"
            self.assertFalse(outside.exists())
            with self.assertRaisesRegex(RuntimeError, "beneath target parent"):
                gate.qualify_target(parent, outside, "abc", binary)
            self.assertFalse(outside.exists())

    @unittest.skipUnless(os.name == "posix", "target lock probe requires POSIX")
    def test_target_probe_canonicalizes_parent_and_rebases_intended_child(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            real_parent = root / "real"
            real_parent.mkdir()
            alias = root / "alias"
            alias.symlink_to(real_parent, target_is_directory=True)
            binary = real_parent / "steve"
            binary.write_bytes(b"candidate")
            evidence = gate.qualify_target(
                alias, alias / "accounting" / "replica-a", "abc", binary
            )
            self.assertEqual(evidence["target_parent"], str(real_parent.resolve()))
            self.assertEqual(evidence["requested_target_parent"], str(alias))
            self.assertEqual(
                evidence["deployment_path"],
                str(alias / "accounting" / "replica-a"),
            )

    @unittest.skipUnless(os.name == "posix", "target lock probe requires POSIX")
    def test_target_probe_rejects_symlink_escape_on_intended_path(self):
        with tempfile.TemporaryDirectory() as temp:
            parent = Path(temp).resolve()
            binary = parent / "steve"
            binary.write_bytes(b"candidate")
            (parent / "escape").symlink_to("/dev", target_is_directory=True)
            with self.assertRaisesRegex(RuntimeError, "symlink"):
                gate.qualify_target(parent, parent / "escape", "abc", binary)

    def test_target_evidence_rejects_a_status_only_file(self):
        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp) / "qualification.json"
            path.write_text('{"status":"PASS","source_sha":"abc"}', encoding="utf-8")
            with self.assertRaisesRegex(RuntimeError, "probe"):
                gate.load_target_qualification(
                    path, "abc", "binary-hash", Path("/srv/steve/accounting")
                )

    def test_target_evidence_rejects_wrong_deployment_identity(self):
        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp) / "qualification.json"
            path.write_text(
                json.dumps(
                    {
                        "probe": gate.TARGET_PROBE,
                        "status": "PASS",
                        "source_sha": "abc",
                        "binary_sha256": "binary-hash",
                        "deployment_path": "/srv/other",
                        "target_parent": "/srv",
                        "target_device": 1,
                        "probe_device": 1,
                        "probe_path": "/srv/probe",
                        "checks": {name: "PASS" for name in gate.TARGET_CHECKS},
                    }
                ),
                encoding="utf-8",
            )
            with self.assertRaisesRegex(RuntimeError, "deployment path"):
                gate.load_target_qualification(
                    path, "abc", "binary-hash", Path("/srv/steve/accounting")
                )

    def test_adoption_manifest_surfaces_unverified_operator_assertion(self):
        with tempfile.TemporaryDirectory() as temp:
            assertion = Path(temp) / "maintenance.json"
            manifest = Path(temp) / "adoption.json"
            fields = {
                "source_path": "/legacy/accounting.jsonl",
                "source_host": "legacy-host",
                "assertion_time": "2026-09-29T00:00:00Z",
                "supervisor_context": "systemd steve.service",
                "claimed_stop_disable_action": "stopped and disabled",
            }
            assertion.write_text(json.dumps(fields) + "\n", encoding="utf-8")
            manifest.write_text(
                json.dumps(
                    {
                        "maintenance_assertion": fields,
                        "maintenance_assertion_sha256": gate.sha256(assertion),
                        "maintenance_assertion_path": str(assertion),
                    }
                ),
                encoding="utf-8",
            )
            evidence = gate.load_adoption_manifest(manifest)
            self.assertEqual(evidence["fields"], fields)
            self.assertEqual(evidence["digest"], gate.sha256(assertion))
            self.assertFalse(evidence["external_stop_verified"])
            self.assertEqual(evidence["source"], "operator_supplied")

            tampered = dict(fields, source_host="other-host")
            manifest.write_text(
                json.dumps(
                    {
                        "maintenance_assertion": tampered,
                        "maintenance_assertion_sha256": gate.sha256(assertion),
                        "maintenance_assertion_path": str(assertion),
                    }
                ),
                encoding="utf-8",
            )
            with self.assertRaisesRegex(RuntimeError, "fields do not match"):
                gate.load_adoption_manifest(manifest)

            fields.pop("supervisor_context")
            manifest.write_text(
                json.dumps(
                    {
                        "maintenance_assertion": fields,
                        "maintenance_assertion_sha256": gate.sha256(assertion),
                        "maintenance_assertion_path": str(assertion),
                    }
                ),
                encoding="utf-8",
            )
            with self.assertRaisesRegex(RuntimeError, "supervisor_context"):
                gate.load_adoption_manifest(manifest)

            fields["supervisor_context"] = "systemd steve.service"
            fields["assertion_time"] = "yesterday"
            assertion.write_text(json.dumps(fields) + "\n", encoding="utf-8")
            manifest.write_text(
                json.dumps(
                    {
                        "maintenance_assertion": fields,
                        "maintenance_assertion_sha256": gate.sha256(assertion),
                        "maintenance_assertion_path": str(assertion),
                    }
                ),
                encoding="utf-8",
            )
            with self.assertRaisesRegex(RuntimeError, "assertion_time"):
                gate.load_adoption_manifest(manifest)

    def test_gate_preflight_failure_always_writes_fail_json(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            output = root / "evidence.json"
            result = gate.GateRun(root, "missing", output, 1, Path("/srv/accounting")).run_once()
            self.assertEqual(result["functional"], "FAIL")
            self.assertFalse(result["release_eligible"])
            self.assertTrue(result["operator_acceptance_required"])
            self.assertEqual(json.loads(output.read_text(encoding="utf-8")), result)

    def test_gate_interrupt_replaces_stale_pass_with_fail_json(self):
        with tempfile.TemporaryDirectory() as temp:
            output = Path(temp) / "evidence.json"
            output.write_text(
                json.dumps(
                    {
                        "source_sha": "abc",
                        "functional": "PASS",
                        "release_eligible": True,
                    }
                ),
                encoding="utf-8",
            )
            run = gate.GateRun(Path(temp), "abc", output, 1, Path("/srv/accounting"))

            def interrupt():
                raise KeyboardInterrupt

            run._run = interrupt
            with self.assertRaises(KeyboardInterrupt):
                run.run_once()
            evidence = json.loads(output.read_text(encoding="utf-8"))
            self.assertEqual(evidence["functional"], "FAIL")
            self.assertFalse(evidence["release_eligible"])
            self.assertIn("interrupted", evidence["error"])

    def test_cli_argument_preflight_failure_writes_fail_json(self):
        with tempfile.TemporaryDirectory() as temp:
            output = Path(temp) / "evidence.json"
            env = os.environ.copy()
            env.pop("M1_DEPLOYMENT_PATH", None)
            result = subprocess.run(
                [
                    sys.executable,
                    "-B",
                    str(SCRIPT),
                    "--headless",
                    "--expected-sha",
                    "abc",
                    "--output",
                    str(output),
                ],
                cwd=SCRIPT.parents[1],
                env=env,
                text=True,
                stdout=subprocess.PIPE,
                stderr=subprocess.STDOUT,
                check=False,
            )
            self.assertNotEqual(result.returncode, 0)
            evidence = json.loads(output.read_text(encoding="utf-8"))
            self.assertEqual(evidence["functional"], "FAIL")
            self.assertIn("M1_DEPLOYMENT_PATH", evidence["error"])

    def test_cli_git_preflight_failure_writes_fail_json(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            output = root / "evidence.json"
            env = os.environ.copy()
            env["M1_DEPLOYMENT_PATH"] = "/srv/steve/accounting"
            result = subprocess.run(
                [
                    sys.executable,
                    "-B",
                    str(SCRIPT),
                    "--headless",
                    "--repo",
                    str(root),
                    "--output",
                    str(output),
                ],
                cwd=SCRIPT.parents[1],
                env=env,
                text=True,
                stdout=subprocess.PIPE,
                stderr=subprocess.STDOUT,
                check=False,
            )
            self.assertNotEqual(result.returncode, 0)
            evidence = json.loads(output.read_text(encoding="utf-8"))
            self.assertEqual(evidence["functional"], "FAIL")
            self.assertIn("git", evidence["error"])

    def test_cli_timeout_preflight_failure_writes_fail_json(self):
        with tempfile.TemporaryDirectory() as temp:
            output = Path(temp) / "evidence.json"
            env = os.environ.copy()
            env["M1_DEPLOYMENT_PATH"] = "/srv/steve/accounting"
            result = subprocess.run(
                [
                    sys.executable,
                    "-B",
                    str(SCRIPT),
                    "--headless",
                    "--expected-sha",
                    "abc",
                    "--step-timeout",
                    "0",
                    "--output",
                    str(output),
                ],
                cwd=SCRIPT.parents[1],
                env=env,
                text=True,
                stdout=subprocess.PIPE,
                stderr=subprocess.STDOUT,
                check=False,
            )
            self.assertNotEqual(result.returncode, 0)
            evidence = json.loads(output.read_text(encoding="utf-8"))
            self.assertEqual(evidence["functional"], "FAIL")
            self.assertIn("timeout", evidence["error"])

    def test_cli_argument_error_writes_fail_json(self):
        with tempfile.TemporaryDirectory() as temp:
            output = Path(temp) / "evidence.json"
            result = subprocess.run(
                [
                    sys.executable,
                    "-B",
                    str(SCRIPT),
                    "--headless",
                    "--step-timeout",
                    "bad",
                    "--output",
                    str(output),
                ],
                cwd=SCRIPT.parents[1],
                text=True,
                stdout=subprocess.PIPE,
                stderr=subprocess.STDOUT,
                check=False,
            )
            self.assertEqual(result.returncode, 2)
            evidence = json.loads(output.read_text(encoding="utf-8"))
            self.assertEqual(evidence["functional"], "FAIL")
            self.assertFalse(evidence["release_eligible"])
            self.assertIn("invalid int value", evidence["error"])

    def test_sdk_require_survives_python_optimization(self):
        result = subprocess.run(
            [
                sys.executable,
                "-B",
                "-O",
                "-c",
                "import scripts.sdk_smoke as s; s.require(False, 'kept')",
            ],
            cwd=SCRIPT.parents[1],
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            check=False,
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("RuntimeError: kept", result.stdout)


if __name__ == "__main__":
    unittest.main()
