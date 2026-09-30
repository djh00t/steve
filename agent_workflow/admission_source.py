"""Refresh live GitHub admission facts without performing an integration."""

from copy import deepcopy
from urllib.parse import quote

from .source import _fetch_github


def refresh_admission(
    project, manifest, fetch_json=None, read_package=None, *, integrated=False
):
    """Replace caller PR/protection facts and verify hosted evidence artifacts.

    The manifest supplies a PR id, including ``release.pr_id`` for final handoff.
    Local checks and independent native-agent review artifacts are attested by
    their identified producers; this reader does not turn those into hosted CI.
    GitHub's current mergeability evaluates hosted branch requirements. Unknown
    mergeability or source failure remains unknown, not permission to integrate.
    """
    result = deepcopy(manifest)
    result["protection"] = {"complete": False, "satisfied": None}
    fetch = fetch_json or _fetch_github
    try:
        if not isinstance(result.get("evidence", []), list) or any(
            not isinstance(item, dict) for item in result.get("evidence", [])
        ):
            raise ValueError("Evidence must be a list of objects")
        phase = result["phase"]
        pr_id = result["pr"]["id"] if phase == "child" else result["release"]["pr_id"]
        if not isinstance(pr_id, str) or not pr_id.isdecimal():
            raise ValueError("Live admission requires a numeric PR id")
        prefix = f"repos/{project}"
        pr = fetch(f"{prefix}/pulls/{pr_id}")
        if phase == "child":
            paths, page, file_count = [], 1, 0
            while True:
                batch = fetch(f"{prefix}/pulls/{pr_id}/files?per_page=100&page={page}")
                file_count += len(batch)
                for item in batch:
                    paths.append(item["filename"])
                    if item.get("status") == "renamed":
                        paths.append(item["previous_filename"])
                if len(batch) < 100:
                    break
                page += 1
            # GitHub caps this endpoint at 3000 files; do not claim completeness.
            if file_count >= 3000:
                raise ValueError("PR file coverage is incomplete")
            result["pr"] = {
                "id": pr_id,
                "target_branch": pr["base"]["ref"],
                "head_sha": pr["head"]["sha"],
                "base_sha": pr["base"]["sha"],
                "changed_paths": list(dict.fromkeys(paths)),
            }
        else:
            result["release"] = {
                "pr_id": pr_id,
                "branch": pr["head"]["ref"],
                "target_branch": pr["base"]["ref"],
                "head_sha": pr["head"]["sha"],
                "base_sha": pr["base"]["sha"],
            }
        state = pr.get("mergeable_state")
        result["protection"] = {
            "complete": pr.get("mergeable") is not None and state != "unknown",
            "satisfied": pr.get("mergeable") is True
            and state == "clean"
            and pr["state"] == "open",
        }
        if integrated:
            branch = fetch(f"{prefix}/branches/{quote(pr['base']['ref'], safe='')}")
            rules = fetch(f"{prefix}/rulesets?includes_parents=true")
            # A merge can bypass protection; only observed absence is conclusive here.
            unprotected = branch.get("protected") is False and rules == []
            result["protection"] = {
                "complete": unprotected,
                "satisfied": pr.get("merged") is True if unprotected else None,
            }
        if phase == "final":
            _refresh_children(project, result, pr, fetch, read_package)
        hosted = [
            item for item in result.get("evidence", []) if item.get("scope") == "hosted"
        ]
        if hosted:
            runs, page = [], 1
            while True:
                batch = fetch(
                    f"{prefix}/commits/{pr['head']['sha']}/check-runs?per_page=100&page={page}"
                )
                runs.extend(batch["check_runs"])
                if len(runs) >= batch["total_count"]:
                    break
                if not batch["check_runs"]:
                    raise ValueError("Incomplete check-run page")
                page += 1
            for item in hosted:
                matching = [
                    run
                    for run in runs
                    if run["name"] == item["check_id"]
                    and run["html_url"] == item["artifact"]
                ]
                if not matching or any(
                    run["head_sha"] != pr["head"]["sha"]
                    or run["status"] != "completed"
                    or run["conclusion"] != "success"
                    for run in matching
                ):
                    item["result"] = "failed"
                    result["protection"]["satisfied"] = False
    except (OSError, KeyError, TypeError, ValueError) as exc:
        result["protection"] = {"complete": False, "satisfied": None}
        result["source_error"] = str(exc)
    return result


def _refresh_children(project, result, release_pr, fetch, read_package):
    """Reconcile release membership and serial receipts from GitHub merge facts."""
    release = result["release"]
    release["children_complete"] = False
    release["expected_children"] = []
    if read_package is None:
        raise ValueError(
            "Final live admission requires refreshed child package sources"
        )
    prefix = f"repos/{project}"
    commits, page = [], 1
    while True:
        comparison = fetch(
            f"{prefix}/compare/{release_pr['base']['sha']}...{release_pr['head']['sha']}?per_page=100&page={page}"
        )
        if comparison["merge_base_commit"]["sha"] != release_pr["base"]["sha"]:
            raise ValueError(
                "Release candidate does not include current default-branch base"
            )
        commits.extend(comparison["commits"])
        if len(commits) >= comparison["total_commits"]:
            break
        if not comparison["commits"]:
            raise ValueError("Incomplete release commit coverage")
        page += 1
    commit_ids = {item["sha"] for item in commits}
    children, page = [], 1
    while True:
        batch = fetch(
            f"{prefix}/pulls?state=closed&base={quote(release['branch'], safe='')}&per_page=100&page={page}"
        )
        children.extend(
            item
            for item in batch
            if item.get("merged_at") and item.get("merge_commit_sha") in commit_ids
        )
        if len(batch) < 100:
            break
        page += 1
    supplied = result.get("children", [])
    by_pr = {str(item["manifest"]["pr"]["id"]): item for item in supplied}
    if len(by_pr) != len(supplied) or set(by_pr) != {
        str(item["number"]) for item in children
    }:
        raise ValueError(
            "Manifest children do not cover the observed release child PR set"
        )
    for child in children:
        item = by_pr[str(child["number"])]
        if not isinstance(item["package"].get("identity"), dict):
            raise TypeError("Child identity must be an object")
        pointer = {
            key: value
            for key, value in item["package"]["identity"].items()
            if key != "revision"
        }
        package, references = read_package(pointer)
        item["package"] = package
        item["manifest"]["references"] = references
        item["manifest"] = refresh_admission(
            project, item["manifest"], fetch, integrated=True
        )
        receipt = item["integration_receipt"]
        commit = fetch(f"{prefix}/git/commits/{child['merge_commit_sha']}")
        # ponytail: merge commits only; add squash provenance when a repo needs it.
        parents = [parent["sha"] for parent in commit["parents"]]
        if len(parents) != 2 or parents != [
            receipt["release_head_before"],
            receipt["child_head_sha"],
        ]:
            raise ValueError(
                "Integration receipt does not match actual merge commit parents"
            )
        if receipt["release_head_after"] != child["merge_commit_sha"]:
            raise ValueError("Integration receipt does not match actual merge commit")
        release["expected_children"].append(
            {
                "package_identity": package["identity"],
                "pr_id": str(child["number"]),
                "head_sha": item["manifest"]["pr"]["head_sha"],
            }
        )
    release["children_complete"] = True
