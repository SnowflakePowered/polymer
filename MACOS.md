# Polymer on macOS

A feasibility assessment for porting Polymer's graphics and input hooks to macOS. This document
assumes familiarity with [ARCHITECTURE.md](ARCHITECTURE.md) and is organised along the same
subsystem boundaries.

The short version: the **Graphics IPC** subsystem ports well, and in places macOS offers a cleaner
primitive than the Windows original — a single Metal-level hook captures Metal and Vulkan guests
alike via a virtual swapchain that removes the copy entirely. The **Input IPC** subsystem splits: the SDL listener
design survives and its dynapi slot constants transfer unchanged, though the *installation* path has
to be rebuilt, while `polymer-bluetooth`'s BR/EDR transport does not port at all.

**Target: macOS 27, Apple Silicon only.** No fallback paths — not for earlier releases, not for
Intel, and not for guests that cannot be injected. Nothing below hedges on version availability:
FSKit, CoreAudio process taps and the SkyLight Space API are all used unconditionally, and the
single Metal-level capture path is the only one.

Claims below are marked **[verified]** where they were checked against the macOS SDK or tested on an
Apple Silicon host (M4, macOS 27), **[validated]** where they were exercised end to end against a
real guest, and **[unverified]** where they rest on reasoning that still needs an experiment.

The **[validated]** claims come from two experiments: a hook probe run against Dolphin 2606a booting
Wii Sports on an M4, and a cross-process frame handshake between a producer and consumer pair. Both
live in `experiments/macos-hook/` on the `worktree-macos-hook-validation` branch; `FINDINGS.md`
there has the detail behind each summary below.

## Process injection

Everything downstream depends on getting a dylib into the guest process.

`bins/polymer-injector` — `OpenProcess` + `VirtualAllocEx` + `CreateRemoteThread` — has no viable
macOS analogue. The equivalent is `task_for_pid` plus `mach_vm_write` and `thread_create_running`,
which against a hardened-runtime target requires the injector to run as root *and* carry
`com.apple.security.cs.debugger`. That is a permanent fight with the platform and should not be
attempted.

The supported mechanism is launch-time insertion via `DYLD_INSERT_LIBRARIES`. This is blocked by two
independent protections:

* **Hardened runtime** (`flags=0x10000(runtime)`) causes dyld to ignore the variable entirely.
* **Library validation** rejects any dylib not signed by the target's own Team ID.

Both are cleared by re-signing the bundle. **[verified]** — measured end to end on an M4 host:

```
=== HARDENED guest + DYLD_INSERT_LIBRARIES ===
guest running                          # constructor never ran; dylib silently ignored

=== ADHOC-RESIGNED guest + DYLD_INSERT_LIBRARIES ===
[polymer-hook] injected into pid 9094  # constructor ran
guest running
```

### Re-sign correctly, or you break the guest

The naive form of this — `codesign -f -s -`, dropping the signature to `flags=0x2(adhoc)` — works
but is the wrong move, because **it silently discards the guest's entitlements**. Dolphin ships
`com.apple.security.cs.allow-jit`, and on Apple Silicon `MAP_JIT` requires that entitlement; strip it
and the recompiler breaks. **[validated]**

The correct operation preserves the original entitlement set, adds
`com.apple.security.cs.allow-dyld-environment-variables`, and **leaves hardened runtime in place**:

```
codesign -d --entitlements ent.plist --xml <app>     # round-trip the guest's own entitlements
# add com.apple.security.cs.allow-dyld-environment-variables
codesign -f -s - --options runtime --entitlements ent.plist <app>
```

This yields `flags=0x10002(adhoc,runtime)` and loads inserted dylibs. **[validated]** — hardened
runtime does not need stripping at all; adding the dyld entitlement is sufficient and is the smaller,
safer change.

Dolphin's stock entitlements also include `com.apple.security.cs.disable-library-validation`, as does
OpenEmu's, so for those guests library validation is a non-issue even before re-signing.
**[verified]**

### The re-signer is a real component

Re-signing has to happen **locally, on the user's machine, per emulator install**. It cannot be done
ahead of time: the signature covers the specific bytes on disk, Polymer cannot redistribute a
modified emulator bundle, and on Apple Silicon every binary must carry at least an ad-hoc signature
to run at all — so touching the bundle forces a re-sign rather than merely permitting one.

The good news is that nothing needs to be installed for this. **`codesign` ships with macOS**; no
Xcode, no Command Line Tools, no developer account, no certificate. **[verified]** `/usr/bin/codesign`
is a real 594KB system binary — the CLT stub shims are all exactly 118640 bytes and contain
`xcode-select` strings, which codesign does not — and `xcrun -f codesign` resolves to `/usr/bin/`
rather than into Xcode. Ad-hoc signing (`-s -`) needs no identity.

The validated pipeline, in this order: **[validated]**

```
cp -Rc <emulator>.app <staged>.app          # APFS clone: copy-on-write
xattr -dr com.apple.quarantine <staged>.app # MUST precede first launch
codesign -d --entitlements ent.plist --xml <staged>.app
# add allow-dyld-environment-variables and disable-library-validation
codesign -f -s - --options runtime --entitlements ent.plist <staged>.app
```

Four things this pipeline gets right, each of which was a failure when done wrong:

* **The copy is nearly free.** `cp -Rc` uses APFS cloning: the 126MB Dolphin bundle cloned in
  **0.357s**, copy-on-write, so only the re-written main executable consumes new space.
  **[validated]**
* **Quarantine must be stripped before the first launch, not after.** A browser-downloaded bundle
  carries `com.apple.quarantine`, which survives re-signing. An ad-hoc bundle that still has it is
  `SIGKILL`ed on exec — and clearing the attribute afterwards does not revive *that copy*; the
  rejection sticks until the bundle is recreated. Stripping first, the same pipeline injects
  cleanly. **[validated]**
* **Entitlements must be round-tripped, and `disable-library-validation` added defensively.** After
  ad-hoc signing the main binary has no Team ID, so a guest's vendor-signed nested frameworks would
  fail library validation unless that entitlement is present. Dolphin already ships it; other guests
  may not. (See the preceding section on `allow-jit` — dropping the original entitlements breaks the
  recompiler.)
* **`--deep` is not needed.** Re-signing only the main executable leaves the bundle passing
  `codesign --verify --deep --strict`. **[validated]**

**The guest must be launched by direct exec, never through LaunchServices.** `spctl -a -t exec`
rejects the re-signed bundle, so `open`-style launches would be refused; direct execution of
`Contents/MacOS/<binary>` is not subject to that assessment. **[validated]** This also sidesteps App
Translocation, which would relocate a quarantined bundle to a random read-only path. Polymer must
exec the binary directly anyway, since `DYLD_INSERT_LIBRARIES` has to be in the process environment.

### Consequences

The launch model on macOS is: Polymer maintains a re-signed *copy* of the emulator bundle and
launches that with `DYLD_INSERT_LIBRARIES` pointing at the staged hooks. Since Polymer already owns
guest launch, this is not a significant departure — but it does mean Polymer must manage bundle
copies, and:

* Re-signing changes the guest's code identity, so any TCC grants that bundle had accumulated
  (Input Monitoring, Bluetooth, Screen Recording) are reset and must be re-granted.
* Injected code runs under the *guest's* TCC identity, not Polymer's. A hook that wants to seize HID
  devices needs the guest bundle to hold that grant.
* The staged copy must be rebuilt and re-signed whenever the emulator updates, so Polymer needs to
  track the upstream bundle's identity (cdhash or content hash) to know when it has gone stale.
* Disk cost is bounded by APFS cloning, but only while the original remains on the same volume and
  unmodified.

## Graphics IPC

macOS provides direct equivalents for every primitive the Windows producers rely on, and the
frame-sharing story is materially simpler because the platform's native surface type is already
shareable.

| Windows | macOS | Status |
| --- | --- | --- |
| `CreateSharedHandle` / `OpenSharedHandle` | `IOSurfaceCreateXPCObject` / `IOSurfaceLookupFromXPCObject` | **[validated]** |
| Shared `ID3D12Fence` | `MTLSharedEvent` → `newSharedEventHandle` | **[validated]** |
| D3D11/D3D12 ↔ Vulkan interop | Not required — see below | **[verified]** |
| `DuplicateHandle` for the shared-memory ring | `SCM_RIGHTS` fd passing over a unix socket | **[unverified]** |

The shared-fence model in `polymer-producer-d3d12` and `polymer-producer-d3d11` maps onto
`MTLSharedEvent` without loss. The complete producer→consumer handshake — render into a shared
IOSurface, `encodeSignalEvent:value:`, consumer blocks in `waitUntilSignaledValue:`, reads the
pixels — was run end to end across two processes and passes: **[validated]**

```
[client] IOSurface received over NSXPC: 256x256
[client] MTLSharedEventHandle received over NSXPC
[client] blocking on waitUntilSignaledValue:7 (current 0)...
[client] wait SATISFIED after 609 ms, signaledValue=7
[client] pixel[0] BGRA = 40 80 bf ff  (expect ~40 80 bf ff)
[client] producer's GPU render visible in consumer: YES
```

The 609 ms wait against a deliberate 500 ms producer delay confirms the consumer genuinely blocked
on the GPU timeline rather than returning early, and the exact pixel match confirms it read the
producer's rendered output from shared memory.

### The sync object forces NSXPCConnection

One constraint came out of this that shapes the IPC layer. **`MTLSharedEventHandle` cannot be moved
over a byte channel.** `NSKeyedArchiver` refuses to encode it with secure coding both on and off,
failing with *"The data couldn't be written because it isn't in the correct format."* — it carries a
Mach port right that only an NSXPC coder knows how to transfer. **[validated]**

`MTLEvent.h`'s remark that these handles "may be passed between processes via XPC connections" turns
out to mean `NSXPCConnection` specifically — not raw libxpc dictionaries, and not a serialised blob.

The consequence for the port: the frame-handshake channel must be an `NSXPCConnection`, which is
Objective-C and Foundation-based. It cannot be a Rust-native socket protocol the way
`polymer-ipc`'s `LocalSocket` is on Windows.

The surface itself carries no such restriction — `IOSurfaceCreateXPCObject` works fine over plain
libxpc **[validated]** — so only the event handle forces NSXPC.

### objc2 covers all of this except the protocol declaration

The obvious follow-up is whether [objc2](https://github.com/madsmtm/objc2) removes the need for an
Objective-C source file. It nearly does, and the bindings are not the problem: `objc2-foundation`
0.3.2 generates the full `NSXPCConnection` / `NSXPCListener` / `NSXPCInterface` surface,
`objc2-metal` has `MTLSharedEventHandle` and `waitUntilSignaledValue:timeoutMS:`,
`objc2-io-surface` has the ObjC `IOSurface` class, and objc2 0.6.4 is already in the tree via winit.

Two NSXPC requirements initially defeat pure Rust. One is solvable; one is not. **[validated]**

**The reply block — solvable.** A default `block2` block is rejected: *"Block was not compiled using
a compiler that inserts type information about arguments."* The cause is precise — `RcBlock::new`
sets no `BLOCK_HAS_SIGNATURE` at all, where clang emits
`v24@?0@"IOSurface"8@"MTLSharedEventHandle"16` with the class names embedded. block2 exposes
`RcBlock::with_encoding::<E>` (`E: ManualBlockEncoding`) to supply the raw string; handing it clang's
exact signature makes NSXPC accept the Rust block. Note the class names are required, not bare `@`;
the practical way to obtain the string is to ask clang for it once.

**The protocol — not solvable today.** `NSXPCInterface` rejects any protocol assembled at runtime
through `objc_allocateProtocol` / `protocol_addMethodDescription`, which is all `ProtocolBuilder` can
do: *"Unable to get extended method signature from Protocol data. Use of clang is required for
NSXPCInterface."* This is a known upstream limitation, not a local gap — objc2 issue
[#820](https://github.com/madsmtm/objc2/issues/820) is someone hitting this same error against
NSXPCInterface, and the maintainer's answer is that emitting protocol metadata as a special static
"requires further compiler support", tracking
[rust-lang/rust#145496](https://github.com/rust-lang/rust/issues/145496) (`darwin_objc`). The
documented workaround is to use clang. Poking `protocol_t`'s `_extendedMethodTypes` after
registration would mean reimplementing private runtime ABI and is not worth it.

**So the Objective-C surface is the protocol declaration and nothing else:**

```objc
@protocol PolymerXchg <NSObject>
- (void)fetchWithReply:(void (^)(IOSurface *surface, MTLSharedEventHandle *event))reply;
@end
```

plus a one-line accessor returning `@protocol(PolymerXchg)`. Building the `NSXPCInterface`,
whitelisting reply classes, opening the connection, the reply block, and all Metal and IOSurface work
are Rust through objc2, validated end to end against the same test the Objective-C client
passes. **[validated]**

If even that is unwanted, the alternative is to drop `MTLSharedEvent` and sync CPU-side — a
command-buffer completion handler signalling through the shared-memory ring `polymer-ipc` already
has. That removes Objective-C entirely and keeps the IPC layer Rust-native, at the cost of a CPU
round trip per frame. **[unverified]**

Two implementation notes. `objc2::exception::catch` does recover an NSXPC exception, but the process
is then `SIGKILL`ed by a Foundation trap on the next NSXPC call, so failing fast is the only viable
response. And the toll-free bridge between ObjC `IOSurface` and CF `IOSurfaceRef` is still `TODO` in
objc2-io-surface 0.3.2, so that pointer cast must be written by hand.

### Never use global surfaces

`IOSurfaceIsGlobal` still functions despite deprecation in 10.11, and makes a surface retrievable by
any process holding nothing but its integer ID. Non-global surfaces are properly protected:
`IOSurfaceLookup` on their ID fails. **[validated]** The XPC path costs nothing extra and does not
expose frame buffers system-wide, so the global flag must not be used.

### Service registration

An XPC listener binds silently but is unreachable unless its service name is registered with launchd
— clients get `Connection invalid`. **[validated]** Polymer's GUI would register a real mach service
and the guest-side hook would connect to it by name, which is a cleaner bootstrap than trying to pass
an endpoint into an injected dylib.

The significant simplification is that a `CAMetalDrawable`'s texture is *already* IOSurface-backed.
`MTLTexture.iosurface` returns the backing surface, and
`-[MTLDevice newTextureWithDescriptor:iosurface:plane:]` wraps it on the consumer side — zero-copy in
both directions, with no interop layer. **[verified]** in SDK. Consequently
`crates/producers/polymer-producer-vulkan/src/d3d12_interop.rs` and the whole of
`polymer-producer-opengl/src/interop/memory_object_win32.rs` have no macOS counterpart; the work they
do is unnecessary rather than merely different.

A producer would still want its own ring of IOSurfaces with a blit into them, since CoreAnimation
recycles drawables and handing the live drawable to the consumer will tear.

### Virtual swapchain, not a present hook

The `swapchain` branch replaces present-hooking on Windows with swapchain *virtualisation*: the guest
receives a fully synthetic `IDXGISwapChain3` whose `GetBuffer` returns producer-owned textures and
whose `Present` never reaches real DXGI, so the guest renders straight into Polymer's shared
textures. **macOS supports the same model, and it is the approach this port should take.**
**[validated]**

`CAMetalDrawable` is a *protocol*, not a concrete class, so a synthetic drawable is the structural
equivalent of implementing the `IDXGISwapChain3` vtable. Swizzling `-[CAMetalLayer nextDrawable]` to
vend a ring of producer-owned, IOSurface-backed drawables means the guest never receives a
CoreAnimation drawable at all:

```
[vswap] built ring of 3 virtual drawables (1280x960, fmt=80)
[vswap] guest never receives a CoreAnimation drawable
[vswap] nextDrawable=1420  present=1420
[vswap] surface id=219 1280x960  centre BGRA=18 17 14 ff  lit=743/1024 (72%)
[vswap] RESULT: PASS -- 2/3 surfaces carry substantive guest content
```

This is strictly better than hooking presentation, which must blit out of a CoreAnimation-owned
drawable because CoreAnimation recycles them. With virtualisation there is no copy anywhere, and the
producer-owned IOSurface ring *is* the swapchain.

Three implementation requirements came out of the experiment:

* **Metal sends private selectors.** `-[MTLCommandBuffer presentDrawable:]` calls
  `-addPresentScheduledHandler:`, which is not in the public `MTLDrawable` protocol; a drawable
  lacking it dies with `unrecognized selector`. That was the only extra selector for these guests,
  but it is private surface and may change between OS releases.
* **Presentation bookkeeping must be settled, not absorbed.** Swallowing that handler without
  invoking its block leaves every command buffer pending — observed as 60 `nextDrawable` calls but
  only the first frame ever reaching a surface. The drawable must run the scheduled and presented
  blocks when it presents.
* **Guests present by three different routes**, all of which must work: `presentDrawable:` →
  `addPresentScheduledHandler:`, `-present` called directly (Dolphin's Metal backend), and
  `addPresentedHandler:` (MoltenVK).

### Backend coverage — a withdrawn claim

An earlier version of this document claimed one Metal hook covered OpenGL as well as Vulkan, citing
an "OGL" run with 1243 drawables. **That was a misattribution and is withdrawn.** The backend had
been written to `GFX.ini`, which Dolphin ignores — the setting lives in `Dolphin.ini [Core]
GFXBackend` — so the run actually exercised the default backend, and the loaded-image list was
sampled 3s after launch, too early to see a lazily-loaded backend.

Corrected, with the backend read back from the config Dolphin wrote: **[validated]**

| Backend | Drawables / ~22s | Guest content in our surface | Presentation route |
| --- | --- | --- | --- |
| Metal (macOS default) | 1420 | yes, 72% / 68% lit | `-present` directly |
| Vulkan (MoltenVK) | 1395 | yes, 72% lit | `addPresentedHandler:` |
| OGL | n/a | n/a | no such backend in this build |

`strings` on the binary lists exactly `Metal`, `Vulkan`, `Null`, `Software Renderer` — Dolphin's
macOS build has **no OpenGL backend**, so the "OGL" run configured a backend that does not exist and
rendered nothing. It was never evidence about OpenGL either way.

The defensible claim is narrower but still covers both real GPU backends: **one Metal-level hook
catches Metal and Vulkan-through-MoltenVK guests.** Whether macOS's legacy GL stack presents through
a client-visible `CAMetalLayer` is **[unverified]**, so `polymer-producer-opengl`'s fate on macOS is
undetermined rather than settled.

### Hook points by API

**Metal** — the primary target, and the one the virtual swapchain builds on.
`-[CAMetalLayer nextDrawable]` swizzles cleanly via `method_setImplementation` and fires for every
presented frame. The returned drawable's texture is an `AGXG16GFamilyTexture` — the private
per-GPU-family class anticipated here, reached via `object_getClass` on a live instance rather than by
name — and it carries a retrievable backing IOSurface. **[validated]**

```
[probe] Metal: nextDrawable -> layer=CAMetalLayer drawable=CAMetalDrawable texture=AGXG16GFamilyTexture
[probe] Metal: IOSurface 0x753c4a84c0  1280x960  seed=0  id=124 -- CAPTURABLE
```

This is Objective-C method swizzling, not vtable patching. `crates/hooks/polymer-hook/src/vtable.rs`
(1896 lines of COM proxying) does not port; macOS needs a sibling `objc.rs` implementing
`method_setImplementation`-style interposition against the same `HookChain` traits.

**Vulkan / MoltenVK** — the loader-layer plan does **not** work for the guests that matter, but this
turns out not to block capture. Dolphin `dlopen`s its bundled `libMoltenVK.dylib` directly rather
than going through the Vulkan loader **[validated]**, so `VK_LAYER_PATH` / `VK_INSTANCE_LAYERS` never
take effect and `polymer-hook-vulkan-layer` would never be inserted. The Metal hook catches the
Vulkan guest anyway.

A Vulkan layer remains the better vantage point for *frame timeline* information — the data the
dual-surface heuristics need — but it is not required to obtain frames, and for directly-linked
MoltenVK guests it is not available at all. Where timeline data is needed from such a guest, the
hook has to go into MoltenVK's own exports rather than a layer. **[unverified]**

The external-memory and external-semaphore handle types available in a given MoltenVK version remain
unchecked; `VK_EXT_metal_objects` is the export path for retrieving the `MTLTexture` behind a
`VkImage`. **[unverified]** — though if capture happens at the Metal layer, this may never be needed.

**OpenGL** — undetermined. The claim that macOS's GL stack is Metal-backed and therefore covered by
the same hook has been withdrawn (see above); it was never actually tested, because Dolphin's macOS
build has no OpenGL backend to test against. `CGLTexImageIOSurface2D` (the Syphon technique) remains
the likely path if a GL guest turns up that needs one. **[unverified]**

### Consumer side: composition

`bins/polymer-gui/src/shell_compositor.rs` builds a retained visual tree on
**`Windows.UI.Composition`** (WinUI Composition, not raw DirectComposition): `Compositor`,
`ContainerVisual`, `SpriteVisual`, `LayerVisual` + `InsetClip`, `DropShadow`,
`CompositionSurfaceBrush`, `CompositionStretch`, per-visual opacity and z-order, with GPU content
attached through `ICompositorInterop::CreateCompositionSurfaceForSwapChain`.

**CoreAnimation is the equivalent, and every feature maps one-to-one.** The whole mapping was
exercised in one probe, which passes: **[validated]**

| `shell_compositor.rs` | CoreAnimation |
| --- | --- |
| `Compositor` + commit batching | `CATransaction` begin/commit |
| `ContainerVisual` | plain `CALayer` used for grouping |
| `SpriteVisual` + `CompositionSurfaceBrush` | `CALayer` with `contents` |
| `LayerVisual` + `InsetClip` | `CALayer.masksToBounds` on a bounded layer |
| `DropShadow` blur / offset / colour / opacity | `shadowRadius` / `shadowOffset` / `shadowColor` / `shadowOpacity` |
| `CompositionStretch::None` | `kCAGravityCenter` |
| `CompositionStretch::Fill` | `kCAGravityResize` |
| `CompositionStretch::UniformToFill` | `kCAGravityResizeAspectFill` |
| `Visual.SetOpacity` | `CALayer.opacity` |
| z-order | `CALayer.zPosition` / sublayer order |
| `CreateCompositionSurfaceForSwapChain` | `CALayer.contents = IOSurfaceRef` |

The last row is the load-bearing one, and it is the reason this maps so cleanly: **a `CALayer` can
display an IOSurface directly**, which is exactly the object every captured frame already is. No
swapchain wrapper, no intermediate surface, no copy. `CALayer.h` documents `contents` as taking a
`CGImageRef`, but the IOSurface assignment is the same path `CAMetalLayer` uses internally, and it
verifiably renders.

The probe builds the full tree — an IOSurface-backed content layer inside a clipped, shadowed group
at 0.75 opacity, the content itself at 0.9 — renders it through `CARenderer` into an IOSurface-backed
Metal texture, and reads the pixels back:

```
first lit pixel at (30,30) BGRA = 2b 56 81 ff
```

The source surface is `40 80 bf`. Under the nested opacities (0.75 × 0.9 = 0.675) that is exactly
`2b 56 81` — a byte-for-byte match on all three channels. CoreAnimation sampled the IOSurface and
composited the whole tree correctly.

`CAMetalLayer` also sits in the same layer tree as a sibling **[validated]**, so the renderer's own
chrome output composites alongside guest surfaces the same way the D3D12 compositor's swapchain does
today.

`CARenderer` is worth noting in its own right: it renders a `CALayer` tree into a Metal texture, so
the GUI's composited output can be captured into an IOSurface without going through the screen —
directly useful for the WebRTC path in `polymer-renderer/src/renderer/webrtc.rs`.

**One gotcha, recorded because it cost time.** `CARenderer` draws the *committed* layer tree. Without
`[CATransaction flush]` before `beginFrameAtTime:`, the properties set moments earlier are still
pending and it renders nothing at all. An initial run without the flush produced an empty target and
looked exactly like "CoreAnimation cannot display an IOSurface" — a control layer with nothing but a
`backgroundColor` also rendered nothing, which is what exposed the harness as the fault rather than
the platform. Likewise `-[CALayer renderInContext:]` is a software path that does not handle these
layers; it is not a valid way to test this.

**What does not port:** Windows can host a composition visual tree *across processes*. macOS's
equivalent — `CAContext` and `CALayerHost` — is private API with no public header. **[verified]**
This does not affect Polymer, whose GUI receives frames over IPC and composites them in its own
process, but it forecloses a design where the guest submits layers directly into the GUI's tree.

## Input IPC

### SDL listeners — port, but not the way the Windows code does it

This is the part of the architecture that transfers best, and it matters most, because it is also
Polymer's answer to a platform gap: **macOS has no `uinput` and no public virtual-HID API.** The
existing design sidesteps that entirely by synthesising gamepads *inside* the guest rather than at
the system level.

The good news first. **The dynapi slot constants in `polymer-hook-sdl3/src/install.rs` transfer
verbatim.** They were re-derived independently on macOS by decoding each SDL export's arm64 stub
against the `_jump_table` address — every public SDL entry point is a three-instruction stub that
indexes the table:

```
_SDL_GetGamepadAxis:
    adrp  x8, 0x100f56000
    ldr   x2, [x8, #0xb48]     ; -> jump_table + 306*8
    br    x2
```

All six slots that Dolphin exports match the Windows constants exactly — `GetGamepadAxis` 306,
`GetGamepadButton` 309, `GetJoysticks` 395, `PollEvent` 677, `UpdateGamepads` 952, `WaitEvent` 972.
**[validated]** The remaining four constants are simply not referenced by that guest, so no export
survives to cross-check them; their table slots still exist.

`__DATA` is writable at runtime, so patching the table faults nothing and needs no `mprotect`.
**[validated]**

Two things about the *installation* path do not survive, though.

**The guest may link SDL statically.** Dolphin does: there is no `libSDL3.dylib` to `dlopen`, and
`SDL_DYNAPI_entry` is not an exported symbol, so neither the Windows approach nor the "mechanical"
`dlopen`/`dlsym` port described in earlier drafts of this document applies. `_jump_table` is a
*local* symbol invisible to `dlsym`. It is still recoverable, because these builds are unstripped
(61,651 symbols): walk the main image's `LC_SYMTAB` and rebase the symbol/string table file offsets
through the `__LINKEDIT` segment. **[validated]** — see `find_local_symbol` in the probe.

The macOS installer therefore needs two strategies, chosen at runtime: `dlsym` when SDL is a separate
dylib, and an `LC_SYMTAB` walk when it is statically linked.

**Installation must happen after SDL initialises, not from the dylib constructor.**
`SDL_InitDynamicAPI()` runs lazily on the first SDL call, which is *after* any inserted dylib's
constructor, and it repopulates `jump_table` — silently overwriting a patch installed at load time.
The first probe run showed the detour correctly installed and never firing for exactly this reason:
**[validated]**

```
[probe] selftest: slot 306 holds 0x1034022e0, detour is 0x104ea8fbc -- CLOBBERED by SDL_InitDynamicAPI
[probe] selftest: re-patched after SDL init
[probe] SDL slot 306 (GetGamepadAxis) reached -- DETOUR LIVE
```

On Windows this is handled by hooking `SDL_DYNAPI_entry` itself, i.e. the function that fills the
table. For a statically linked guest that export is unavailable; hooking `SDL_Init`, which *is*
exported, is the obvious substitute. **[unverified]**

Once installed after initialisation, the guest's own input path flows through the detour under real
load — **11,734 `SDL_UpdateGamepads` calls** from Dolphin's controller thread in a 20-second session.
**[validated]**

Guests that use GameController.framework or IOHIDManager directly need the same treatment in a
different idiom — ObjC swizzling for the former, symbol rebinding (fishhook-style `__DATA` segment
patching) for the latter.

A genuine system-wide virtual gamepad would require a HIDDriverKit system extension carrying
`com.apple.developer.driverkit.family.hid.device`, which Apple grants by request. That is a
logistical dependency rather than an engineering one, and the SDL listener design mostly removes the
need for it.

### Bluetooth — does not port

`polymer-bluetooth` is ~10.7k lines implementing a full host stack (HCI, L2CAP, SDP, pairing). Per
ARCHITECTURE.md it expects two radios: the onboard radio for BLE-class devices, and an external USB
radio with the WinUSB driver installed, taken over completely for EDR devices. The two transports
fare very differently on macOS.

**`transport/ble` (btleplug) — likely ports.** btleplug targets CoreBluetooth on macOS, so the BLE
half of the stack, covering Joy-Con 2 and the Nintendo Switch Online GameCube controller, has a
plausible path. Requires a Bluetooth TCC grant. **[unverified]**

**`transport/bredr` (nusb HCI over USB) — does not port.** Two independent blockers:

* On Apple Silicon the internal radio is not a USB device. On the test host: chipset `BCM_4388C2`,
  `Transport: PCIe`, vendor `0x004C (Apple)`. **[verified]** There is no userspace HCI transport for
  it; `transport/bredr/hci/usb.rs` has nothing to bind to.
* An external dongle does not rescue this. `IOBluetoothHostControllerUSBTransport` matches and claims
  USB Bluetooth devices, and macOS offers userspace no way to detach a kernel driver — libusb and
  nusb both report `detach_kernel_driver` as unsupported on macOS. Claiming the dongle first would
  require a DriverKit USB driver holding
  `com.apple.developer.driverkit.transport.usb`. **[unverified]**, but the direction is not in doubt.

The replacement is a different HAL behind the same `polymer-input` hub interface. The device decoders
in `devices/nintendo/*` and `devices/sony/*` are transport-agnostic and are the valuable part; they
should be retained.

* **Standard HID pads** (DualSense, Switch Pro): allow macOS to pair them normally, then take
  exclusive control with `IOHIDDeviceOpen` and `kIOHIDOptionsTypeSeizeDevice` **[verified]** in SDK,
  and speak raw reports. Rumble, touchpad and adaptive triggers survive. Costs an Input Monitoring
  grant.
* **Wii Remotes**: `IOBluetooth` L2CAP channels on `kBluetoothL2CAPPSMHIDControl` (0x0011) and
  `kBluetoothL2CAPPSMHIDInterrupt` (0x0013), both **[verified]** present in
  `BluetoothAssignedNumbers.h`. This is the long-established approach on macOS and is sufficient for
  the Dolphin passthrough use case.

The architectural promise that does **not** survive the port is zero-click pairing of non-standard
devices by bypassing the OS device manager. Device *support* is largely recoverable, but through
macOS's own pairing flow rather than around it.

## Other subsystems

An audit of every Windows API area in `Cargo.toml` against what this document covers turned up
several items the graphics/input analysis had missed. Two have **no macOS equivalent at all**; two
more are possible but demand a TCC permission that Windows does not require.

### No equivalent: per-process audio control

`crates/runtime/polymer-platform-windows/src/audio.rs` uses `IAudioSessionManager2` and
`ISimpleAudioVolume` to read and set **volume and mute for a specific process** — how Polymer
controls a guest emulator's audio independently of the system.

macOS has no counterpart. `AudioHardwareCreateProcessTap` with `CATapDescription`
exists and can *capture* a named process's output **[verified]** — `kAudioProcessPropertyPID` and
`kAudioProcessPropertyBundleID` identify the process — but there is **no per-process volume or mute
setter** anywhere in `CoreAudio`. **[verified]**

The only route to the same behaviour is heavier by an order of magnitude: tap the guest process,
route it into an aggregate device, apply gain in Polymer, and play the result back — turning a
property write into an audio pipeline Polymer must own and keep glitch-free. It also needs the Audio
Recording TCC grant. Muting specifically could be done by tapping and discarding, but volume control
means running the mixer.

This is worth deciding early, because "per-game volume" is a frontend feature that looks trivial on
Windows and is not.

### Desktop isolation: a private Space

`desktop.rs` creates a Windows station/desktop (`CreateDesktopW`, `SetThreadDesktop`) and launches
the guest onto it. That does three jobs at once: the guest's window is invisible to the user, it
cannot steal focus, and it is isolated from the user's input. `automation/driver.rs` then attaches
its *own* thread to that desktop with `DesktopThreadAttachment`, so UI automation drives the guest
there — invisibly, without disturbing anything the user is doing.

macOS has no *public* API for a second desktop, but it has the thing itself: Spaces. Creating one
and moving the guest's windows onto it recovers presentation and focus isolation outright; only
input isolation has no counterpart.

**What does not work.** Because Polymer already stages and re-signs a *copy* of the bundle, the
obvious move is to set `LSUIElement` in that copy's `Info.plist` and demote the app to an accessory.
This fails: a directly-exec'd guest still activates and takes focus with the flag set.
**[validated]** (`Info.plist` is also sealed by the signature, so the edit must precede the re-sign —
doing it afterwards leaves `codesign --verify` reporting an invalid bundle. **[validated]**)

**What does work** is applying the policy from *inside* the guest, where LaunchServices is not
consulted — which costs nothing extra, since Polymer injects a dylib regardless. Three steps, in
increasing order of what they buy: **[validated]**

| Applied in the injected hook | Result |
| --- | --- |
| `[NSApp setActivationPolicy:Accessory]` | accepted, but the guest is still frontmost |
| + swizzle `-activateIgnoringOtherApps:` to a no-op | blocked (Qt apps call it on startup) — still frontmost |
| + taking the window out of the user's Space | **no longer frontmost**; no Dock tile, no Cmd-Tab entry |

The activation policy and the `activate` block are necessary but not sufficient — the window itself
has to leave the user's Space.

**Order the window in; do not withhold it.** The tempting implementation — swallow `-orderFront:` so
the window is never ordered in — hides the guest but makes it **invisible to Accessibility too**:
`AXWindows` returns 0 and no automation is possible. Accessibility needs the window *ordered in*, not
*on screen*: **[validated]**

| Window state | AX can enumerate and `AXPress` it |
| --- | --- |
| ordered front, visible (control) | yes |
| **never ordered in** | **no — `AXWindows` = 0** |
| ordered front, moved off every display | yes |
| ordered front, `alphaValue = 0` | yes |
| ordered front, below the desktop window level | yes |

So the guest's windows must be ordered in normally. Hiding them is then a separate problem, solved
below by moving them to a Space the user cannot reach — which is both stronger than off-screen
placement and the honest analogue of what `desktop.rs` does on Windows.

### A real private Space, via SkyLight

A dedicated **Space** is the genuine analogue of a Windows private desktop, and it is reachable
through SkyLight's private CGS API. Every symbol needed is present on macOS 27: **[verified]**

```
CGSMainConnectionID   CGSSpaceCreate      CGSSpaceDestroy    CGSSpaceSetType
CGSAddWindowsToSpaces CGSRemoveWindowsFromSpaces             CGSCopySpacesForWindows
CGSCopyManagedDisplaySpaces
```

They live in `/System/Library/PrivateFrameworks/SkyLight.framework/SkyLight`, which is in the dyld
shared cache rather than on disk, so they are resolved with `dlsym` — there is no header to link
against. **No SIP changes are required.** These are the same calls yabai, Amethyst and Hammerspoon
have used for years; the interface has been stable since Spaces were reworked.

One signature detail costs an afternoon if missed: **`CGSSpaceCreate`'s second argument must be `1`,
not `NULL`** — passing `NULL` returns space id 0 and silently does nothing. **[validated]**

```c
CGSSpaceID space = CGSSpaceCreate(cid, (void *)1, NULL);
CGSSpaceSetType(cid, space, 0);                 // 0 = user space
CGSAddWindowsToSpaces(cid, @[@(win.windowNumber)], @[@(space)]);
```

**Adding to the private Space is the load-bearing step, not removing from the old one.** A control
that only called `CGSRemoveWindowsFromSpaces` left the window clickable in the user's Space — the
WindowServer appears to re-home an orphaned window. **[validated]**

Against Dolphin booting Wii Sports, with the guest's windows moved to a private Space:
**[validated]**

| User's route to the window | Result |
| --- | --- |
| Click it | **blocked** — 0 of 6 guest windows are in the current Space's on-screen list |
| Cmd-Tab / Dock | **blocked** — activation policy is Accessory |
| Mission Control / Ctrl-arrow | **blocked** — the Space is not among the managed display's Spaces |
| Accessibility automation | **still works** — `AXWindows = 2`, including the real title `"Dolphin 2606a \| JITARM64 SC \| Vulkan \| HLE \| Wii Sports (RSPE01)"` |
| Frame capture | **unaffected** — 1501 drawables, 72% lit surfaces |

That satisfies the hard requirement: **the user cannot focus the running instance**, through any of
the three UI routes, while automation and capture keep working.

**Spaces do not leak.** The private Space is never added to a managed display's Space list, and the
WindowServer reclaims it when the owning connection dies — the managed-Space count was identical
before and after a guest was `SIGKILL`ed with its destructor never running. **[validated]**

### What to be aware of

* **Coverage is per-window, and cooperative.** The hook moves windows as they pass through
  `-orderFront:` / `-makeKeyAndOrderFront:`. Dolphin owned 6 windows but only 2 went through those
  paths; the rest were never on-screen anyway. A guest that creates and shows a window by another
  route would land on the user's Space, so a production implementation should also sweep on
  `NSWindowDidBecomeVisibleNotification` rather than trusting the two selectors.
* **No input isolation.** A Windows desktop separates input as well; this only separates presentation
  and focus.
* **`DesktopThreadAttachment` has no analogue.** Automation works because Accessibility reaches
  across Spaces, not because Polymer can attach a thread to one — and it still costs an Accessibility
  grant that Windows does not require, revoked whenever a staged bundle is re-signed.
* **Modal dialogs land on the private Space too**, where the user will never see them — an argument
  for porting `polymer-hook-ui`'s modal interception early, so a guest that would have shown a dialog
  surfaces it through Polymer instead of hanging invisibly.

### Needs a permission Windows does not: guest UI automation and input forwarding

Two subsystems become TCC-gated:

* **`WindowsAutomationDriver`** (`platform-windows/src/automation/`) drives guest UI through UI
  Automation. The macOS equivalent is the Accessibility API — `AXUIElementCreateApplication`,
  `AXUIElementCopyAttributeValue`, `AXUIElementPerformAction` **[verified]** — gated on
  `AXIsProcessTrusted`.
* **`window.rs` input forwarding** posts `WM_KEYDOWN`, `WM_CHAR`, `WM_LBUTTONDOWN` and friends
  straight to a guest window with `PostMessage`. That is unprivileged on Windows and touches no
  global input state. macOS's nearest equivalent is `CGEventPostToPid` **[verified]**, which targets
  a *process* rather than a window and also requires the Accessibility grant.

Accessibility cannot be granted programmatically; the user must toggle it in System Settings. And it
is keyed to code identity, so **it is revoked every time Polymer re-signs a staged emulator bundle**
— the same interaction noted in the injection section. Any design that depends on driving guest UI
needs a first-run permission flow, and needs to survive losing that permission after an emulator
update.

Note the SDL virtual-gamepad path does **not** need this: it synthesises input inside the guest, so
no OS-level input injection is involved. Accessibility is only required for driving the emulator's
*own* UI.

### Tractable, but different

| Windows | macOS | Notes |
| --- | --- | --- |
| Media Foundation H.264 (`webrtc/encoders/d3d12.rs`) | VideoToolbox **[verified]** | Hardware H.264/HEVC encode |
| Job objects (`isolation.rs`, `KillOnCloseJob`) | process groups + `kqueue`/`NOTE_EXIT` | No single-handle "kill the whole tree" primitive; must be assembled |
| `resume_initial_thread` (suspended launch) | `posix_spawn` + `POSIX_SPAWN_START_SUSPENDED` | Direct equivalent, and required anyway for `DYLD_INSERT_LIBRARIES` |
| `QueryPerformanceCounter` (`timing.rs`) | `mach_absolute_time` / `clock_gettime_nsec_np` | Trivial |
| `windows-registry` (emulator discovery) | bundle scanning, `Info.plist`, LaunchServices | Different shape, no real difficulty |
| `polymer-hook-ui` (modal `MessageBox` interception) | swizzle `NSAlert` / `runModal` | Same ObjC interposition as the Metal hooks |
| `polymer-vfs-winfsp` | FSKit | Userspace, no kext; macFUSE works but is kext-bound |
| DInput hooks | n/a | Already slated for deprecation; no macOS analogue needed |

### Architecture selection becomes a Rosetta question

`architecture_artifact_dir` picks x86 vs x64 hook artifacts per target. The macOS equivalent is
Rosetta 2: an x86_64-only emulator runs translated, and **an arm64 dylib will not load into it**.
Dolphin ships universal (`x86_64 arm64` **[verified]**) but not every emulator does.

Two consequences: hook dylibs should be built universal (or selected per-arch), and Polymer needs to
detect translation — `sysctl.proc_translated` is the per-process flag. **[verified]** Rosetta also
interacts with the JIT entitlement, so a translated guest is the case most likely to break if the
re-signer drops `allow-jit`.

## Suggested sequencing

Validation shifted the balance here. The Metal producer is no longer the expensive Tier 2 item it
looked like — it is proven, it needs no inline hooking, and it subsumes the OpenGL and Vulkan
producers.

**Tier 1 — working build.** The entitlement-preserving re-signer, the virtual-swapchain
`CAMetalDrawable` producer, the SDL3 dynapi hook installed post-`SDL_Init`, an `IOHIDDevice`-seizing
input HAL, and the SkyLight private-Space isolation hook. Every mechanism in this tier has been
exercised against a real guest.

Two things belong in the design from the start rather than being retrofitted: the IPC layer needs a
four-line Objective-C `@protocol` declaration for the `NSXPCConnection` handshake (objc2 handles
everything else), and the launch path must exec the guest binary directly rather than through
LaunchServices.

**Tier 2 — completeness.** Whatever timeline data the dual-surface heuristics need, which for
directly-linked MoltenVK guests means hooking MoltenVK's exports rather than shipping a layer. The
producer-owned IOSurface ring and blit has dropped off this list entirely: with a virtual swapchain
the ring *is* the swapchain, and the guest renders into it directly.

**Out of scope.** Capturing a guest Polymer cannot inject into. Injection is the contract: if a
bundle cannot be staged, re-signed and injected, it is not a supported guest. There is no
screen-capture path.

**Not planned.** The BR/EDR userspace Bluetooth stack. Expose the same hub interface behind an IOKit
HAL instead.

## What remains unvalidated

Hook installation and dispatch are validated, and so is the cross-process frame handshake. What
remains:

* **End-to-end integration.** The two halves were validated separately — the hook against Dolphin,
  the IPC between two purpose-built test processes. A frame has not yet travelled from a hooked
  `nextDrawable` in a real guest to a consumer process.
* **Whether the private `-addPresentScheduledHandler:` surface is stable** across macOS releases,
  and whether other guests send selectors these two did not.
* **Throughput and latency.** The IPC test moved one 256x256 surface, not 60 fps of 1280x960 with
  the event signalled per frame.
* **OpenGL guests entirely** — no macOS Dolphin backend exists to test against, and the earlier
  claim of coverage is withdrawn.
* **Guests that run translated under Rosetta** — an x86_64-only emulator is still in scope, and
  needs an x86_64 hook dylib.
* **Per-process audio**: whether a tap-and-remix pipeline can match `ISimpleAudioVolume`'s latency
  and reliability, or whether per-game volume should simply be dropped on macOS.
* **Guest windows that reach the screen without passing through `-orderFront:`** — Space coverage is
  per-window and a sweep on window-visible notifications is likely needed.
* **Any guest other than Dolphin** — in particular guests that link SDL dynamically, and guests
  using GameController.framework or IOHIDManager instead of SDL.
* **`SCM_RIGHTS` for the shared-memory ring.** Assumed straightforward, still untested.

## Scope estimate

112 of 528 Rust files reference `windows`, `windows_sys`, or `target_os = "windows"`, but that
overstates the work — much of it is thin platform plumbing in IPC and process management that has
direct POSIX equivalents. The concentrated cost is `polymer-bluetooth`'s BR/EDR transport, which is a
rewrite against a different HAL rather than a port.
