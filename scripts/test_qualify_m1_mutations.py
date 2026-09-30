#!/usr/bin/env python3
"""Check M1 mutation verdicts and bounded-runner failure handling."""

import copy
import json
import os
from pathlib import Path
import tempfile
import sys
import unittest
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent))
import qualify_m1_mutations as qualification


class M1MutationTests(unittest.TestCase):
    def test_exact_head_requires_every_named_caught_fault(self):
        result = {
            "source_head": "candidate",
            "source_dirty": False,
            "status": "caught",
            "faults": {},
        }
        for name, (_, _, test, assertion) in qualification.FAULTS.items():
            result["faults"][name] = {
                "test": test,
                "assertion": assertion,
                "status": "caught",
                "baseline_exit_code": 0,
                "baseline_selected_tests": 1,
                "fault_build_exit_code": 0,
                "fault_exit_code": 101,
                "fault_selected_tests": 1,
                "expected_assertion_seen": True,
            }
        qualification.validate_result(result, "candidate")
        invalid = [
            dict(result, source_head="other"),
            dict(result, source_dirty=True),
            dict(result, faults={}),
            dict(result, status="not_run"),
        ]
        for name in qualification.FAULTS:
            for field, values in {
                "status": ["survived", "equivalent", "unviable", "timeout", "error"],
                "test": ["other"],
                "assertion": ["other"],
                "baseline_exit_code": [101],
                "baseline_selected_tests": [0, 2],
                "fault_build_exit_code": [101],
                "fault_exit_code": [0],
                "fault_selected_tests": [0, 2],
                "expected_assertion_seen": [False],
            }.items():
                for value in values:
                    broken = copy.deepcopy(result)
                    broken["faults"][name][field] = value
                    invalid.append(broken)
            broken = copy.deepcopy(result)
            del broken["faults"][name]
            invalid.append(broken)
        for broken in invalid:
            with self.subTest(broken=broken), self.assertRaises(RuntimeError):
                qualification.validate_result(broken, "candidate")

    @unittest.skipUnless(os.name == "posix", "runner requires Unix")
    def test_runner_retains_observed_outcomes_and_removes_temporary_copy(self):
        with tempfile.TemporaryDirectory() as temp:
            output = Path(temp)

            def run(command, source, env, log):
                self.assertNotIn("STEVE_TEST_BINARY", env)
                if log.name in {"baseline.log", "fault.log"}:
                    test = command[7]
                    assertion = next(
                        values[3]
                        for values in qualification.FAULTS.values()
                        if values[2] == test
                    )
                    failed = log.name == "fault.log"
                    status = "FAILED" if failed else "ok"
                    log.write_text(
                        f"running 1 test\ntest {test} ... {status}\ntest result: {status}\n{assertion if failed else ''}"
                    )
                    return 101 if failed else 0
                return 0

            with (
                mock.patch.object(
                    qualification.runner,
                    "_source_identity",
                    return_value=("candidate", False),
                ),
                mock.patch.object(qualification.runner, "_copy_source"),
                mock.patch.object(qualification.runner, "_check_patch", return_value=0),
                mock.patch.object(qualification.runner, "_run", side_effect=run),
                mock.patch.dict(os.environ, {"STEVE_TEST_BINARY": "/wrong/prebuilt"}),
            ):
                self.assertEqual(qualification.qualify(output, "candidate"), 0)
            qualification.validate_result(
                json.loads((output / "result.json").read_text()), "candidate"
            )
            self.assertFalse(list(output.glob("steve-m1-mutations-*")))
            with (
                mock.patch.object(
                    qualification.runner,
                    "_source_identity",
                    return_value=("candidate", False),
                ),
                mock.patch.object(qualification.runner, "_copy_source"),
                mock.patch.object(qualification.runner, "_check_patch", return_value=0),
                mock.patch.object(
                    qualification.runner,
                    "_run",
                    side_effect=qualification.runner.QualificationError(
                        "command timed out"
                    ),
                ),
            ):
                self.assertEqual(qualification.qualify(output, "candidate"), 1)
            self.assertEqual(
                json.loads((output / "result.json").read_text())["status"], "timeout"
            )
            self.assertFalse(list(output.glob("steve-m1-mutations-*")))


if __name__ == "__main__":
    unittest.main()
