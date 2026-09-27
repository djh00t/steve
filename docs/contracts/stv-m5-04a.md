# STV-M5-04A (#545): OTLP signals and configuration boundary

**Status:** Proposal only; David/Cos acceptance pending. No exporter implementation or config path is present; this draft makes no runtime claim.

| Decision | Proposed contract |
|---|---|
| MVP signals | Support traces, metrics, and structured logs, retaining all three required by [spec §13](../specs/2026-09-26-steve-gateway.md). |
| Default/enablement | All signals are disabled by default. Configure static daemon settings with independent trace, metric, and log switches; enabling one never enables another. SDK/environment defaults must not implicitly enable outbound export. |
| Minimal logical inputs | Static settings provide independent enabled-signal switches and one absolute HTTP(S) OTLP destination URI when any signal is enabled. These are decision concepts, not committed field names or schema; #319 owns exact persisted fields, routes, secrets, and restart/reload schema. |
| Misconfiguration | All switches off with no destination is valid/no export. Any enabled signal without a destination, malformed destination, or unsupported signal setting is a startup configuration error; never silently disable a requested signal. A configured destination with all switches off is inert. |
| Adapter boundary | Producers submit normalized signal records to a background adapter, which routes only enabled signals to the configured destination. Current queue is generic `kind + JSON payload`, not a signal adapter. No exporter call occurs on the request path; outcome, timeout, retry, and loss semantics belong to #547. |

Do not accept arbitrary URI schemes or inline credentials, query strings, or fragments. Exact transport/encoding and signal endpoint mapping require qualification by #185/#319; an absolute URI alone is not proof of compatibility. Records must satisfy accepted #546 privacy and size bounds. No SDK environment variable may override the disabled default. Per-signal privacy/allowlists belong to #546.

**Acceptance examples:** With all switches off and no destination, startup succeeds, no export is started, and built-in reporting/local logs remain available. Enabling only metrics sends no traces or logs; enabling traces and logs with metrics off leaves metrics disabled. Any enabled switch without a valid destination fails startup clearly.

**Evidence:** Read-only review of live #545 and consumers #185/#206/#256/#319; inspected base `4472ca59896465fcf27b0d1df1d5218552d80efd`; [spec §§13/17](../specs/2026-09-26-steve-gateway.md); [`src/config.rs`](../../src/config.rs) has no OTLP inputs, [`src/deferred.rs`](../../src/deferred.rs) has a bounded generic telemetry queue/worker but no exporter, and [`src/server.rs`](../../src/server.rs) has no exporter lifecycle. Consumers #185/#206/#256/#319 remain blocked pending proposal acceptance and runtime/config qualification. #177 coordinates; #545 owns signals/default/static-config boundary; #319, #546, and #547 own their stated boundaries. No SDK version, dependency, collector, backend, dashboard, or SLO is selected. Documentation-only review; runtime tests are N/A.
