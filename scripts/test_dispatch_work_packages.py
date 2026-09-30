#!/usr/bin/env python3
"""Focused checks for dispatch inventory validation."""

from __future__ import annotations

import unittest

from dispatch_work_packages import derive_inventory, paths_conflict


SHA = "a" * 40


def package(package_id: str, issue: int, **changes: object) -> dict[str, object]:
    """Build one complete synthetic package."""
    row: dict[str, object] = {
        "id": package_id,
        "issue": issue,
        "issue_state": "OPEN",
        "readiness": "READY",
        "delivery_state": "BLOCKED",
        "kind": "leaf",
        "predecessor_ids": [],
        "required_inputs": [],
        "owned_paths": [f"src/{package_id.lower()}.rs"],
        "active_minutes_estimate": {"min": 5, "max": 10},
        "acceptance_command": f"cargo test {package_id.lower()}",
        "acceptance_command_kind": "introduce_test",
        "acceptance_command_verified": False,
        "single_writer": True,
    }
    row.update(changes)
    return row


def derive(*packages: dict[str, object]) -> dict[str, object]:
    """Derive a synthetic inventory."""
    return derive_inventory({"schema_version": 1, "packages": list(packages)})


class DispatchInventoryTests(unittest.TestCase):
    """Protect readiness, graph, and ownership rules."""

    def test_complete_leaf_is_dispatchable(self) -> None:
        result = derive(package("A", 1))
        self.assertEqual(result["dispatchable_frontier"], ["A"])

    def test_closed_ready_issue_is_not_dispatchable(self) -> None:
        result = derive(package("A", 1, issue_state="CLOSED"))
        self.assertEqual(result["dispatchable_frontier"], [])
        self.assertIn("issue_closed", result["packages"][0]["reasons"])

    def test_reconciled_readiness_can_supersede_stale_source_text(self) -> None:
        result = derive(
            package("A", 1, readiness="BLOCKED", reconciled_readiness="READY")
        )
        self.assertEqual(result["dispatchable_frontier"], ["A"])

    def test_unknown_metadata_is_explicitly_ineligible(self) -> None:
        result = derive(package("A", 1, owned_paths=None, acceptance_command=None))
        self.assertEqual(result["dispatchable_frontier"], [])
        self.assertEqual(
            result["packages"][0]["reasons"],
            ["acceptance_command_unknown", "owned_paths_unknown"],
        )

    def test_malformed_non_null_metadata_is_ineligible(self) -> None:
        malformed = [
            ("predecessor_ids", "A"),
            ("predecessor_ids", [{}]),
            ("required_inputs", [None]),
            ("owned_paths", "src/a.rs"),
            ("active_minutes_estimate", 5),
        ]
        for field, value in malformed:
            with self.subTest(field=field, value=value):
                result = derive(package("A", 1, **{field: value}))
                self.assertEqual(result["dispatchable_frontier"], [])
                self.assertIn(
                    f"invalid_{field}", result["packages"][0]["reasons"]
                )

    def test_dependency_needs_exact_evidence_on_the_source_base(self) -> None:
        predecessor = package(
            "A", 1, readiness="BLOCKED", delivery_state="DELIVERED"
        )
        dependent = package(
            "B",
            2,
            predecessor_ids=["A"],
            required_inputs=[
                {
                    "predecessor_id": "A",
                    "accepted_revision": SHA,
                    "evidence": "https://example.test/review/1",
                    "baseline_state": "present",
                    "required_source_policy": "source_base",
                }
            ],
        )
        result = derive(predecessor, dependent)
        self.assertEqual(result["dispatchable_frontier"], ["B"])

    def test_cycles_and_missing_predecessors_are_reported(self) -> None:
        result = derive(
            package("A", 1, predecessor_ids=["B"], required_inputs=[]),
            package("B", 2, predecessor_ids=["A"], required_inputs=[]),
            package("C", 3, predecessor_ids=["MISSING"], required_inputs=[]),
        )
        self.assertEqual(result["summary"]["cycles"], 1)
        self.assertEqual(result["summary"]["missing_predecessors"], 1)

    def test_source_base_policy_rejects_parent_branch_only_input(self) -> None:
        predecessor = package(
            "A", 1, readiness="BLOCKED", delivery_state="DELIVERED"
        )
        dependent = package(
            "B",
            2,
            predecessor_ids=["A"],
            required_inputs=[
                {
                    "predecessor_id": "A",
                    "accepted_revision": SHA,
                    "evidence": "https://example.test/review/2",
                    "baseline_state": "parent_branch_only",
                    "required_source_policy": "source_base",
                }
            ],
        )
        result = derive(predecessor, dependent)
        row = next(row for row in result["packages"] if row["id"] == "B")
        self.assertIn("input_not_on_source:A", row["reasons"])

    def test_consumed_acceptance_command_must_be_verified(self) -> None:
        result = derive(
            package(
                "A",
                1,
                acceptance_command_kind="consume_test",
                acceptance_command_verified=False,
            )
        )
        self.assertIn("acceptance_command_unverified", result["packages"][0]["reasons"])

    def test_prefix_and_glob_paths_conflict_conservatively(self) -> None:
        self.assertTrue(paths_conflict("src/proxy", "src/proxy/openai.rs"))
        self.assertTrue(paths_conflict("src/*.rs", "src/server.rs::router"))
        self.assertFalse(paths_conflict("docs/*.md", "src/server.rs"))
        result = derive(
            package("A", 1, owned_paths=["src/proxy"]),
            package("B", 2, owned_paths=["src/proxy/openai.rs"]),
        )
        self.assertEqual(result["dispatchable_frontier"], ["A"])
        self.assertEqual(result["summary"]["path_conflicts"], 1)


if __name__ == "__main__":
    unittest.main()
