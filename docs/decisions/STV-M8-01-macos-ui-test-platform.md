# Proposal: STV-M8-01 macOS UI and UI automation platform

**Issue:** [#335](https://github.com/djh00t/steve/issues/335)
**Status:** Proposed; pending David/Cos review. This note does not record acceptance.
**Intended accepted artifact:** `docs/decisions/STV-M8-01-macos-ui-test-platform.md`

## Decision question

Which native macOS UI framework, UI automation surface, deployment target, app source root, and future build/test command should M8 use while keeping the controller optional to the Steve daemon?

## Proposed decision

- **UI:** SwiftUI `MenuBarExtra`, using its window presentation for the status and profile controls. It is a first-party menu-bar scene for a persistent macOS menu-bar control; no third-party UI framework or custom AppKit status-item wrapper is needed.
- **Automation:** Xcode UI Testing with XCTest and XCUIAutomation. Keep one end-to-end UI smoke test for the disconnected state; use menu-bar status-item accessibility queries to open the menu and assert the visible state.
- **Minimum OS:** macOS 13 (Ventura), the release generation that introduced SwiftUI `MenuBarExtra`.
- **Source root/layout:** `macos/SteveApp/`, with `SteveApp.xcodeproj`, `Sources/SteveApp/`, and `Tests/SteveAppUITests/`. Check in a shared `SteveApp` scheme with the UI test target enabled.
- **Future command:** `xcodebuild test -project macos/SteveApp/SteveApp.xcodeproj -scheme SteveApp -destination 'platform=macOS'`
- **Daemon boundary:** Keep the Swift app as a separate optional control surface. It is outside the Rust crate and is not a dependency of `steve`, `make check`, daemon startup, or daemon operation. The existing macOS CI job continues to validate Rust; a macOS/Xcode job can run the app command after the project and scheme exist.

## Acceptance case

Given Steve is not installed or running, when the UI smoke test launches the controller and opens its accessible menu-bar status item, then the controller presents a clear disconnected state without starting or requiring the daemon. The test must locate the status item through XCUIAutomation and assert the visible state, rather than only asserting that the app process launched.

This qualifies the UI platform and the first consumer (#340); later UI slices can add fixture-backed management API workflows once their endpoint contracts are accepted.

## Evidence

- The M8 plan calls for a Swift macOS controller covering onboarding, deployments, status, usage, spend, and profile changes: [`docs/plans/2026-09-26-steve-mvp.md`](../plans/2026-09-26-steve-mvp.md#L199).
- The gateway specification states the Swift app is optional and is a control surface, not a required runtime: [`docs/specs/2026-09-26-steve-gateway.md`](../specs/2026-09-26-steve-gateway.md#L27).
- Current CI runs Rust `make check` on `macos-latest`; it has no Xcode project or Swift test command yet: [`.github/workflows/ci.yml`](../../.github/workflows/ci.yml#L20), [`Cargo.toml`](../../Cargo.toml).
- Apple documents `MenuBarExtra` as a persistent system menu-bar control and introduced the scene in the WWDC22 SwiftUI session (macOS Ventura): [MenuBarExtra](https://developer.apple.com/documentation/swiftui/menubarextra), [WWDC22: Bring multiple windows to your SwiftUI app](https://developer.apple.com/videos/play/wwdc2022/10061/).
- Apple recommends XCTest/XCUIAutomation for UI workflows and exposes a status-item query for UI automation: [Adding tests to your Xcode project](https://developer.apple.com/documentation/xcode/adding-tests-to-your-xcode-project), [XCUIAutomation `statusItems`](https://developer.apple.com/documentation/xcuiautomation/xcuielementtypequeryprovider/statusitems), [Running tests](https://developer.apple.com/documentation/xcode/running-tests-and-interpreting-results).

## Command status and qualification boundary

The command above is the exact proposed future check, not a currently runnable or verified command: this checkout contains no `macos/SteveApp/` project, shared scheme, or UI test target. No app build or UI automation result is claimed. Direct consumer #340 introduces those assets and runs the command on its candidate branch; later consumers use passing evidence from #340’s delivered revision. The current Rust workflow does not qualify Swift/Xcode behavior.

## Consumers still blocked

- Direct consumer [STV-M8-29 (#340)](https://github.com/djh00t/steve/issues/340), the menu-bar target with disconnected state, also depends on #72. It may create the project, scheme, and UI smoke test, then qualify them with the proposed command after David/Cos accepts a specific revision of this decision and #72 is satisfied.
- Other M8 UI work that cites #335, including #353–#357, #359, and #378 in [`docs/backlog-index.md`](../backlog-index.md#L421), remains blocked pending the accepted revision, the runnable command qualified by #340, and its own management API/control-plane prerequisites. This proposal does not qualify API behavior, installation/distribution (#336), discovery/action policy (#337), credential storage, or remote forwarding.
