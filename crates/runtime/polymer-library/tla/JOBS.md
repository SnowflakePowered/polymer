# Library job verification (T8)

The model covers imports sharing one `(kind, dedupe path)` key. It assumes one live JobStore per database, as used by the application. It checks runner exclusivity as well as row uniqueness: marking a live import cancelled must not free its dedupe key.

## Implementation mapping

| Model action/state | Rust operation |
|---|---|
| `Submit`, `pending` | `JobStore::submit`: serialized enqueue and runner dispatch; tasks wait to claim. |
| `CancelCheck`, `CancelModify` | A cancellation request followed by eligibility checking and mutation together inside `modify`, under the write lock or ephemeral entry guard. The split conservatively allows requests to become stale before the guarded mutation. |
| `RetryCheck`, `RetryModify` | A retry request followed by `requeue` checking current state and dedupe ownership under `writes`. Only a successful requeue dispatches this job again. |
| `Claim`, `working` | Conditional queued-to-running update and an import executing with its JobHandle. |
| `Finish` | Serialized handle settlement, requiring Running; the handle is marked finished after the database write succeeds. |
| `FinishFails`, `cleanup` | Settlement write fails; the runner drops its unfinished handle and schedules interruption cleanup. |
| `Cleanup`, `CleanupFails` | Drop cleanup either marks Running as Failed or fails to persist that transition. |
| `RecoverUpdate` | `JobStore::open` resets interrupted running jobs and steps before returning the shared store. |
| `RecoverList` | `resume` lists queued jobs and dispatches them, without resetting any state. New requests can overlap this phase. |
| `Crash` | Process death discards requests, runners and cleanup tasks; durable rows remain. |

Cancellation can only change Queued or Paused jobs. If claim wins first, cancellation returns InvalidState and the job retains its dedupe reservation until settlement. Retry eligibility and dedupe checks are serialized with enqueue, claim and cancellation. Dismiss also checks eligibility under the write lock, preventing a concurrent retry from being deleted.

Imports do not produce Paused or Partial, so those states are omitted. Progress, child installation jobs, ephemeral jobs, dismissal and download abandonment are outside the model. Work effects inside an import are abstracted; this proves neither rollback nor exactly-once import effects across crashes. Opening a second store for the same live database is outside the ownership assumption.

## TLC results - 2026-10-10

Both configurations use two jobs, two clients, at most three requests and one crash. Weak fairness applies to recovery, claim, finish, cancellation, retry and cleanup.

| Configuration | Result | Distinct states |
|---|---|---:|
| `MCJobStore.cfg`, at most one write failure | All invariants and temporal properties pass | 2,516 |
| `MCJobStoreWriteFailures.cfg`, at most two write failures | Safety passes; `NoPermanentlyPending` fails as expected | 3,411 |

The second configuration retains a known liveness limit: submit -> claim -> settlement write fails -> Drop cleanup write fails -> Running persists without a runner. Startup recovery resolves that row on restart. Drop cleanup is best effort, not an indefinite persistence retry mechanism.

The passing checks are `TypeOK`, `AtMostOneActivePerGame`, `AtMostOneRunner`, `TerminalOnce`, `TerminalOnlyLeftByRequeue`, `CancelSticky`, and (within the one-failure bound) `NoPermanentlyPending`.

Run with the repository's TLC launcher, passing `MCJobStore.tla` as the spec and the desired configuration. `MCJobStoreWriteFailures.cfg` deliberately reports a temporal-property violation.

## Rust validation

- `cargo check -p polymer-library -p polymer-runtime`
- `cargo test -p polymer-library --test job_lifecycle`

The public-API integration test exercises both cancellation/claim orderings for persistent and ephemeral jobs, dedupe reservation while work is running, competing settlement calls, and a replacement job after settlement or cancellation. It uses a real temporary database without mocks.
