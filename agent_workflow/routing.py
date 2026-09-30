"""Pure, read-only task route and check selection."""

from itertools import pairwise

STEP_CLASS = {
    "investigate": "investigation",
    "contract": "contract",
    "implement": "implementation",
    "diagnose": "diagnosis",
    "repair": "repair",
    "verify": "verification",
    "review": "independent-review",
    "integrate": "integration",
    "handoff": "human-handoff",
}


def select_route(
    profile,
    package,
    fingerprint,
    observations=(),
    affected_check_ids=(),
    *,
    candidate_sha=None,
    base_sha=None,
):
    """Select a route from validated profile/package data without acting on it.

    ``package`` is a snapshot entry with ``identity`` and ``metadata``. The
    caller supplies the current source/graph ``fingerprint`` and current
    ``candidate_sha``/``base_sha`` when code evidence exists. Each ordered
    observation contains ``step``, ``result`` (passed/failed/unknown), and
    ``fingerprint``. Verify, review, integrate and handoff observations also
    require nonempty ``candidate_sha``/``base_sha`` matching the current pair;
    failed verification has the same rule. Earlier investigation, contract,
    implementation, diagnosis and repair progress can be fingerprint-bound
    without a candidate. Missing or changed SHA refs cannot advance code
    evidence. ``affected_check_ids`` contains
    reviewed contract/impact check IDs found by the graph producer; all IDs
    must occur in ``profile['checks']``. The result is advice, not authority.
    """
    metadata = package["metadata"]
    impact = metadata["impact"]
    declared = set(metadata["check_ids"]) | set(affected_check_ids)
    if impact == "unknown":
        selected = set(profile["checks"])
    else:
        declared.update(impact["check_ids"])
        selected = declared
    unknown = declared - profile["checks"].keys()
    if unknown:
        raise ValueError(f"unknown check ID: {', '.join(sorted(unknown))}")

    kind = metadata["kind"]
    uncertain = metadata["uncertainty"] == "investigation-needed"
    steps = {
        "investigation": ["investigate"],
        "contract": ["contract"],
        "implementation": ["implement"],
        "validation": [],
    }[kind].copy()
    if uncertain and kind == "implementation":
        steps = ["investigate", "contract", "implement"]
    elif uncertain and kind != "investigation":
        steps.insert(0, "investigate")
    steps += ["verify", "review", "integrate", "handoff"]
    recovery = ["diagnose", "repair", "verify", "review", "integrate", "handoff"]
    revision_bound = {"verify", "review", "integrate", "handoff"}
    position = 0
    for observation in observations:
        if observation["fingerprint"] != fingerprint:
            continue
        if observation["step"] in revision_bound and not (
            isinstance(candidate_sha, str)
            and candidate_sha.strip()
            and isinstance(base_sha, str)
            and base_sha.strip()
            and observation.get("candidate_sha") == candidate_sha
            and observation.get("base_sha") == base_sha
        ):
            continue
        if observation["step"] == "verify" and observation["result"] == "failed":
            steps, position = recovery, 0
            continue
        if (
            position < len(steps)
            and observation["step"] == steps[position]
            and observation["result"] == "passed"
        ):
            position += 1

    risk = metadata["risk"]
    route = (
        "investigation"
        if uncertain or kind == "investigation"
        else ("inline" if risk == "low" else "delegated")
    )
    effort = "high" if risk == "high" else "medium"
    capability = next(
        (
            {"model": item["model"], "effort": effort}
            for item in profile["capabilities"]
            if effort in item["efforts"]
        ),
        None,
    )
    transitions = [
        {"from": source, "on": "passed", "to": target}
        for source, target in pairwise(steps)
    ]
    transitions.append({"from": "verify", "on": "failed", "to": "diagnose"})
    next_step = steps[position] if position < len(steps) else None
    return {
        "route": route,
        "steps": steps,
        "transitions": transitions,
        "next_step": next_step,
        "next_step_class": STEP_CLASS[next_step] if next_step else "complete",
        "check_ids": sorted(selected),
        "capability": capability,
        "unavailable_reason": None
        if capability
        else f"no configured capability supports {effort} effort",
    }
