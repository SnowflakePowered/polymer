# Kameo command infrastructure migration

Status: proposed implementation plan. No runtime migration is included in this document.

Prepared against the repository on 2026-09-11 and Kameo 0.22.2 documentation. Validate the pinned dependency's APIs in compiling prototypes before finalizing macro syntax. This document describes a proposed architecture; ARCHITECTURE.md remains human-maintained and is not modified.

## 1. Objective and scope

Replace Polymer's custom public command transport and request/reply implementation with Kameo while retaining generated, domain-specific client methods. Migrate Bluetooth to a native actor. Expose GUI and retained V8 worker commands through Kameo bridges, preserving execution on their owning threads.

Keep the polymer-command crate as the compatibility and generation facade. It should contain protocol generation, domain error policy, a small pending-reply abstraction, and bridge admission/lifecycle support. It should no longer implement a general-purpose actor mailbox or a parallel request/reply runtime.

Existing method calls such as manager.connect(address, options).await and library.games_list(platform).await should remain familiar. Exact types of deferred receivers and internal constructors can change with coordinated caller updates.

Include Library, UI automation, and inspector protocols in the final consumer sweep: leaving these on the old implementation would prevent removal of the shared infrastructure. Low-level Bluetooth USB/HCI/L2CAP queues and raw input broadcasts are outside the replacement scope. They implement protocol flow control and streaming, rather than the public command abstraction.

Success is a simpler shared implementation with preserved observable behavior. Do not promise a specific LOC reduction: bridge code can offset local savings in GUI/V8. Measure handwritten source changes separately from moved code, generated expansion, and tests.

## 2. Current implementation and migration targets

| Location | Current responsibility | Target |
| --- | --- | --- |
| crates/runtime/polymer-command/src/lib.rs | Sinks, response channels, declaration and dispatch macros, waiting and timeout policy | Generated Kameo clients, request policy, bridge support |
| crates/runtime/polymer-command-macros/src/lib.rs | Rewrites ordinary methods into response-sending dispatch handlers | Native Message implementations and local bridge dispatch generation |
| crates/input/polymer-bluetooth/src/manager.rs | Bounded command queue, borrowed driver view, manager loop, device lifecycle | Owned Bluetooth actor and explicit cancellable operations |
| bins/polymer-gui/src/app.rs | CommandSink closures delivering UserEvent values | Bridge construction tied to GUI lifetime |
| bins/polymer-gui/src/app/application.rs | Local command execution and pending-operation scheduling | Retained scheduling with generated local envelopes |
| bins/polymer-gui/src/app/coordinator_commands.rs | Domain handlers and pending callbacks | Local handlers with immediate/deferred Kameo completion |
| crates/runtime/polymer-pluginhost/controller/mod.rs | Blocking/deferred controller-session client and last-client shutdown | Generated Kameo client and explicit lifetime ownership |
| crates/runtime/polymer-pluginhost/runtime/controller_session.rs | Dedicated V8 thread, command loop, deadlines and cleanup | Thread-owned execution behind a bridge |
| crates/runtime/polymer-runtime/src/library/ | Simple async backend loop | Native actor pilot |
| crates/runtime/polymer-automation/src/session/mod.rs | Dedicated blocking automation actor | Thread bridge initially |
| bins/polymer-gui/src/inspector/elements/ | Inspector command protocol and proxy delivery | GUI bridge using the same infrastructure |

The existing working tree contains Bluetooth cable-pairing changes. Implementation must rebase its inventory on those changes and avoid overwriting unrelated work.

## 3. Contract inventory before implementation

Build a per-protocol table recording every method's request mode, acknowledgement point, error mapping, queue policy, shutdown behavior, and calling thread. Cover all command_client!, actor_client!, command_handler, CommandSink, CommandResponse and CommandFuture consumers, including aliases and generated constructors.

Record these existing contracts explicitly:

- Bluetooth has a 64-entry command queue, three 250 ms queue-admission attempts, and a 60-second response timeout. Connection attempts have a separate 55-second execution timeout. Preserve these initially.
- Bluetooth start currently returns a handle before adapter initialization completes; readiness/unavailability arrives through events. Preserve that contract rather than accidentally making actor startup a new synchronous readiness barrier.
- GUI commands may acknowledge acceptance before asynchronous callback completion. Preserve each method's existing meaning; do not globally redefine replies as operation completion.
- GUI save/resume-state requests and input changes have application-level serialization. Pause/resume and quit have their own pending state.
- ControllerSession has synchronous methods and deferred methods whose receivers are polled by GUI code. Submission and completion are distinct.
- ControllerSession and AutomationSession request stop on last-client drop. Actor internal references and generated per-method recipients must not defeat that behavior.
- Fire-and-forget methods currently suppress some send errors. Keep the public return contract initially, while making closure visible in tracing where appropriate.

Do not infer FIFO between racing producers or unrelated bridges. Preserve ordering within a submitted stream and the explicit domain scheduling constraints.

## 4. Generated API and crate structure

### Protocol generation

Keep protocol definitions independent of actor implementation crates. Generate one message type per method, a cloneable client, and construction from an actor implementing the complete protocol. Use typed ReplyRecipient values for request/reply methods and Recipient values for notification methods so the client does not expose the concrete actor type.

This preserves the existing dependency direction where Library is declared in polymer-deno and implemented in polymer-runtime. Prototype cross-crate generation first. Avoid blanket foreign-trait implementations that violate Rust coherence: emit implementations for the concrete local actor, or use a deliberately local wrapper where needed.

Generate the following modes:

| Declaration mode | Generated behavior |
| --- | --- |
| Async request | Construct message, admit it, await Kameo reply, map transport errors |
| Blocking request | Use verified blocking request APIs from a permitted calling thread |
| Deferred request | Submit now and return a pending reply without waiting for execution |
| Notification | Send without a reply, preserving the documented admission/error policy |

Preserve impl Into argument conversion before constructing owned messages. Preserve Send + 'static client futures where downstream APIs require them. Keep only a future alias if its type remains useful; do not retain obsolete sending implementations merely to retain names.

Deferred replies need a Polymer PendingReply<T, E> facade if Kameo's concrete pending types cannot directly satisfy all current callers. It should expose the actual needed operations: asynchronous waiting and nonblocking completion inspection, plus blocking waiting only where used. Implement it over Kameo's supported APIs. A compile spike must establish how synchronous enqueue and polling work before committing to this public shape. Do not silently turn deferred submission into a lazy future that is never polled.

### Handler generation

Provide two explicitly distinct generation paths sharing one protocol parser:

1. Native actors: generate Message<M> implementations around naturally returning methods. Ordinary handlers no longer need Output<'handler>, response parameters, or a bool dispatch result. Expose Kameo Context for handlers that delegate or stop.
2. Local execution: generate an envelope enum and a local handler trait/dispatch implementation without requiring the executor or its futures to be Send. The envelope crosses threads; its handler borrows thread-owned state only after arrival. Retain an explicit dispatch outcome where GUI scheduling needs pending callbacks.

Request envelopes carry an owned reply sender; notification envelopes do not. Do not treat absence of a reply sender as permission to skip executing a notification. Generate rejection handling that can complete a request when delivery fails or admission closes.

Use mod.rs for multi-file modules. Suggested facade modules are protocol/mod.rs, request/mod.rs, bridge/mod.rs and lifecycle.rs; keep a module single-file until its size warrants splitting. Split the procedural macro implementation into parser and generation modules. Put shared behavior in traits/impls and avoid duplicating GUI and V8 forwarding runtimes.

### Error and timeout policy

Map handler errors back to their original domain values. Separately map mailbox closure, admission timeout, reply timeout, and worker termination. Verify tell-handler error behavior so an ordinary domain rejection does not unexpectedly trigger actor failure.

Retry only admission failures proven to occur before enqueue. Never retry a reply timeout or replay an operation after an uncertain completion. The mailbox-admission and reply deadlines remain separate. For bridged calls, reply waiting includes local queue delay after bridge admission. Any new end-to-end deadline is a deliberate contract change, not an incidental implementation detail.

## 5. Shared bridge design

The data path is client -> Kameo bridge -> local envelope transport -> GUI/V8 executor -> original reply sender. Kameo Context::reply_sender delegates ownership of completion to the local executor. The bridge forwards and returns DelegatedReply rather than awaiting the local operation.

Define a small local-delivery trait implemented by the GUI proxy and worker sender. It sends an owned generated envelope and returns that envelope on delivery failure so the caller can reject it and release resources. It does not own GUI/V8 state and does not implement another reply-channel system.

### Admission and backpressure

A bounded Kameo mailbox does not bound work already forwarded into an OS event queue or a worker queue. Use a service-wide admission budget covering queued and executing local requests. Acquire capacity before submission; carry its owned permit through the message, local envelope and deferred operation. Release it on completion, rejection or teardown, not when the bridge forwards the command.

Share this budget across protocol endpoints using the same local executor where necessary. GUI emulator/input/WebRTC endpoints must not create uncontrolled independent queues. Pick and document capacities after baseline workload measurement; previously unbounded protocols gain an explicit overload policy.

Synchronous deferred submissions from the GUI must not block waiting for capacity that requires GUI progress. Use immediate admission/rejection for those paths, or restructure the caller to submit asynchronously. Verify exact semantics against current call sites before migration.

Completion notifications, cancellation and shutdown must bypass ordinary work admission. Keep these control paths narrowly scoped. Do not spawn an unbounded task per forwarded request, wait for a permit inside a handler that blocks lifecycle messages, or require a completion to acquire a permit held by its own operation.

### Lifetime and termination

Represent the service lifetime independently from the number of generated recipients. A shared public-client lease can request closure when its final owner drops; internal actor handles and worker monitoring must not hold that lease. Drop initiates best-effort shutdown; an explicit async shutdown operation waits for cleanup.

Use Running -> Closing -> Closed lifecycle state. Close admission first, then resolve queued work according to that service's contract, terminate or finish executing work, and acknowledge actual executor shutdown. Worker exit must close the public endpoint even if the bridge task still exists.

Forwarded reply senders can outlive the bridge. Ensure worker/GUI teardown rejects or drops every pending envelope and completion holder; do not assume stopping the actor completes delegated replies. Define error mapping for callers in this case.

Initially disable automatic replay/restart of GUI/V8 bridges and Bluetooth hardware ownership. A replacement worker must have a new generation identity so late completions cannot mutate a replacement session. Joining OS threads must not block the GUI or a Tokio worker thread.

## 6. GUI migration

Create GUI bridge endpoints during service wiring, passing only EventLoopProxy and shared lifecycle/admission state. ShellCoordinator, window/rendering objects, DOM runtime and local futures remain owned by the GUI.

Replace command-sink construction in app.rs and inspector server setup with generated bridge clients. Retain typed GameHost/UserEvent variants or equivalent typed envelopes because application.rs classifies operations for scheduling. Avoid opaque closures that conceal save/resume command identity.

Adapt coordinator handlers to complete immediate requests through Kameo reply senders. Preserve PendingEmulatorCallback, pending_state_commands, pending_input_commands and their completion rules. A deferred GUI operation retains its admission permit and, when its contract requires a completion reply, its reply sender until finish_* applies state.

For GUI -> V8 -> GUI flows, return to event processing after invoking the worker. Migrate existing oneshot polling to the PendingReply facade or a completion event that wakes the GUI. Do not synchronously wait for plugin execution on the GUI thread. Preserve the existing event-loop polling/wakeup cadence unless changing it is independently justified.

On GUI teardown, close admission, reject pending envelopes, finish/cancel pending callbacks according to existing policy, and notify bridge owners. Do not wait on a bridge whose shutdown itself requires a GUI event to execute. Preserve domain serialization independently of bridge mailbox order.

## 7. Retained V8 worker migration

Keep the dedicated OS thread and its local Tokio/Deno runtime. Construct and destroy the isolate on that thread. The Kameo bridge owns only the worker transport, lifecycle state, and an appropriate thread-safe cancellation handle.

Replace ControllerSessionCommand reply fields with generated local envelopes backed by Kameo. Keep the worker receive/dispatch loop and local natural handlers. Migrate blocking attach methods and deferred change/pause/state/quit methods, including all GUI receiver consumers.

Preserve the startup handshake: only return a usable retained session after initialization succeeds. Report worker startup failure to its creator and close the bridge. Avoid synchronously blocking a runtime thread on an actor that needs that same runtime to initialize.

Keep IsolateDeadline and InvocationControl behavior. Caller reply timeout does not terminate JavaScript. Execution deadlines must still drive thread-safe isolate termination where currently required. Keep termination-reset handling and filesystem/configuration cleanup on the worker thread. Do not expand timeout recovery semantics as part of transport migration.

Worker exit monitoring must distinguish normal stop, startup failure and panic/abnormal exit. Reject or drop queued envelopes and release permits. Shutdown acknowledgement occurs after runtime cleanup and thread exit, not after enqueueing Stop. Audit queued Stop versus out-of-band cancellation so busy synchronous JavaScript cannot prevent termination.

Do not move V8 into Arc<Mutex<_>>, add unsafe Send implementations, or expect a LocalSet to remove Kameo's Send actor bounds.

## 8. Bluetooth migration

Replace BluetoothServer's local state plus borrowed BluetoothDriver with an owned actor. Split manager.rs into manager/mod.rs and coherent modules for protocol, actor lifecycle, discovery, connection and session/report handling. Re-export existing public types to preserve crate consumers.

Use a 64-entry public mailbox initially and preserve queue/reply policies. Initialize adapters through the actor lifecycle while preserving event-based readiness and partial adapter availability. Keep adapter retry policy and firmware events.

Move ordinary command methods into generated native handlers. Keep long scans and connection attempts off the message loop. Use explicit completion messages where shared domain logic benefits; use pipe_with only for contained continuations with a clear cancellation owner. Kameo piped futures are not automatically cancelled on actor shutdown, so retain explicit cancellation tokens or tracked tasks.

Represent operation identity so a late scan/connect completion cannot install stale results after cancellation, disconnect or replacement. Reply after updating actor state at the same logical point as today. Retain per-transport connection exclusion, scan suspension/resumption, automatic reconnect and BLE wake coordination.

Feed HCI lifecycle events through a tracked event pump, retaining lag/error reporting. Do not await sending into the actor's own full mailbox from its handler. Keep raw report broadcasts on their existing direct path; do not put input reports behind slow control commands. Preserve session IDs, external ownership, rumble/IR commands and report-task cleanup.

Shutdown closes admission, cancels and awaits owned work as appropriate, stops discovery, closes sessions, cleans up pairing/report/wake tasks and releases adapters. Verify the existing acknowledgement point and document any intentional strengthening to mean cleanup has completed.

## 9. Remaining consumers and removal ledger

Migrate Library as the initial native-actor pilot. Migrate AutomationSession through the same thread-bridge abstraction initially, preserving thread ownership and driver shutdown. Audit non-retained plugin workers before changing anything: worker execution without polymer-command usage does not need a gratuitous bridge.

| Infrastructure | Final disposition |
| --- | --- |
| CommandResponse async/blocking enum | Remove; use Kameo completion |
| CommandSink and ActorCommandSink | Remove after all consumers migrate; retain only specific local delivery adapters |
| CommandUnavailable / ActorCommandUnavailable | Remove or replace with a minimal domain mapping policy |
| Manual request response-channel creation | Remove from generated public command code |
| Native command enum dispatch loops | Remove in migrated native actors |
| Output<'handler> for native actor dispatch | Remove |
| Protocol parsing, generated messages and client methods | Retain/rewrite |
| Local envelope enums and GUI/V8 dispatch | Retain/generate |
| Deferred receiver wrappers | Replace with PendingReply where needed |
| GUI pending operations and V8 startup/deadline machinery | Retain |
| Low-level Bluetooth protocol queues and broadcasts | Retain |

Remove paste only if the new generation no longer requires it. Do not remove Tokio dependencies that remain necessary for services. Keep Kameo local-only features minimal; distributed actors are outside scope.

## 10. Implementation sequence and gates

1. **Baseline and API spike.** Inventory contracts and source counts. Add the pinned Kameo dependency in an isolated implementation change. Compile cross-crate recipient construction, Result error mapping, blocking requests, deferred polling, delegated cross-thread replies and bounded admission. Gate: all required modes have a demonstrated implementation without unsafe thread workarounds.
2. **Shared generation and Library pilot.** Introduce Kameo generation alongside the old implementation temporarily. Migrate Library and prove call-site compatibility. Gate: real service tests pass and native handlers have no old response plumbing.
3. **Bridge core and V8 pilot.** Implement local delivery, admission leases, pending replies and worker lifecycle. Migrate the full ControllerSession protocol rather than mixing ordered methods across old and new queues. Gate: startup, deadlines, nested service calls and teardown pass.
4. **GUI and inspector.** Migrate complete protocol endpoints and pending receiver consumers. Coordinate this change with the V8 pending-reply API. Gate: GUI scheduling and responsiveness pass under load and shutdown.
5. **Bluetooth actor.** Consolidate state and migrate command handling, events and operation cancellation. Gate: deterministic transport-backed tests and available hardware smoke tests pass.
6. **Automation and consumer sweep.** Move remaining command clients to Kameo. Gate: no production consumer requires the legacy sink/response implementation.
7. **Delete compatibility code and measure.** Remove old macros/backends and obsolete tests; retain semantic regression tests. Report net handwritten LOC, dependency impact and behavioral verification. Gate: targeted and broader required checks pass, and no obsolete command infrastructure references remain.

Each phase should be a reviewable commit/change set. During coexistence, choose one transport per service instance. Never mirror side-effecting requests to both backends. If a phase fails, revert that service migration with its caller changes while retaining successful independent work; do not introduce transparent retries or replay as a rollback strategy.

## 11. Validation plan

Minimize new tests. Budget at most three focused integration tests for the migration, preferably by adapting existing tests rather than adding new ones. Do not create one test per command, error variant, macro expansion or service. Do not test Kameo's own guarantees again. Phase gates use the evidence below; they do not require additional test suites.

| Integration scenario | Migration-specific behavior to verify |
| --- | --- |
| Request admission and completion | Adapt the existing bounded-actor timeout test to verify separate admission/reply failures and that admission retries cannot execute the operation twice. Exercise the generated deferred submission path in the same fixture. |
| Local bridge under saturation and shutdown | Use one thread-owned fake executor shared by GUI/V8 bridge infrastructure. Hold work in flight, verify the outstanding-work bound, then complete and close it. Verify replies settle, permits release, execution/cleanup stay on the owning thread, and final public-client drop initiates closure despite internal references. |
| Bluetooth cancellation followed by late completion | Use a controlled transport/operation fixture to cancel a connection or scan, deliver its late completion, and verify it cannot install stale state or prevent shutdown. Reuse an existing fixture where possible. |

Use deterministic synchronization and controlled timers. Keep these scenarios focused; do not hide a large parameterized test suite behind the three-test budget. Add another test only for a concrete uncovered defect discovered during implementation, and document that reason.

Use compilation of migrated real consumers to verify message types, cross-crate coherence, Send bounds, blocking calls and handler generation. Retain and run relevant existing semantic tests for V8 deadlines, GUI scheduling and Bluetooth behavior; adapt broken tests only where they still protect a meaningful contract. Remove tests that merely verify replaced plumbing or obvious dispatch behavior. No new compile-test framework or benchmark suite is required.

Run the focused tests when their implementation changes. At the end, run an affected-package cargo check and relevant existing tests once, plus repository-required formatting/lint checks. Broaden or repeat only for a new change, failure or unresolved concern. Confirm exact package targets from current manifests.

Perform one GUI/V8 smoke sequence with polymer-gui --debug: launch, pause/resume, save/resume state, inspector request and quit; observe responsiveness and cleanup. Perform one available Bluetooth smoke sequence: discover, connect, receive reports and disconnect/shutdown. Record unavailable hardware checks explicitly. Use existing tracing to inspect queue delays if responsiveness changes; require dedicated performance measurement only if a regression is observed.

## 12. Completion criteria and open prototype questions

Complete when all public polymer-command consumers use Kameo requests, native services own their actor state, GUI/V8 remain thread-correct, old sink/response code is deleted, and the minimal validation plan is satisfied or explicitly reports environmental gaps. Keep local dispatch as an intentional execution adapter, not an unfinished legacy backend.

Resolve these implementation questions in phase 1, before broad changes:

- Exact Kameo pending-reply APIs for synchronous submission and GUI nonblocking polling.
- Cross-crate recipient and generated trait coherence for both native actors and bridge actors.
- Mapping of dropped delegated senders and domain Result errors, including tell requests.
- How actor shutdown and public-client lease closure interact with internal strong references.
- Concrete shared admission capacities and immediate-overload mapping for currently synchronous deferred APIs.

These are bounded prototype tasks. If an API requires a wrapper, keep it narrow and document why; do not recreate a custom actor runtime underneath Kameo.

## References

- [Kameo Actor bounds and lifecycle](https://docs.rs/kameo/0.22.2/kameo/actor/trait.Actor.html): actors and handler futures require Send; GUI/V8 ownership therefore stays outside ordinary actors.
- [Kameo recipient construction](https://docs.rs/kameo/latest/kameo/actor/struct.ActorRef.html#method.reply_recipient): actor type erasure for protocol clients.
- [Kameo request APIs](https://docs.rs/kameo/latest/kameo/request/struct.AskRequest.html): mailbox/reply deadlines and submission modes; verify the pinned release during the spike.
- [Kameo delegated replies and asynchronous continuations](https://docs.rs/kameo/0.22.2/kameo/message/struct.Context.html): external completion ownership and the explicit need to manage cancellation of piped work.

## Implementation status

Implemented; see [KAMEO-MIGRATION-AUDIT.md](KAMEO-MIGRATION-AUDIT.md) for contracts, policy choices, source/dependency impact, validation and the outstanding hardware smoke gap. Changes remain in the worktree for review.


## Superseding simplification

The user approved direct owner-thread mailboxes instead of Kameo forwarding actors. Local clients now only send; explicit worker owners control shutdown. Library and Bluetooth remain Kameo actors. The current design and verification are documented in KAMEO-MIGRATION-AUDIT.md; earlier all-Kameo and final-client-drop requirements in this plan are superseded.
