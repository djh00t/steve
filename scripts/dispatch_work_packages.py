#!/usr/bin/env python3
"""Validate the canonical work-package inventory and derive a safe frontier."""

from __future__ import annotations

import argparse
import fnmatch
import json
import posixpath
import re
from collections import Counter
from pathlib import Path
from typing import Any


SHA_RE = re.compile(r"^[0-9a-f]{40}$")
GLOB_RE = re.compile(r"[*?[]")
UNKNOWN_REASONS = {
    "predecessor_ids": "predecessors_unknown",
    "required_inputs": "required_inputs_unknown",
    "owned_paths": "owned_paths_unknown",
    "active_minutes_estimate": "active_minutes_unknown",
    "acceptance_command": "acceptance_command_unknown",
    "acceptance_command_kind": "acceptance_command_kind_unknown",
    "single_writer": "single_writer_unknown",
}


def _path(value: str) -> str:
    """Return a normalized file ownership path without a Rust symbol suffix."""
    return posixpath.normpath(value.strip().removeprefix("./").split("::", 1)[0])


def paths_conflict(left: str, right: str) -> bool:
    """Conservatively detect exact, prefix, and glob ownership overlap."""
    left_path, right_path = _path(left), _path(right)
    if left_path == right_path:
        return True
    if not GLOB_RE.search(left_path + right_path):
        return left_path.startswith(right_path + "/") or right_path.startswith(
            left_path + "/"
        )
    if fnmatch.fnmatchcase(left_path, right_path) or fnmatch.fnmatchcase(
        right_path, left_path
    ):
        return True
    left_root = GLOB_RE.split(left_path, 1)[0].rstrip("/")
    right_root = GLOB_RE.split(right_path, 1)[0].rstrip("/")
    if not left_root or not right_root:
        return True
    return (
        left_root == right_root
        or left_root.startswith(right_root + "/")
        or right_root.startswith(left_root + "/")
    )


def _cycles(packages: dict[str, dict[str, Any]]) -> list[list[str]]:
    """Return dependency cycles without treating unknown predecessors as edges."""
    state: dict[str, int] = {}
    stack: list[str] = []
    found: set[tuple[str, ...]] = set()

    def visit(package_id: str) -> None:
        state[package_id] = 1
        stack.append(package_id)
        predecessors = packages[package_id].get("predecessor_ids") or []
        if not isinstance(predecessors, list):
            predecessors = []
        for predecessor in predecessors:
            if not isinstance(predecessor, str):
                continue
            if predecessor not in packages:
                continue
            if state.get(predecessor, 0) == 0:
                visit(predecessor)
            elif state.get(predecessor) == 1:
                cycle = stack[stack.index(predecessor) :]
                start = min(range(len(cycle)), key=lambda index: cycle[index])
                found.add(tuple(cycle[start:] + cycle[:start]))
        stack.pop()
        state[package_id] = 2

    for package_id in packages:
        if state.get(package_id, 0) == 0:
            visit(package_id)
    return [list(cycle) for cycle in sorted(found)]


def _metadata_reasons(
    package: dict[str, Any], packages: dict[str, dict[str, Any]], cycle_ids: set[str]
) -> list[str]:
    """Explain why one package cannot be dispatched."""
    reasons: list[str] = []
    if package.get("issue_state") != "OPEN":
        reasons.append("issue_closed")
    readiness = package.get("reconciled_readiness", package.get("readiness"))
    if readiness != "READY":
        reasons.append(f"readiness_{str(readiness or 'unknown').lower()}")
    if package.get("delivery_state") != "BLOCKED":
        reasons.append(
            f"delivery_state_{str(package.get('delivery_state', 'unknown')).lower()}"
        )
    if package.get("kind") != "leaf":
        reasons.append("coordinator")
    for field, reason in UNKNOWN_REASONS.items():
        if package.get(field) is None:
            reasons.append(reason)

    predecessors = package.get("predecessor_ids")
    required_inputs = package.get("required_inputs")
    predecessors_valid = isinstance(predecessors, list) and all(
        isinstance(item, str) and bool(item.strip()) for item in predecessors
    )
    required_inputs_valid = isinstance(required_inputs, list) and all(
        isinstance(item, dict)
        and isinstance(item.get("predecessor_id"), str)
        and bool(item["predecessor_id"].strip())
        for item in required_inputs
    )
    if predecessors is not None and not predecessors_valid:
        reasons.append("invalid_predecessor_ids")
    if required_inputs is not None and not required_inputs_valid:
        reasons.append("invalid_required_inputs")
    if predecessors_valid:
        missing = sorted(set(predecessors) - packages.keys())
        reasons.extend(f"missing_predecessor:{item}" for item in missing)
        if required_inputs_valid:
            evidence = {
                item.get("predecessor_id"): item
                for item in required_inputs
            }
            for predecessor in predecessors:
                item = evidence.get(predecessor)
                if not item:
                    reasons.append(f"missing_input_evidence:{predecessor}")
                    continue
                revision_valid = bool(
                    SHA_RE.fullmatch(str(item.get("accepted_revision", "")))
                )
                evidence_valid = bool(str(item.get("evidence", "")).strip())
                if not revision_valid:
                    reasons.append(f"invalid_input_revision:{predecessor}")
                if not evidence_valid:
                    reasons.append(f"missing_input_evidence_ref:{predecessor}")
                source_policy = item.get("required_source_policy")
                baseline_state = item.get("baseline_state")
                if (
                    revision_valid
                    and evidence_valid
                    and source_policy == "source_base"
                    and baseline_state != "present"
                ):
                    reasons.append(f"input_not_on_source:{predecessor}")
                elif (
                    revision_valid
                    and evidence_valid
                    and source_policy == "reviewed_baseline"
                    and baseline_state not in {
                    "present",
                    "reviewed",
                    }
                ):
                    reasons.append(f"input_not_reviewed:{predecessor}")
                elif source_policy not in {"source_base", "reviewed_baseline"}:
                    reasons.append(f"required_source_policy_unknown:{predecessor}")
    if package["id"] in cycle_ids:
        reasons.append("dependency_cycle")

    paths = package.get("owned_paths")
    if paths is not None:
        if not isinstance(paths, list) or not all(
            isinstance(path, str) and bool(path.strip()) for path in paths
        ):
            reasons.append("invalid_owned_paths")
        elif not paths:
            reasons.append("owned_paths_empty")
    estimate = package.get("active_minutes_estimate")
    if estimate is not None:
        if not isinstance(estimate, dict):
            reasons.append("invalid_active_minutes_estimate")
            estimate = {}
        minimum, maximum = estimate.get("min"), estimate.get("max")
        if not (
            isinstance(minimum, int)
            and isinstance(maximum, int)
            and 0 < minimum <= maximum <= 10
        ):
            reasons.append("invalid_active_minutes_estimate")
    command = package.get("acceptance_command")
    if command is not None and (
        not isinstance(command, str)
        or not command.strip()
        or command.strip().lower().startswith("n/a")
    ):
        reasons.append("invalid_acceptance_command")
    command_kind = package.get("acceptance_command_kind")
    if command_kind is not None and command_kind not in {
        "introduce_test",
        "consume_test",
    }:
        reasons.append("invalid_acceptance_command_kind")
    if command_kind == "consume_test" and package.get("acceptance_command_verified") is not True:
        reasons.append("acceptance_command_unverified")
    if package.get("single_writer") is not None and package.get("single_writer") is not True:
        reasons.append("single_writer_not_confirmed")
    return sorted(set(reasons))


def _field_populated(package: dict[str, Any], field: str) -> bool:
    """Report whether one dispatch field is concrete enough to validate."""
    value = package.get(field)
    if field == "predecessor_ids":
        return isinstance(value, list)
    if field == "required_inputs":
        return isinstance(value, list) and all(
            isinstance(item, dict)
            and SHA_RE.fullmatch(str(item.get("accepted_revision", ""))) is not None
            and bool(str(item.get("evidence", "")).strip())
            and item.get("required_source_policy")
            in {"source_base", "reviewed_baseline"}
            for item in value
        )
    if field == "owned_paths":
        return isinstance(value, list) and bool(value)
    if field == "active_minutes_estimate":
        return isinstance(value, dict) and (
            isinstance(value.get("min"), int)
            and isinstance(value.get("max"), int)
            and 0 < value["min"] <= value["max"] <= 10
        )
    if field == "acceptance_command":
        return isinstance(value, str) and bool(value.strip())
    if field == "acceptance_command_kind":
        return value in {"introduce_test", "consume_test"}
    if field == "single_writer":
        return value is True
    raise ValueError(f"unknown coverage field: {field}")


def derive_inventory(inventory: dict[str, Any]) -> dict[str, Any]:
    """Derive diagnostics and one deterministic, write-disjoint frontier."""
    rows = inventory.get("packages")
    if not isinstance(rows, list):
        raise ValueError("inventory packages must be a list")
    packages: dict[str, dict[str, Any]] = {}
    for package in rows:
        if not isinstance(package, dict) or not isinstance(package.get("id"), str):
            raise ValueError("every package must have a string id")
        if package["id"] in packages:
            raise ValueError(f"duplicate package id: {package['id']}")
        packages[package["id"]] = package

    cycles = _cycles(packages)
    cycle_ids = {package_id for cycle in cycles for package_id in cycle}
    derived: dict[str, dict[str, Any]] = {}
    candidates: list[dict[str, Any]] = []
    for package_id in sorted(packages):
        package = packages[package_id]
        reasons = _metadata_reasons(package, packages, cycle_ids)
        derived[package_id] = {
            "id": package_id,
            "issue": package.get("issue"),
            "dispatch_state": "ELIGIBLE" if not reasons else "INELIGIBLE",
            "reasons": reasons,
            "conflicts": [],
        }
        if not reasons:
            candidates.append(package)

    conflicts: list[dict[str, Any]] = []
    for index, left in enumerate(candidates):
        for right in candidates[index + 1 :]:
            overlap = sorted(
                {
                    f"{left_path} <> {right_path}"
                    for left_path in left["owned_paths"]
                    for right_path in right["owned_paths"]
                    if paths_conflict(left_path, right_path)
                }
            )
            if overlap:
                conflicts.append(
                    {"left": left["id"], "right": right["id"], "paths": overlap}
                )
                derived[left["id"]]["conflicts"].append(right["id"])
                derived[right["id"]]["conflicts"].append(left["id"])

    frontier: list[str] = []
    owned: list[str] = []
    for package in candidates:
        if any(
            paths_conflict(candidate_path, selected_path)
            for candidate_path in package["owned_paths"]
            for selected_path in owned
        ):
            derived[package["id"]]["dispatch_state"] = "WAITING_OWNERSHIP"
            continue
        frontier.append(package["id"])
        owned.extend(package["owned_paths"])
        derived[package["id"]]["dispatch_state"] = "DISPATCHABLE"

    states = Counter(item["dispatch_state"] for item in derived.values())
    unknown_fields = Counter(
        field
        for package in packages.values()
        for field in UNKNOWN_REASONS
        if package.get(field) is None
    )
    coverage_fields = tuple(UNKNOWN_REASONS)
    field_coverage = {
        field: sum(_field_populated(package, field) for package in packages.values())
        for field in coverage_fields
    }
    return {
        "schema_version": inventory.get("schema_version"),
        "source": inventory.get("source"),
        "summary": {
            "packages": len(packages),
            "states": dict(sorted(states.items())),
            "issue_states": dict(
                sorted(Counter(row.get("issue_state", "UNKNOWN") for row in rows).items())
            ),
            "readiness": dict(
                sorted(Counter(row.get("readiness", "UNKNOWN") for row in rows).items())
            ),
            "reconciled_readiness": dict(
                sorted(
                    Counter(
                        row.get("reconciled_readiness", row.get("readiness", "UNKNOWN"))
                        for row in rows
                    ).items()
                )
            ),
            "delivery_states": dict(
                sorted(
                    Counter(row.get("delivery_state", "UNKNOWN") for row in rows).items()
                )
            ),
            "kinds": dict(sorted(Counter(row.get("kind", "UNKNOWN") for row in rows).items())),
            "unknown_fields": dict(sorted(unknown_fields.items())),
            "field_coverage": field_coverage,
            "complete_metadata": sum(
                all(_field_populated(package, field) for field in coverage_fields)
                for package in packages.values()
            ),
            "cycles": len(cycles),
            "missing_predecessors": sum(
                reason.startswith("missing_predecessor:")
                for item in derived.values()
                for reason in item["reasons"]
            ),
            "path_conflicts": len(conflicts),
        },
        "dispatchable_frontier": frontier,
        "cycles": cycles,
        "path_conflicts": conflicts,
        "packages": list(derived.values()),
    }


def main() -> int:
    """Run the inventory validator and print its derived FSM."""
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "inventory",
        nargs="?",
        type=Path,
        default=Path("docs/work-packages.json"),
    )
    parser.add_argument("--json", action="store_true", help="print the full FSM")
    args = parser.parse_args()
    result = derive_inventory(json.loads(args.inventory.read_text()))
    if args.json:
        print(json.dumps(result, indent=2, sort_keys=True))
    else:
        print(json.dumps(result["summary"], sort_keys=True))
        print("frontier:", ", ".join(result["dispatchable_frontier"]) or "none")
    return int(bool(result["cycles"] or result["summary"]["missing_predecessors"]))


if __name__ == "__main__":
    raise SystemExit(main())
