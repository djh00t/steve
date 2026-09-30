"""Offline end-to-end rehearsal; all claims live in a temporary SQLite journal."""

import copy
import json
import tempfile
from datetime import datetime
from pathlib import Path

from .admission import evaluate_admission, profile_revision
from .graph import ContractError, explain
from .journal import claim, read_claims, read_receipts, record
from .routing import select_route


def run():
    """Exercise the portable path and material failure cases, returning evidence."""
    bundle = json.loads(
        (Path(__file__).parent / "examples/github_python.json").read_text()
    )
    profile, request = bundle["profile"], bundle["input"]
    snapshot, target = request["snapshot"], request["target"]
    now = datetime.fromisoformat(request["as_of"].replace("Z", "+00:00"))
    subject = next(
        p
        for p in snapshot["packages"]
        if p["identity"]["object_id"] == target["object_id"]
    )
    results = {}

    def expect_error(name, code, operation):
        try:
            operation()
        except ContractError as exc:
            assert exc.code == code, (name, exc.code)
            results[name] = code
        else:
            raise AssertionError(name + " unexpectedly succeeded")

    with tempfile.TemporaryDirectory(prefix="agent-workflow-demo-") as directory:
        profile["coordination_root"] = directory
        answer = explain(profile, snapshot, target, now)
        assert answer["status"] == "READY"
        results["ready_chain"] = answer["status"]
        altered = copy.deepcopy(snapshot)
        altered["sources"][0]["complete"] = False
        assert explain(profile, altered, target, now)["status"] == "UNKNOWN"
        results["missing_source"] = "UNKNOWN"
        altered = copy.deepcopy(snapshot)
        altered["git"][0]["release_head"] = "changed-base"
        assert (
            explain(profile, altered, target, now)["fingerprint"]
            != answer["fingerprint"]
        )
        results["changed_base"] = "fingerprint invalidated"
        subject["metadata"]["risk"] = "low"
        answer = explain(profile, snapshot, target, now)
        route = select_route(profile, subject, answer["fingerprint"])
        assert route["route"] == "inline"
        uncertain = copy.deepcopy(subject)
        uncertain["metadata"]["uncertainty"] = "investigation-needed"
        assert (
            select_route(profile, uncertain, answer["fingerprint"])["next_step"]
            == "investigate"
        )
        risky = copy.deepcopy(subject)
        risky["metadata"]["risk"] = "high"
        assert (
            select_route(profile, risky, answer["fingerprint"])["route"] == "delegated"
        )
        results["task_routes"] = ["inline", "investigation", "delegated"]
        base = snapshot["git"][0]["release_head"]
        claim_request = {
            "attempt_id": "demo-1",
            "owner": "offline-demo",
            "authority_reference": "fixture-only-no-native-dispatch",
            "expected_fingerprint": answer["fingerprint"],
            "route": route["route"],
            "step": route["next_step"],
            "base_sha": base,
            "candidate_sha": base,
        }
        reserved = claim(
            profile, target, claim_request, lambda: copy.deepcopy(snapshot), now
        )
        assert reserved["state"] == "intent"
        assert read_claims(directory)[0]["state"] == "uncertain"
        results["interrupted_handoff"] = "uncertain; reservation retained"
        expect_error(
            "no_blind_retry",
            "ATTEMPT_CONFLICT",
            lambda: claim(
                profile, target, claim_request, lambda: copy.deepcopy(snapshot), now
            ),
        )
        overlapping = copy.deepcopy(snapshot)
        overlapping["claims"] = read_claims(directory)
        assert explain(profile, overlapping, target, now)["status"] == "BLOCKED"
        results["overlapping_claim"] = "BLOCKED"
        receipt_request = {
            "attempt_id": "demo-1",
            "owner": "offline-demo",
            "package_revision": subject["identity"]["revision"],
            "base_sha": base,
            "candidate_sha": base,
            "receipt": {
                "receipt_id": "demo-reconciliation",
                "stage": "dispatch",
                "external_object": {
                    "source_type": "fixture",
                    "project": "rehearsal",
                    "object_type": "task",
                    "object_id": "demo-1",
                    "revision": "observed-1",
                },
                "result": "not-dispatched",
                "observed_at": request["as_of"],
                "evidence": {
                    "step": route["next_step"],
                    "result": "unknown",
                    "candidate_sha": base,
                    "base_sha": base,
                    "artifact": "fixture:no-native-call-was-made",
                },
            },
        }
        assert record(profile, receipt_request)["released"]
        assert record(profile, receipt_request)["released"]
        assert len(read_receipts(directory, "demo-1")) == 1
        assert read_claims(directory) == []
        results["receipt_replay"] = "idempotent; one receipt; own claim released"
        failed = [
            {
                "step": "verify",
                "result": "failed",
                "fingerprint": answer["fingerprint"],
                "candidate_sha": base,
                "base_sha": base,
            }
        ]
        assert (
            select_route(
                profile,
                subject,
                answer["fingerprint"],
                failed,
                candidate_sha=base,
                base_sha=base,
            )["next_step"]
            == "diagnose"
        )
        results["failed_check"] = "diagnose -> repair -> verify -> review"
        manifest = {
            "schema_version": 1,
            "profile_revision": profile_revision(profile),
            "phase": "child",
            "requested_action": "admit-child",
            "candidate_sha": base,
            "base_sha": base,
            "ticket_revision": subject["identity"]["revision"],
            "implementer": "builder",
            "references": [
                {
                    "target": {
                        k: v for k, v in p["identity"].items() if k != "revision"
                    },
                    "revision": p["identity"]["revision"],
                }
                for p in snapshot["packages"] + snapshot["references"]
            ],
            "pr": {
                "id": "1",
                "target_branch": subject["metadata"]["release"],
                "head_sha": base,
                "base_sha": base,
                "changed_paths": subject["metadata"]["owned_paths"],
            },
            "reviews": [
                {
                    "reviewer": "independent-reviewer",
                    "independent": True,
                    "verdict": "accepted",
                    "candidate_sha": base,
                    "base_sha": base,
                    "source": {
                        "source_type": "fixture",
                        "project": "rehearsal",
                        "object_type": "review",
                        "object_id": "1",
                    },
                }
            ],
            "protection": {"complete": True, "satisfied": True},
            "required_check_ids": subject["metadata"]["check_ids"],
            "evidence": [
                {
                    "stage": "verify",
                    "criterion_id": "C1",
                    "check_id": "workflow",
                    "result": "passed",
                    "scope": "local",
                    "artifact": "fixture:demo",
                    "candidate_sha": base,
                    "base_sha": base,
                    "environment": "fixture",
                    "configuration": "offline",
                }
            ],
        }
        for item in manifest["reviews"] + manifest["evidence"]:
            item["profile_revision"] = profile_revision(profile)
        assert evaluate_admission(profile, subject, manifest)["status"] == "ADMITTED"
        final = copy.deepcopy(manifest)
        final.update(
            phase="final",
            requested_action="human-handoff",
            candidate_sha="merge-2",
            integration_base_sha=base,
            children=[],
        )
        final.pop("pr")
        final["release"] = {
            "pr_id": "release-demo",
            "target_branch": "main",
            "branch": subject["metadata"]["release"],
            "head_sha": "merge-2",
            "base_sha": base,
            "children_complete": True,
            "expected_children": [],
        }
        for item in final["reviews"] + final["evidence"]:
            item["candidate_sha"] = "merge-2"
        before = base
        for index in (1, 2):
            child_package = copy.deepcopy(subject)
            child_package["identity"]["object_id"] = f"child-{index}"
            child = copy.deepcopy(manifest)
            child.update(candidate_sha=f"child-head-{index}", base_sha=before)
            child["pr"].update(
                id=str(index), head_sha=child["candidate_sha"], base_sha=before
            )
            for item in child["reviews"] + child["evidence"]:
                item.update(candidate_sha=child["candidate_sha"], base_sha=before)
            final["children"].append(
                {
                    "package": child_package,
                    "manifest": child,
                    "integration_receipt": {
                        "receipt_id": f"integration-{index}",
                        "result": "integrated",
                        "child_pr_id": str(index),
                        "child_head_sha": child["candidate_sha"],
                        "release_head_before": before,
                        "release_head_after": f"merge-{index}",
                        "final_candidate_sha": "merge-2",
                    },
                }
            )
            final["release"]["expected_children"].append(
                {
                    "package_identity": child_package["identity"],
                    "pr_id": str(index),
                    "head_sha": child["candidate_sha"],
                }
            )
            before = f"merge-{index}"
        handoff = evaluate_admission(profile, subject, final)
        assert handoff["status"] == "HANDOFF", handoff
        results["serial_child_integration"] = "two exact child receipts -> HANDOFF"
        final["children"].reverse()
        assert evaluate_admission(profile, subject, final)["status"] == "BLOCKED"
        results["out_of_order_integration"] = "BLOCKED"
        manifest["pr"]["target_branch"] = "main"
        assert evaluate_admission(profile, subject, manifest)["status"] == "BLOCKED"
        results["wrong_child_target"] = "BLOCKED"
        manifest["requested_action"] = "merge-release-to-main"
        verdict = evaluate_admission(profile, subject, manifest)
        assert any(item["code"] == "FORBIDDEN_ACTION" for item in verdict["reasons"])
        results["agent_release_merge"] = "BLOCKED"
    results["cleanup"] = "temporary journal removed"
    return {"schema_version": 1, "mode": "fixture", "ok": True, "scenarios": results}


if __name__ == "__main__":
    print(json.dumps(run(), indent=2, sort_keys=True))
