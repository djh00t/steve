"""Pure child and final-release admission checks."""

import hashlib
import json

from agent_workflow.routing import select_route


def profile_revision(profile):
    """Return the SHA-256 revision of canonical reviewed profile JSON."""
    canonical = json.dumps(
        profile, sort_keys=True, separators=(",", ":"), ensure_ascii=False
    )
    return hashlib.sha256(canonical.encode("utf-8")).hexdigest()


def _filled(value):
    return isinstance(value, str) and bool(value.strip())


def _pointer(value):
    return isinstance(value, dict) and all(
        _filled(value.get(key))
        for key in ("source_type", "project", "object_type", "object_id")
    )


def _malformed(message):
    return {
        "schema_version": 1,
        "ok": False,
        "error": {"code": "MALFORMED_INPUT", "message": message, "details": []},
    }


def evaluate_admission(profile, package, manifest):
    """Evaluate a refreshed admission manifest; never perform an action.

    ``profile`` and ``package`` are already validated contract objects. The
    manifest is a mapping with schema_version 1, phase (child/final),
    requested_action, candidate_sha, base_sha, ticket_revision,
    profile_revision, implementer,
    references (target pointer + observed revision), reviews, protection,
    required_check_ids and evidence. Child adds pr (id, target_branch,
    head_sha, base_sha, changed_paths); final adds release (pr_id,
    target_branch, branch, head_sha, base_sha) and nonempty children. Each child
    has a validated package, child manifest and integration receipt binding
    its PR/head to a serial release revision chain ending at the final
    candidate. Evidence adds an adapter-attested scope (local, hosted,
    integrated, operational). The caller must refresh all source observations
    and enforce authority before any subsequent external action.
    """
    if (
        not isinstance(manifest, dict)
        or type(manifest.get("schema_version")) is not int
        or manifest["schema_version"] != 1
    ):
        return _malformed("manifest schema_version must be 1")
    phase = manifest.get("phase")
    if phase not in ("child", "final"):
        return _malformed("phase must be child or final")
    for key in (
        "requested_action",
        "candidate_sha",
        "base_sha",
        "ticket_revision",
        "profile_revision",
        "implementer",
    ):
        if not _filled(manifest.get(key)):
            return _malformed(f"{key} must be a nonempty string")
    for key in ("references", "reviews", "required_check_ids", "evidence"):
        if not isinstance(manifest.get(key), list):
            return _malformed(f"{key} must be a list")
    if any(not _filled(check_id) for check_id in manifest["required_check_ids"]):
        return _malformed("required_check_ids must contain nonempty strings")
    if any(
        not isinstance(item, dict)
        or not _pointer(item.get("target"))
        or not _filled(item.get("revision"))
        for item in manifest["references"]
    ):
        return _malformed("references must contain target pointers and revisions")
    if any(
        not isinstance(item, dict)
        for item in manifest["reviews"] + manifest["evidence"]
    ):
        return _malformed("reviews and evidence must contain objects")
    protection = manifest.get("protection")
    if (
        not isinstance(protection, dict)
        or type(protection.get("complete")) is not bool
        or protection.get("satisfied") is not None
        and type(protection.get("satisfied")) is not bool
    ):
        return _malformed("protection must contain complete and satisfied observations")
    observed = manifest.get("pr" if phase == "child" else "release")
    if not isinstance(observed, dict):
        return _malformed("phase observation must be an object")
    fields = (
        ("id", "target_branch", "head_sha", "base_sha")
        if phase == "child"
        else ("pr_id", "target_branch", "branch", "head_sha", "base_sha")
    )
    if any(not _filled(observed.get(key)) for key in fields):
        return _malformed("phase observation has missing identity or revision")
    if phase == "child" and (
        not isinstance(observed.get("changed_paths"), list)
        or any(not _filled(path) for path in observed["changed_paths"])
    ):
        return _malformed("pr changed_paths must be a list of paths")
    if phase == "final" and not _filled(manifest.get("integration_base_sha")):
        return _malformed("integration_base_sha must be a nonempty string")
    if phase == "final" and (
        type(observed.get("children_complete")) is not bool
        or not isinstance(observed.get("expected_children"), list)
        or any(
            not isinstance(item, dict)
            or not isinstance(item.get("package_identity"), dict)
            or not all(
                _filled(item["package_identity"].get(key))
                for key in (
                    "source_type",
                    "project",
                    "object_type",
                    "object_id",
                    "revision",
                )
            )
            or not _filled(item.get("pr_id"))
            or not _filled(item.get("head_sha"))
            for item in observed["expected_children"]
        )
    ):
        return _malformed("release child observation is malformed")
    if phase == "final" and (
        not isinstance(manifest.get("children"), list)
        or any(
            not isinstance(child, dict)
            or not isinstance(child.get("package"), dict)
            or not isinstance(child.get("manifest"), dict)
            or not isinstance(child.get("integration_receipt"), dict)
            for child in manifest["children"]
        )
    ):
        return _malformed("final children must be a nonempty list of admission records")

    metadata = package["metadata"]
    head, base = manifest["candidate_sha"], manifest["base_sha"]
    current_profile_revision = profile_revision(profile)
    reasons = []

    def block(code):
        reasons.append({"code": code, "message": code.replace("_", " ").lower()})

    action = manifest["requested_action"]
    if action != ("admit-child" if phase == "child" else "human-handoff"):
        block("FORBIDDEN_ACTION")
    if manifest["ticket_revision"] != package["identity"]["revision"]:
        block("STALE_TICKET")
    if manifest["profile_revision"] != current_profile_revision:
        block("STALE_PROFILE")
    if any(
        item.get("profile_revision") != current_profile_revision
        for item in manifest["reviews"] + manifest["evidence"]
    ):
        block("STALE_PROFILE_EVIDENCE")
    refs = {}
    for item in manifest["references"]:
        key = tuple(
            item["target"][name]
            for name in ("source_type", "project", "object_type", "object_id")
        )
        refs.setdefault(key, set()).add(item["revision"])
    unknown = False
    for edge in metadata["requires"] + metadata["contracts"]:
        key = tuple(
            edge["target"][name]
            for name in ("source_type", "project", "object_type", "object_id")
        )
        found = refs.get(key)
        if not found:
            reasons.append(
                {
                    "code": "REFERENCE_UNAVAILABLE",
                    "message": "reference observation unavailable",
                }
            )
            unknown = True
        elif found != {edge["accepted_revision"]}:
            block("STALE_REFERENCE")

    if phase == "child":
        bindings = [
            project["git"]
            for project in profile["projects"].values()
            if project["tracker"].get("source_type")
            == package["identity"]["source_type"]
            and project["tracker"]["project"] == package["identity"]["project"]
        ]
        if (
            len(bindings) != 1
            or not all(
                _filled(bindings[0].get(key))
                for key in ("release_branch", "default_branch")
            )
            or bindings[0]["release_branch"] == bindings[0]["default_branch"]
        ):
            block("INVALID_RELEASE_BINDING")
        else:
            if metadata["release"] != bindings[0]["release_branch"]:
                block("WRONG_PACKAGE_RELEASE")
            if observed["target_branch"] != bindings[0]["release_branch"]:
                block("WRONG_PR_TARGET")
            if observed["target_branch"] == bindings[0]["default_branch"]:
                block("DEFAULT_BRANCH_CHILD_TARGET")
        if observed["target_branch"] != metadata["release"]:
            block("WRONG_PR_TARGET")
        if observed["head_sha"] != head or observed["base_sha"] != base:
            block("STALE_PR_REVISION")
        owned = metadata["owned_paths"]
        for path in observed["changed_paths"]:
            if (
                path.startswith("/")
                or any(part in ("", ".", "..") for part in path.split("/"))
                or not any(
                    path == item or path.startswith(item + "/") for item in owned
                )
            ):
                block("OUT_OF_SCOPE_PATH")
                break
    else:
        if (
            observed["branch"] != metadata["release"]
            or observed["head_sha"] != head
            or observed["base_sha"] != base
        ):
            block("STALE_RELEASE_REVISION")
        default_branches = [
            item["git"]["default_branch"]
            for item in profile["projects"].values()
            if item["tracker"]["project"] == package["identity"]["project"]
        ]
        if (
            len(default_branches) != 1
            or observed["target_branch"] != default_branches[0]
        ):
            block("WRONG_RELEASE_PR_TARGET")
        if not manifest["children"]:
            block("NO_CHILDREN")
        if not observed["children_complete"]:
            reasons.append(
                {
                    "code": "RELEASE_CHILDREN_UNAVAILABLE",
                    "message": "release child observation incomplete",
                }
            )
            unknown = True
        expected_children = {
            tuple(
                item["package_identity"][name]
                for name in ("source_type", "project", "object_type", "object_id")
            )
            + (item["package_identity"]["revision"], item["pr_id"], item["head_sha"])
            for item in observed["expected_children"]
        }
        if len(expected_children) != len(observed["expected_children"]):
            block("DUPLICATE_EXPECTED_CHILD")
        observed_children = []
        seen_prs, seen_heads = set(), set()
        release_before = manifest["integration_base_sha"]
        for child in manifest["children"]:
            child_package = child["package"]
            child_manifest = child["manifest"]
            receipt = child["integration_receipt"]
            if (
                child_manifest.get("phase") != "child"
                or not isinstance(child_package.get("identity"), dict)
                or not isinstance(child_package.get("metadata"), dict)
            ):
                return _malformed(
                    "child entry needs a validated package and child manifest"
                )
            identity = child_package["identity"]
            if not all(
                _filled(identity.get(key))
                for key in (
                    "source_type",
                    "project",
                    "object_type",
                    "object_id",
                    "revision",
                )
            ):
                return _malformed("child package identity is incomplete")
            try:
                child_result = evaluate_admission(
                    profile, child_package, child_manifest
                )
            except (KeyError, TypeError, ValueError):
                return _malformed("child package or manifest is malformed")
            if "error" in child_result:
                return _malformed("child manifest is malformed")
            if child_result.get("status") != "ADMITTED":
                block("CHILD_NOT_ADMITTED")
            child_pr = child_manifest.get("pr")
            if not isinstance(child_pr, dict):
                return _malformed("child pr observation must be an object")
            pr_id, child_head = child_pr.get("id"), child_pr.get("head_sha")
            if not _filled(pr_id) or not _filled(child_head):
                return _malformed("child pr identity and head must be nonempty")
            observed_children.append(
                tuple(
                    identity[name]
                    for name in (
                        "source_type",
                        "project",
                        "object_type",
                        "object_id",
                        "revision",
                    )
                )
                + (pr_id, child_head)
            )
            if pr_id in seen_prs or child_head in seen_heads:
                block("DUPLICATE_CHILD")
            seen_prs.add(pr_id)
            seen_heads.add(child_head)
            if (
                child_package["metadata"].get("release") != metadata["release"]
                or not all(
                    _filled(receipt.get(key))
                    for key in (
                        "receipt_id",
                        "child_pr_id",
                        "child_head_sha",
                        "release_head_before",
                        "release_head_after",
                        "final_candidate_sha",
                    )
                )
                or receipt.get("result") != "integrated"
                or receipt.get("child_pr_id") != pr_id
                or receipt.get("child_head_sha") != child_head
                or receipt.get("release_head_before") != release_before
                or receipt.get("release_head_before") != child_pr.get("base_sha")
                or receipt.get("final_candidate_sha") != head
            ):
                block("CHILD_INTEGRATION_MISMATCH")
            release_before = receipt.get("release_head_after")
        if release_before != head:
            block("FINAL_CANDIDATE_NOT_CHILD_RESULT")
        if set(observed_children) != expected_children or len(observed_children) != len(
            expected_children
        ):
            block("CHILD_SET_MISMATCH")

    if not protection["complete"] or protection["satisfied"] is None:
        reasons.append(
            {
                "code": "PROTECTION_UNAVAILABLE",
                "message": "protection observation unavailable",
            }
        )
        unknown = True
    elif protection["satisfied"] is False:
        block("PROTECTION_UNSATISFIED")

    current_reviews = [
        review
        for review in manifest["reviews"]
        if review.get("candidate_sha") == head and review.get("base_sha") == base
    ]
    if any(review.get("verdict") == "rejected" for review in current_reviews):
        block("REVIEW_REJECTED")
    if not any(
        review.get("verdict") == "accepted"
        and review.get("independent") is True
        and _filled(review.get("reviewer"))
        and review["reviewer"] != manifest["implementer"]
        and _pointer(review.get("source"))
        for review in current_reviews
    ):
        block("INDEPENDENT_REVIEW_MISSING")

    if phase in ("child", "final"):
        required = set(manifest["required_check_ids"])
        try:
            minimum = set(
                select_route(profile, package, current_profile_revision)["check_ids"]
            )
        except (KeyError, ValueError):
            block("INVALID_REQUIRED_CHECKS")
            minimum = set()
        if not minimum.issubset(required) or not required.issubset(profile["checks"]):
            block("INVALID_REQUIRED_CHECKS")
        check_scopes = {
            check_id: profile["checks"][check_id].get("evidence_scope", "local")
            for check_id in required & profile["checks"].keys()
        }
        if any(
            scope not in ("local", "hosted", "integrated", "operational")
            for scope in check_scopes.values()
        ):
            block("INVALID_CHECK_SCOPE")
        current = [
            item
            for item in manifest["evidence"]
            if item.get("candidate_sha") == head and item.get("base_sha") == base
        ]

        def actual(item):
            return (
                all(
                    _filled(item.get(key))
                    for key in (
                        "stage",
                        "artifact",
                        "environment",
                        "configuration",
                        "check_id",
                    )
                )
                and item.get("check_id") in profile["checks"]
            )

        for criterion in metadata["criteria"]:
            matches = [
                item for item in current if item.get("criterion_id") == criterion["id"]
            ]
            if (
                not matches
                or any(
                    not actual(item)
                    or item.get("scope") != criterion["evidence_scope"]
                    or item.get("result") not in ("passed", "inapplicable")
                    or (
                        item.get("result") == "inapplicable"
                        and not _filled(item.get("reason"))
                    )
                    for item in matches
                )
                or len({item.get("result") for item in matches}) > 1
            ):
                block("CRITERION_EVIDENCE_INVALID")
        for check_id in required:
            matches = [item for item in current if item.get("check_id") == check_id]
            if not any(
                item.get("result") == "passed"
                and actual(item)
                and item.get("scope") == check_scopes.get(check_id)
                for item in matches
            ) or any(item.get("result") == "failed" for item in matches):
                block("CHECK_EVIDENCE_INVALID")

    if reasons:
        return {
            "schema_version": 1,
            "ok": False,
            "status": "UNKNOWN" if unknown else "BLOCKED",
            "reasons": reasons,
        }
    return {
        "schema_version": 1,
        "ok": True,
        "status": "ADMITTED" if phase == "child" else "HANDOFF",
        "next_step": "release-integration-review"
        if phase == "child"
        else "human-handoff",
        "reasons": [],
    }
