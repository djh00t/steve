# Proposal: STV-M8-02 native macOS distribution and update channel

**Issue:** [#336](https://github.com/djh00t/steve/issues/336)
**Status:** Proposal only; pending explicit David/Cos review and acceptance of this exact revision.
**Decision owner:** David/Cos.
**Scope:** One macOS developer/MVP native channel. No release, package, updater, signing workflow, or credentials are created by this proposal.

## Decision question

Which single native install/update source should M8 build around, and what trust, version, staging, rollback, and unsupported-source rules must consumers use?

## Proposed decision

Use one direct-distribution channel: a versioned, signed and notarized macOS `.pkg` published as a stable asset on the Steve GitHub Release. The package installs the optional menu-bar app, companion TUI and native daemon/service. Treat the package as the source of truth for a managed native installation. Do not add Homebrew, Mac App Store, or another native channel in this MVP.

The app remains optional. The verified package is the distribution/staging artifact, but an update must pass through the supervisor/coordinator contract in the existing runtime specification. Package installation must never replace the active worker or stop service before candidate readiness and cutover. No generic automatic-updater framework is proposed.

| Concern | Proposed contract |
|---|---|
| Release source | Stable `.pkg` asset from the latest non-draft, non-prerelease `djh00t/steve` GitHub Release. Release metadata locates a candidate; it is not artifact trust. |
| Publisher trust | Sign every distributed executable (app, daemon, helpers) with the publisher's **Developer ID Application** identity, hardened runtime, secure timestamp, and no `get-task-allow=true`. Sign the installer with **Developer ID Installer**. Notarize the final package and staple its ticket. |
| Identity pinning | At implementation, pin the actual Apple Team ID observed on a David/Cos-approved signed release and require the same Team ID for installed app/daemon and candidate package. The Team ID, signing identities, Apple account, and credentials are **unknown/unavailable today**; do not guess them or publish a package without them. Reject unsigned, ad-hoc, mismatched, invalid, revoked, or non-notarized artifacts. |
| Version check | Read the running daemon's installed version from existing `GET /api/v1/system/version`; compare SemVer with the stable GitHub Release tag. The release API is discovery only. If API access fails, keep installed state and show “update check unavailable”; never infer an update or downgrade. |
| Stage and install | Download and fully validate the exact `.pkg` (Developer ID Installer signature and Team ID, Gatekeeper install assessment, stapled ticket, version, and platform) before touching the active installation. Stage the candidate at a versioned path; the supervisor starts it with the same configuration, waits for migration/compatibility checks and readiness, then switches new work to it and drains the old worker. The package installer must not replace or stop the serving worker before this coordinator boundary. Preserve the current worker and stable endpoint throughout staging and readiness checks. |
| Rollback | Keep the old worker available through cutover completion. If the candidate fails before cutover completes, the supervisor automatically routes work back to the old worker, as required by the runtime spec; do not substitute a manual reinstall. Migrations must use expand/contract so old and new workers can coexist; defer destructive cleanup until every old worker has exited. A candidate that cannot meet this constraint is blocked before migration. Manual operator recovery is reserved for exceptional data incompatibility that prevents the previous binary from safely running, and must preserve the data; it is not the normal update rollback path. |
| Unknown/unsupported source | Enable neither update check nor update/install controls unless a valid Steve package receipt and the expected signed app/daemon identity establish this managed channel. Disable them for `cargo install`/`make install`, source-tree builds, copied binaries, containers, remote deployments, missing receipts, and unknown/mismatched signatures. Show the detected source and direct the operator to that source's own update path. |

**Implementation gate:** The existing runtime specification requires a supervisor that stages a versioned worker, checks compatibility and readiness, switches traffic, drains the old worker, and routes back automatically if the candidate fails before cutover completes. That coordinator is not implemented in this checkout. The implementation leaves produced by #350 remain blocked until the coordinator exists and its real-process behavior is qualified. #350 is a decomposition packet and can proceed once its listed #336/#337/#89/#293 prerequisites are accepted; it must define those implementation gates rather than wait for their code to exist. A package-only overwrite, service stop/restart, or manual reinstall is not an update path.

## Negative example

Do not enable “Update Steve” merely because `/api/v1/system/version` responds or a newer tag/checksum appears on GitHub. A locally built Cargo binary can report the same version while having no managed package receipt or approved publisher signature. Likewise, a SHA-256 digest published beside the `.pkg` in the same release is useful for transfer diagnostics but is not independent publisher authentication. In every such case, disable update controls.

## Evidence and current-state boundary

- `Makefile` currently provides `make install` (`cargo install --path . --force`) and `make update` (`git pull --ff-only`, then `make install`): developer source workflows, not signed release installers or rollback-safe updates.
- `src/server.rs` already serves `GET /api/v1/system/version` with the daemon's compiled Cargo package version. It does not identify the install source or a release channel.
- `docs/specs/2026-09-26-steve-gateway.md`, “Lifecycle, draining, and hitless upgrades,” requires versioned candidate staging, compatibility/readiness checks, traffic switch, old-worker drain, automatic route-back on candidate failure before cutover completes, and expand/contract compatibility for mixed-version migrations. `src/lifecycle.rs` implements process lifecycle/drain tracking only; it is not the upgrade coordinator.
- `.github/` currently has CI only; this checkout has no macOS package/release signing and notarization workflow. No Team ID, Developer ID identity, notarization credential, package receipt, installer, macOS app, native service registration, or updater is evidenced here.
- Apple's direct-distribution guidance calls for Developer ID signing and notarization; notarization preparation requires signed executables and hardened runtime. Apple's packaging guidance covers signed/notarized direct `.pkg` distribution and ticket stapling. [Notarizing macOS software before distribution](https://developer.apple.com/documentation/security/notarizing-macos-software-before-distribution), [Packaging Mac software for distribution](https://developer.apple.com/documentation/xcode/packaging-mac-software-for-distribution).
- GitHub documents a “Get the latest release” endpoint; its `draft`/`prerelease` metadata can filter the discovery candidate but provides no substitute for validating Apple's package trust. [GitHub REST release endpoints](https://docs.github.com/en/rest/releases/releases).

## Consumer gates after acceptance

These issues remain blocked until they reference the accepted artifact revision and resize their work against existing source seams:

- **#341 native installation decomposition:** defines the package layout, stable service/package identities and receipt contract, user-data preservation, and child work packages with runnable install/uninstall qualification. Its implementation children deliver service registration and installed receipt evidence; that evidence is required for managed-source detection.
- **#345 native service/source detection and #343 configured deployment probe:** distinguish this package-managed native daemon from Cargo/source, copied-binary, container, remote, and unknown deployments; unsupported origins must not expose package-update controls.
- **#350 native update decomposition:** follows its listed #336/#337/#89/#293 prerequisites and defines child work packages for release metadata lookup, version comparison, staged package verification, compatibility/readiness, traffic cutover, drain, and automatic route-back qualification. Its implementation children remain blocked on the required supervisor/coordinator and must verify a real signed/notarized package, stopping on missing identity or credentials. The existing Rust lifecycle test does not qualify this coordinator or package trust.
- **#340 menu-bar target:** may present update controls only after #335, #336, #337, its management API prerequisites, and #350 are accepted and qualified. Until then, update controls remain disabled.

**Acceptance required:** David/Cos accepts or revises this exact proposal. No status in issue #336 alone constitutes acceptance; #350 implementation leaves stay blocked until their coordinator and rollback behavior are implemented and qualified; the decomposition packet itself follows its listed prerequisites.

## Phase dependency conflict

The implementation plan lists native update in M8 while deferring the required supervisor, signed versioned installation and cutover/rollback to post-MVP. Existing #422–#424 are deferred behind M8 acceptance, so they cannot gate M8 implementation completion without removing that reverse dependency. The proposed correction is to qualify the minimum signed/versioned installation, coordinator and cutover producer slices before the corresponding implementation leaves from #341/#350; scheduled/automatic policy and HA remain in their later phase. The backlog owner must reconcile those producer briefs before dispatch. This proposal neither claims an implemented coordinator nor substitutes downtime/manual reinstall for the specified upgrade contract.
