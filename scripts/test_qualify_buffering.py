#!/usr/bin/env python3
"""Check SIGTERM cleanup for the buffering qualification CLI."""

from __future__ import annotations

import json
import os
import signal
import subprocess
import tempfile
import time
import unittest
from pathlib import Path


class TerminationCleanupTest(unittest.TestCase):
    """Exercise the real CLI while its baseline command has a child process."""

    def test_sigterm_stops_process_group_and_records_error(self) -> None:
        """Verify termination leaves evidence but no child or temporary copy."""
        repository = Path(__file__).resolve().parents[1]
        runner = repository / "scripts/qualify_buffering.py"
        with tempfile.TemporaryDirectory(prefix="steve-buffering-sigterm-") as temp:
            root = Path(temp)
            fixture = root / "repo"
            (fixture / "src/proxy").mkdir(parents=True)
            (fixture / "tests/mutations").mkdir(parents=True)
            stream = repository / "src/proxy/stream.rs"
            patch = repository / "tests/mutations/buffer-until-eof.patch"
            (fixture / "src/proxy/stream.rs").write_bytes(stream.read_bytes())
            (fixture / "tests/mutations/buffer-until-eof.patch").write_bytes(
                patch.read_bytes()
            )
            subprocess.run(["git", "init", "-q", str(fixture)], check=True)
            subprocess.run(
                ["git", "config", "user.name", "Qualification Check"],
                cwd=fixture,
                check=True,
            )
            subprocess.run(
                ["git", "config", "user.email", "qualification@example.invalid"],
                cwd=fixture,
                check=True,
            )
            subprocess.run(["git", "add", "."], cwd=fixture, check=True)
            subprocess.run(
                ["git", "commit", "-qm", "fixture"], cwd=fixture, check=True
            )

            bin_dir = root / "bin"
            bin_dir.mkdir()
            marker = root / "pids.txt"
            cargo = bin_dir / "cargo"
            cargo.write_text(
                "#!/usr/bin/env python3\n"
                "import os, subprocess, time\n"
                "child = subprocess.Popen(['sleep', '60'])\n"
                "with open(os.environ['CARGO_FIXTURE_PIDS'], 'w') as file:\n"
                "    file.write(f'{os.getpid()} {child.pid}\\n')\n"
                "time.sleep(60)\n",
                encoding="utf-8",
            )
            cargo.chmod(0o755)
            evidence = root / "evidence"
            env = os.environ.copy()
            env["PATH"] = f"{bin_dir}{os.pathsep}{env['PATH']}"
            env["CARGO_FIXTURE_PIDS"] = str(marker)
            process = subprocess.Popen(
                ["python3", "-B", str(runner), "--output", str(evidence)],
                cwd=fixture,
                env=env,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
                start_new_session=True,
            )
            pids: list[int] = []
            survivors: list[int] = []
            try:
                deadline = time.monotonic() + 10
                while not marker.exists() and process.poll() is None:
                    if time.monotonic() >= deadline:
                        self.fail("fake cargo did not start")
                    time.sleep(0.05)
                if marker.exists():
                    pids = [int(pid) for pid in marker.read_text().split()]
                    os.kill(process.pid, signal.SIGTERM)
                stdout, stderr = process.communicate(timeout=10)
                survivors = [pid for pid in pids if _process_is_running(pid)]
            finally:
                if process.poll() is None:
                    process.kill()
                    process.communicate(timeout=5)
                if pids:
                    try:
                        os.killpg(pids[0], signal.SIGKILL)
                    except ProcessLookupError:
                        pass

            self.assertEqual(process.returncode, 1, (stdout, stderr))
            result = json.loads((evidence / "result.json").read_text())
            self.assertEqual(result["status"], "error")
            self.assertTrue((evidence / "baseline.log").exists())
            self.assertFalse(
                any(
                    path.name.startswith("steve-buffering-")
                    for path in evidence.iterdir()
                )
            )
            self.assertEqual(len(pids), 2)
            self.assertEqual(survivors, [], f"processes survived: {survivors}")
            self.assertIn("SIGTERM", result["error"], result)


def _process_is_running(pid: int) -> bool:
    """Return whether a PID exists and is not an exited zombie."""
    try:
        result = subprocess.run(
            ["ps", "-o", "stat=", "-p", str(pid)],
            check=False,
            capture_output=True,
            text=True,
            timeout=5,
        )
    except subprocess.SubprocessError:
        return True
    if result.returncode == 1 and result.stderr:
        raise AssertionError(f"ps could not inspect process {pid}: {result.stderr}")
    if result.returncode not in {0, 1}:
        raise AssertionError(f"ps failed for process {pid}: {result.stderr}")
    output = result.stdout.strip()
    return bool(output) and not output.startswith("Z")


if __name__ == "__main__":
    unittest.main()
