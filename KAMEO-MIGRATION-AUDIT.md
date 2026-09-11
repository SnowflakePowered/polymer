# Native Kameo actors and direct thread mailboxes

The final design uses **Kameo 0.22.2 for Library and Bluetooth**, and direct owner-thread mailboxes for GUI, inspector, controller-plugin and automation work. This supersedes the original plan's requirement that all commands pass through Kameo; the user approved removing the forwarding actors.

## Local execution

A local request takes one path: typed client -> owner-thread queue -> handler. LocalDelivery checks admission and sends the generated command enum directly. There is no forwarding actor, second actor mailbox, per-method local payload struct, LocalRequest trait, public-client lease, internal request endpoint or dedicated bridge runtime.

PendingReply is an alias for Tokio's oneshot Receiver<Result<T, E>>. Async code awaits it; GUI code uses try_recv; the few blocking attachment methods use futures::executor::block_on, which also works from a current-thread V8 runtime. Completion owns the sender and outstanding-work permit. Sending a reply releases the permit before waking the receiver. Dropping the receiver does not release work capacity early.

One Admission budget (64) is shared by all endpoints of a local executor; Emulator/Input/WebRtc share the GUI budget. The transport queue can be unbounded because admission bounds outstanding public work, including work already removed from that queue. Stop/Close bypass admission so teardown remains possible under saturation. Full/closed local admission maps to the existing endpoint unavailable error; rejected notifications are traced.

ControllerSession and AutomationSession are explicit, non-Clone WorkerOwner values. ControllerClient and AutomationClient are clonable send handles; obtain them with owner.client(). Game presentation owns the workers, and V8 stores only an automation client. Worker-owner Drop closes admission, invokes cancellation and sends Stop even if clients or pending futures still exist. Dropping a sending client has no lifecycle side effect.

WorkerOwner.shutdown requests closure and awaits local cleanup. ExecutorLifetime.run executes inside the existing worker thread, records completion after the worker closure and its captured local state have been dropped, and records failure on panic. No background join runtime is required; completion denotes cleanup, not a join of the OS thread's final thread-local destructors. Controller cancellation retains the existing V8 termination hook. Receivers discard queued work after closure. GUI lifecycle remains owned by ShellCoordinator.

## Calling contracts

| Endpoint / methods | Submission and acknowledgement |
| --- | --- |
| Library: games_list/get/delete, launchables_add/delete, metadata_add/update/delete/replace/text/url, scraping_jobs/start/job/retry/scrapers/refresh | Async Kameo request; native handler result |
| Bluetooth: scan, connect | Async Kameo request; delegated operation completion |
| Bluetooth: stop_scan, send_raw, set_external_ownership, set_player_index, set_rumble, set_ir_camera, sleep, disconnect, forget | Async Kameo request; handler result |
| Bluetooth: shutdown | Async control request; cleanup before reply |
| Emulator: launch, pause_state, save_state, resume_state, state_screenshot | Async local request; existing GUI/plugin completion point |
| Emulator: close, set_volume, set_muted, message, input | Notification; no acknowledgement |
| Input: configure | Notification; admission follows pending configuration |
| Input: controller_api_response | Eager deferred local request |
| WebRtc: start, stop; Inspector: dispatch | Async local request; handler result |
| ControllerClient: attach_automation, attach_action_sink | Blocking local request |
| ControllerClient: change, pause_resume_with_timeout, dispatch_state_action, quit_with_timeout | Eager deferred local request; plugin callback result |
| AutomationClient: execute | Async local request; driver result |
| WorkerOwner: close / shutdown | Request closure / await worker cleanup |

## Native actors and dependencies

Library uses a protocol-local native actor wrapper for cross-crate coherence and Kameo's default mailbox/send behavior. Bluetooth owns adapters, sessions and operation tasks. Completion IDs prevent cancelled scans/connections/wakes from installing stale state; cleanup cancels tracked tasks, stops discovery and releases devices. Raw reports remain broadcasts.

Native Bluetooth requests retain 64-permit public admission, released when handling starts. Its unbounded Kameo mailbox permits internal completion/shutdown traffic to bypass public capacity. Three 250 ms admission attempts, a separate 60 s reply timeout and 55 s operation deadlines remain. Accepted operations are never replayed. Native requests await Kameo directly.

Kameo default/distributed features remain disabled, with tracing enabled. Cargo.lock adds Kameo and downcast-rs 2.0.2; other dependencies already existed. The command crate uses futures only for blocking attachment waits and Tokio sync/time/rt; its explicit multithread runtime requirement has been removed. Paste remains used by declaration generation.

## Source impact

Physical Rust lines include comments, blanks and tests, exclude generated expansions, and include all new modules. Counts compare HEAD with the current changed-file set (including files additionally touched by ownership changes).

| Area | Before | After | Net |
| --- | ---: | ---: | ---: |
| Command runtime/macros/tests | 1,091 | 1,462 | +371 |
| Bluetooth manager | 1,847 | 2,261 | +414 |
| Changed consumers | 9,796 | 9,821 | +25 |
| Total | 12,734 | 13,544 | +810 |

The direct-mailbox change removes 196 command infrastructure lines versus the preceding version (1,658 -> 1,462), plus consumer cleanup. Overall migration source size still grows because of native generation and explicit Bluetooth cancellation handling. No pre-migration empirical performance baseline was collected.

## Verification

After direct-mailbox conversion:

- Full GUI cargo check passed.
- Both existing command scenarios passed: admission/reply timeouts without replay; local saturation with receiver cancellation and owner-drop cleanup while a sending client remains alive.
- All 16 existing pluginhost runtime tests passed, including retained configurations, automation attachment and V8 cancellation.
- Affected GUI/command/pluginhost/automation all-target Clippy passed with repository warnings.
- No new tests were added. The migration retains three focused scenarios overall, including the previously passing Bluetooth cancelled-scan fixture.

Before this conversion, the full Bluetooth suite (41), runtime library tests (2), GUI shell-document tests (15), and the complete GUI smoke sequence passed. The physical Bluetooth discovery/connect/report/disconnect sequence remains unverified: the earlier UI reported OS BLE available, WinUSB unknown and no connected controllers.

The rebuilt direct-mailbox GUI also passed launch -> pause/resume -> save slot 0/restore -> native Elements DOM.getDocument -> quit, followed by normal window closure. The first pause attempt correctly returned a domain error while Dolphin had no registered active surface; after foregrounding the GUI, pause/resume succeeded. This smoke does not establish a performance baseline. Full cargo build and diff whitespace checks passed.
