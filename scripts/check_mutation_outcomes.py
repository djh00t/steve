#!/usr/bin/env python3
"""Check the two named cargo-mutants qualifications and print their evidence."""

from __future__ import annotations

import json
import re
import sys
from pathlib import Path
from typing import Any

EXPECTED = {
    "cancel": {
        "function": "<impl Drop for DisconnectStream>::drop",
        "baseline_tests": [
            "messages_disconnect_cancels_upstream",
            "responses_disconnect_cancels_upstream",
        ],
        "mutant_test": "messages_disconnect_cancels_upstream",
        "assertion": "upstream body was not dropped after client disconnect",
    },
    "replay": {
        "function": "ReplayGate::ensure_can_attempt",
        "baseline_tests": ["proxy::stream::tests::no_replay_after_output_begun"],
        "mutant_test": "proxy::stream::tests::no_replay_after_output_begun",
        "assertion": "assertion failed: gate.ensure_can_attempt().is_err()",
    },
}
ZERO_COUNTS = ("missed", "timeout", "unviable", "success")


class QualificationError(ValueError):
    """An outcome did not prove the named qualification."""


def _require(condition: bool, message: str) -> None:
    if not condition:
        raise QualificationError(message)


def _phase(outcome: dict[str, Any], name: str) -> dict[str, Any]:
    return next(
        (
            phase
            for phase in outcome.get("phase_results", [])
            if phase.get("phase") == name
        ),
        {},
    )


def _log_path(root: Path, outcome: dict[str, Any]) -> Path:
    path = (root / outcome["log_path"]).resolve()
    _require(
        path.is_relative_to(root.resolve()), "outcome log path escapes mutants.out"
    )
    return path


def _proof_lines(log: str) -> list[str]:
    return [
        line.strip()
        for line in log.splitlines()
        if re.search(
            r"\*\*\* cargo test|running \d+ tests?|^test |test result:|"
            r"upstream body was not dropped|assertion failed: gate\.ensure_can_attempt",
            line,
        )
    ]


def _check_test_log(log: str, test_name: str, passed: bool, label: str) -> None:
    _require(
        sum(int(count) for count in re.findall(r"running (\d+) tests?", log)) > 0,
        f"{label} executed zero selected tests",
    )
    status = "ok" if passed else "FAILED"
    _require(
        re.search(rf"^test {re.escape(test_name)} \.\.\. {status}$", log, re.M)
        is not None,
        f"{label} did not report {test_name} as {status}",
    )


def validate_list(kind: str, path: Path) -> None:
    """Require cargo-mutants to select the single intended function mutant."""
    _require(kind in EXPECTED, f"unknown mutation kind: {kind}")
    selected = json.loads(path.read_text(encoding="utf-8"))
    target = EXPECTED[kind]["function"]
    _require(
        isinstance(selected, list) and len(selected) == 1,
        "--list must select exactly one mutant",
    )
    mutant = selected[0]
    _require(isinstance(mutant, dict), "--list returned a malformed mutant")
    _require(
        mutant.get("file") == "src/proxy/stream.rs",
        "--list selected the wrong source file",
    )
    _require(
        mutant.get("function", {}).get("function_name") == target,
        f"--list did not select {target}",
    )
    print(f"selection {kind}: 1 mutant, {target}")


def validate_outcomes(kind: str, root: Path, tested_sha: str) -> None:
    """Print compact log evidence and enforce one intended caught mutant."""
    _require(kind in EXPECTED, f"unknown mutation kind: {kind}")
    root = root.resolve()
    evidence = root / "outcomes.json"
    print(f"mutation {kind} head={tested_sha} outcomes={evidence}")
    _require(evidence.is_file(), f"missing outcomes file: {evidence}")
    data = json.loads(evidence.read_text(encoding="utf-8"))
    _require(isinstance(data, dict), "outcomes file is not a JSON object")
    counts = {key: data.get(key) for key in ("total_mutants", "caught", *ZERO_COUNTS)}
    print(
        f"mutation {kind} head={tested_sha} version={data.get('cargo_mutants_version')} counts={counts}"
    )
    outcomes = data.get("outcomes")
    _require(isinstance(outcomes, list), "outcomes is not a JSON array")
    _require(
        all(isinstance(outcome, dict) for outcome in outcomes),
        "outcomes contains a malformed row",
    )
    baseline = next(
        (outcome for outcome in outcomes if outcome.get("scenario") == "Baseline"),
        None,
    )
    mutant_rows = [
        outcome
        for outcome in outcomes
        if isinstance(outcome.get("scenario"), dict)
        and isinstance(outcome["scenario"].get("Mutant"), dict)
    ]
    logs: dict[str, str] = {}
    for label, outcome in (
        ("baseline", baseline),
        ("mutant", mutant_rows[0] if mutant_rows else None),
    ):
        if outcome is None:
            print(f"{kind} {label} evidence: missing")
            continue
        try:
            log = _log_path(root, outcome).read_text(encoding="utf-8", errors="replace")
        except (KeyError, OSError, QualificationError) as error:
            print(f"{kind} {label} evidence: unavailable ({error})")
            continue
        logs[label] = log
        print(f"{kind} {label} log:")
        for line in _proof_lines(log):
            print(f"  {line}")

    _require(
        data.get("cargo_mutants_version") == "27.1.0",
        "cargo-mutants version is not 27.1.0",
    )
    _require(
        data.get("total_mutants") == 1 and data.get("caught") == 1,
        "expected one caught mutant",
    )
    _require(
        all(data.get(field) == 0 for field in ZERO_COUNTS),
        "missed, timed out, unviable, or surviving mutant",
    )
    _require(len(outcomes) == 2, "expected exactly baseline and one mutant outcome")
    _require(
        baseline is not None and baseline.get("summary") == "Success",
        "baseline did not succeed",
    )
    _require(
        len(mutant_rows) == 1 and mutant_rows[0].get("summary") == "CaughtMutant",
        "target mutant was not caught",
    )

    mutant = mutant_rows[0]["scenario"]["Mutant"]
    target = EXPECTED[kind]
    _require(
        mutant.get("file") == "src/proxy/stream.rs", "mutant has the wrong source file"
    )
    _require(
        mutant.get("function", {}).get("function_name") == target["function"],
        "caught mutant is not the named function",
    )
    _require(
        mutant.get("name", "").find(target["function"]) >= 0,
        "mutant name does not identify the named function",
    )
    _require(
        _phase(baseline, "Build").get("process_status") == "Success",
        "baseline build did not succeed",
    )
    _require(
        _phase(baseline, "Test").get("process_status") == "Success",
        "baseline test phase did not succeed",
    )
    _require(
        _phase(mutant_rows[0], "Build").get("process_status") == "Success",
        "mutant did not build",
    )
    _require(
        _phase(mutant_rows[0], "Test").get("process_status") == {"Failure": 101},
        "mutant test did not fail with assertion exit 101",
    )

    baseline_log = logs.get("baseline", "")
    mutant_log = logs.get("mutant", "")
    for test_name in target["baseline_tests"]:
        _check_test_log(baseline_log, test_name, True, "baseline")
    _check_test_log(mutant_log, target["mutant_test"], False, "mutant")
    _require(
        target["assertion"] in mutant_log,
        "mutant failed without the expected assertion",
    )
    print(f"mutation {kind}: caught {target['function']} at head {tested_sha}")


def main(argv: list[str] | None = None) -> int:
    args = list(sys.argv[1:] if argv is None else argv)
    try:
        if len(args) == 3 and args[0] == "list":
            validate_list(args[1], Path(args[2]))
        elif len(args) == 4 and args[0] == "verify":
            validate_outcomes(args[1], Path(args[2]), args[3])
        else:
            raise QualificationError(
                "usage: check_mutation_outcomes.py list KIND JSON | verify KIND MUTANTS.OUT SHA"
            )
    except (
        OSError,
        json.JSONDecodeError,
        KeyError,
        TypeError,
        QualificationError,
    ) as error:
        print(f"mutation qualification failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
