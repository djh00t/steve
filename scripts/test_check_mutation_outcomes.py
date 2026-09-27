"""Small contract checks for the mutation outcome verifier."""

from __future__ import annotations

import copy
import json
import tempfile
import unittest
from pathlib import Path

from check_mutation_outcomes import QualificationError, validate_list, validate_outcomes


class MutationOutcomeTests(unittest.TestCase):
    def test_rejects_empty_selection(self) -> None:
        """An empty --list result cannot qualify the target."""
        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp) / "list.json"
            path.write_text("[]", encoding="utf-8")
            with self.assertRaisesRegex(QualificationError, "exactly one"):
                validate_list("cancel", path)

    def test_accepts_only_the_expected_assertion_and_outcome(self) -> None:
        """Only the intended cancellation fault and assertion count as caught."""
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp) / "mutants.out"
            logs = root / "log"
            logs.mkdir(parents=True)
            baseline = (
                "*** cargo test --test e2e_messages --test e2e_responses\n"
                "running 1 test\ntest messages_disconnect_cancels_upstream ... ok\n"
                "running 1 test\ntest responses_disconnect_cancels_upstream ... ok\n"
            )
            mutant_log = (
                "*** cargo test --test e2e_messages\nrunning 1 test\n"
                "test messages_disconnect_cancels_upstream ... FAILED\n"
                "upstream body was not dropped after client disconnect\n"
            )
            (logs / "baseline.log").write_text(baseline, encoding="utf-8")
            (logs / "mutant.log").write_text(mutant_log, encoding="utf-8")
            outcome = {
                "cargo_mutants_version": "27.1.0",
                "total_mutants": 1,
                "caught": 1,
                "missed": 0,
                "timeout": 0,
                "unviable": 0,
                "success": 0,
                "outcomes": [
                    {
                        "scenario": "Baseline",
                        "summary": "Success",
                        "log_path": "log/baseline.log",
                        "phase_results": [
                            {"phase": "Build", "process_status": "Success"},
                            {"phase": "Test", "process_status": "Success"},
                        ],
                    },
                    {
                        "scenario": {
                            "Mutant": {
                                "file": "src/proxy/stream.rs",
                                "function": {
                                    "function_name": "<impl Drop for DisconnectStream>::drop"
                                },
                                "name": "src/proxy/stream.rs:227:9: replace <impl Drop for DisconnectStream>::drop with ()",
                            }
                        },
                        "summary": "CaughtMutant",
                        "log_path": "log/mutant.log",
                        "phase_results": [
                            {"phase": "Build", "process_status": "Success"},
                            {"phase": "Test", "process_status": {"Failure": 101}},
                        ],
                    },
                ],
            }
            evidence = root / "outcomes.json"
            evidence.write_text(json.dumps(outcome), encoding="utf-8")
            validate_outcomes("cancel", root, "head-sha")

            for mutate in (
                lambda value: value.update(total_mutants=0),
                lambda value: value["outcomes"][1]["scenario"]["Mutant"][
                    "function"
                ].update(function_name="ReplayGate::ensure_can_attempt"),
            ):
                changed = copy.deepcopy(outcome)
                mutate(changed)
                evidence.write_text(json.dumps(changed), encoding="utf-8")
                with self.assertRaises(QualificationError):
                    validate_outcomes("cancel", root, "head-sha")

            (logs / "mutant.log").write_text(
                "running 1 test\ntest messages_disconnect_cancels_upstream ... FAILED\n",
                encoding="utf-8",
            )
            evidence.write_text(json.dumps(outcome), encoding="utf-8")
            with self.assertRaisesRegex(QualificationError, "expected assertion"):
                validate_outcomes("cancel", root, "head-sha")


if __name__ == "__main__":
    unittest.main()
