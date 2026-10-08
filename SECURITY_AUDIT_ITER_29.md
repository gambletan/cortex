# Security Audit — Iteration 29

Date: 2026-10-07
Scope: Muse revocation, memory deletion, disclosure accounting, and read-status integrity.

## HIGH: cloud export removal and tenant deletion raced disclosure

Signed device operations held the cloud tenant mutex, while recall/remember held a
different OS disclosure lock. A recall could snapshot shared text while a concurrent
export replacement removed it and reported success. Tenant deletion also left an
in-flight cached router holding the database.

Export replacement now takes `gateway-state.lock` before changing the export. All
tenant deletion paths, including retention cleanup, take that same lock and mark the
tenant disabled before removing its public mapping and directory. A waiting stale
request fails closed after deletion. No recursive OAuth-disconnect lock is taken.

## HIGH: OAuth replay revocation raced recall and remember

Authorization-code replay and refresh-token reuse removed grants under only the OAuth
state lock. A request could have already passed its grant check and then disclose or
append an inbox entry after replay-triggered revocation.

The token endpoint now takes the disclosure lock before its OAuth state lock. Recall
and remember retain their authorization check after acquiring the disclosure lock.
Explicit disconnect, enrollment, and replay revocation therefore share one ordering.

## HIGH: deleting an original raced sharing its copy

Sharing could read an original memory, pause during cloud settlement, then create an
export copy after deletion had already scanned for copies. Deletion's later
reconciliation could publish that new copy while reporting the original deleted.

Original lookup, copy removal, deletion, and reconciliation now run under the same
per-database Muse operation lock as sharing. Local deletions and CLI revocation also
take the disclosure lock. CLI sharing follows the same operation-lock ordering.
Reconciliation has a caller-already-locked path to avoid recursive locking.

Follow-up adversarial review found two implementation hazards, both fixed: CLI lock
inversion, and original-first deletion losing the copy-discovery linkage on failure.
Export copies are now removed before the original; a failed copy removal preserves the
original for a retry. Storage errors cannot silently become an empty copy list.

## Read status and external copies

Unreadable/corrupt audit files and export lookup errors no longer appear as successful
empty read history. Audit reads hold the disclosure lock, validate records, and return
an explicit nullable/error result. A missing active log is accepted only with no
previous request history, including history from an earlier day. Missing rotated logs
are allowed. Connection and inbox failures are not reported as zero connections/items.
Device read status uses `Cache-Control: no-store`.

Sharing previews and the guide no longer claim that disabling Muse memory guarantees
copy deletion. Revocation orders future Cortex disclosure; it cannot withdraw a response
already constructed before revocation, erase Meta's retained copies, or guarantee when
network delivery completes. Read counts cover retained audit records for currently
shared items.

## Validation

- Full required command: `cargo test --workspace --exclude cortex-python --exclude cortex-wasm`.
  Final result: **794 passed, 0 failed, 1 ignored**, exit 0.
- Nine acceptance tests were written by a context-isolated agent from design/public
  documentation and the black-box harness, without implementation diffs or unit tests.
  They cover snapshot refresh/read counts, failed calls, stale pushes after restart,
  tenant deletion persistence, audit failure, and disclosure fences for export removal,
  tenant deletion, code replay, and refresh reuse. All nine pass.
- Implementer unit tests cover operation-lock ordering, failed export deletion and retry,
  and missing/corrupt audit history. Existing integration suites also pass.
- Independent adversarial review found no remaining HIGH/CRITICAL issue in the final diff.
- `cargo check -p cortex-mcp-server --no-default-features` passes.
- `git diff --check` and `bash -n deploy/cortex-cloud/deploy.sh` pass.

The ignored test is the live Google Drive encrypted-sync test. It previously wrote to a
fixed folder in any installed Drive and failed here with a mount timeout. It is now an
explicit `--ignored` opt-in using a unique owned temporary folder; selecting it still
fails on a broken mount. Local encrypted-sync coverage remains in the normal suite.
This audit does not claim live Google Drive validation.

Codex CLI second-opinion initialization failed with `Operation not permitted`. The
advisor's independent assessment endorsed the shared-lock approach, warned against
recursive disconnect locking, and recommended honest unknown status and isolated live
cloud tests. The independent code review and isolated acceptance checks were completed.

## New hostname preparation

The user supplied `cortex.alvinsclub.ai`. DNS resolves, but ordinary verified HTTPS to
`/healthz` fails with curl error 60: the certificate does not cover this hostname.
A dedicated nginx site example and rollout instructions are prepared. Compiled defaults
and the running server have not changed. Release needs matching TLS coverage, routing
verification, and deliberate OAuth issuer migration/reconnection of existing clients.
