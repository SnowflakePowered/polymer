# Polymer Architecture

This document is human-written and must remain human written. Agents MUST NOT modify this document. If any changes are made where this document deviates from the observed architecture, agents MUST notify the user of the discrepency.

This repository hosts the Polymer emulator frontend platform ("Polymer"),  the "Horizon" theme for Polymer, and various TypeScript plugins to interact with external standalone emulators and scraping services.


## Overview
Polymer is a sui-generis emulator frontend that goes beyond launching a program with arguments. Using DLL hooking to reimplement core parts of the operating system stack, Polymer is able to integrate with standalone emulators at a deep level.

Polymer currently supports Windows only, but is designed in such a way that only certain leaf crates must be implemented to extend support for macOS and Linux.

At its core, Polymer consists of five parts:

### Graphics IPC
  
This is contained in the `consumer` and `producer` directories. These crates hook into an emulator's graphics device, and use GPU shared memory APIs to transfer the emulator's displayed swapchain texture for recomposition in the **GUI**.

Graphics IPC is also responsible for supplying swapchain size information for **Input IPC** in order to do screen-space mouse interpolation, as well as containing a number of heuristics to determine which surfaces to expose to the **GUI**. For example, dual screen handheld emulators will merge two surfaces into a single swapchain texture. Heuristics are used to determine from the frame's rendering timeline what the two separate surfaces are before sending those to the **GUI**.

Frame *Producers* interact with the hooked device, and send the texture over to a Frame *Consumer*. We support producing frames from D3D11, D3D12, OpenGL, Vulkan, and GDI; and consuming frames from a host D3D12, or Vulkan device. *Producers* are injected into an emulator ("guest process") via a *hook*; graphics hooks are implemented for D3D11, D3D12, OpenGL, and Vulkan. Frames are signaled to the **GUI** via the IPC protocol in `polymer-ipc`. 

The **graphics IPC** system is contained in the following crates

* `polymer-consumer-core` -- Common logic for frame consumers
* `polymer-consumer-d3d12` -- D3D12 frame consumer
* `polymer-consumer-vulkan` -- Vulkan frame consumer
* `polymer-hook-d3d11` -- D3D11 entrypoint Hook 
* `polymer-hook-d3d12` -- D3D12 entrypoint Hook 
* `polymer-hook-dxgi` -- DXGI entrypoint Hook 
* `polymer-hook-gdi` -- GDI entrypoint Hook 
* `polymer-hook-vulkan-layer` -- Vulkan layer hook
* `polymer-hook-opengl32` -- OpenGL entrypoint hook
* `polymer-producer-d3d11` -- D3D11 frame producer
* `polymer-producer-d3d12` -- D3D12 frame producer
* `polymer-producer-gdi` -- GDI frame producer
* `polymer-producer-opengl` -- OpenGL frame producer
* `polymer-producer-vulkan` -- Vulkan frame producer

### Input IPC

Similarly to **Graphics hooks**, we also hook a guest process's input handling in order to implement *virtual gamepads* to map host devices to guest input. This allows us to generate a fixed set of configuration files for a guest process while allowing on-the fly input remapping and reconfiguration. 

The SDL input hooks expose standard SDL gamepads to the guest application named "Polymer Virtual Gamepad"; the emulator plugin is to map these as well as possible to the guest controller layout. We then capture input in the **GUI** via SDL 3 or our *userspace bluetooth driver* in `polymer-bluetooth` and resynthesize the inputs on the *virtual gamepad*, potentially after a controller remapping filter. Gamepad layouts for an emulated guest console are defined by the `consolespec` crate.

At an architectural level, input handling is modeled as **HAL**s and **listeners**, which talk to the hub in `polymer-input`. HALs speak to the host hardware driver, whereas listeners like the SDL input hooks listen for input events and re-emit them to the guest application. We support two HALs -- SDL3, and `polymer-input-bluetooth`. 

For listeners, we support hooking SDL 2 and SDL 3 in the guest process as well as limited support for DInput (soon to be deprecated). We also support hooking the Win32 Bluetooth API for *bluetooth passthrough*. 

Bluetooth support is handled via two crates, `polymer-bluetooth`, which is a userspace bluetooth driver, and `polymer-input-bluetooth`, which is the HAL for the input IPC. `polymer-input-bluetooth` is responsible for decoding the raw HID messages from the userspace driver and publishing those messages to the `polymer-input` hub. This allows us to support zero-click support for non-standard Bluetooth input devices, such as Wii Remotes, Nintendo Switch and Switch 2 controllers, and DualShock 3/4/DualSense with complete support for their respective features such as pressure sensitive buttons, HD rumble, touchpads, and adaptive triggers. By bypassing the OS device manager we can support native-like pairing of these non standard devices. The userspace Bluetooth driver handles and expects two separate Bluetooth radios; the onboard Bluetooth radio used for BLE communication for BLE-class devices, such as Joy-Con 2 and the Nintendo Switch Online GameCube controller, and an external bluetooth radio with the WinUSB driver installed that we take complete control of to handle EDR devices like Wii Remotes, DualShock 3, and Joy-Con 1.
  
Listener crates can also subscribe to raw HID reports, which allow *bluetooth passthrough*. While it is really only used for Dolphin since only Wii Remotes are "unique" enough to reequire support for this, it is in principle supported for any emulator that requires speaking a custom Bluetooth protocol.  

The **Input IPC** system is contained in the following crates

* `polymer-input` -- The common input hub 
* `polymer-bluetooth` -- Userspace Bluetooth driver
* `polymer-input-sdl3` -- HAL for SDL3
* `polymer-input-bluetooth` -- HAL for Bluetooth

### Filesystem

In order to truly isolate the guest process as well as provide niceties like automatic save backup and restore and seamless game installation, Polymer must emulate the *structure* of a guest console's storage devices and file system, as well as the directory structure expected by the guest process. 

For example, to launch a game from a PKG for PlayStation 3, a user must first install it in the RPCS3 Qt UI. Once installed, there is no user-friendly way to manage the installation like one would on an actual PlayStation 3. Polymer's filesystem abstraction solves this by implementing the PlayStation 3's NAND structure and PKG installation process outside the guest. At runtime, the emulated NAND is mounted via a virtualized filesystem so the guest process sees the emulated NAND as expected. 

Additionally, memory cards are are emulated in the same way. Through the virtual filesystem, Polymer detects writes to a mounted memory card file by the guest process, and redirects them to individual save file directories on the host. This allows "Folder Memory Cards" without giving up compatibility or cross-game save reading such as in Metal Gear Solid 2. 

This is all implemented via the `polymer-consolestorage` crate, which handles each console manually. Each emulated file system is backed by a Git repository on the host disk, so save files are versioned and can be rolled-backed or backed up simply by pushing to a Git server. `polymer-vfs` then glues all of this together during runtime, exposing a jailed filesystem that the guest process can't escape easily out of, ensuring all writes and reads are logged. This allows Polymer to control exactly where save files, configuration files, and game data is read and written. 

`consolestorage` also provides reflection on each individual emulated NAND and memory card. It provides endpoints for save and game ("depot") enumeration as well as icons and metadata associated with each save and installed game. This allows the GUI to manipulate NANDs and memory cards within the frontend itself.

The **Filesystem** system is contained in the following crates

* `polymer-consolestorage` -- Console aware NAND, EEPROM/SRAM/etc. battery save, memory card handling, and installation drivers.
* `polymer-vfs` -- Host-agnostic VFS implementation
* `polymer-vfs-winfsp` -- Windows driver for `polymer-vfs`.

### Plugin Host Runtime
The plugin runtime manages plugin registration and execution, which are small TypeScript programs used to configure the behaviour of guest emulator launches, scraping, and theme definitions.

It handles the lifecycle of a plugin, which is run in it's own Deno isolate, and provides the following APIs for plugins.

* `pkg` import namespace -- import resources, `toml` configuration, and JS/TS modules within the package.
* `polymer` import namespace -- plugin-host APIs
  * `polymer:uiautomation` -- UI automation host, implemented by `polymer-automation`
  * `polymer:cache` -- Plugin-local persistent cache directory
  * `polymer:pluginStorage` -- Plugin-local ephemeral storage, like `AsyncLocalStorage`, but only supports JSON-safe objects.
  * `polymer:database` -- Plugin-local persistent SQLite database, as a replacement for IndexedDB.
* `jsr` imports from the [jsr.io](https://jsr.io) repository. 
  * Plugins get access to most of the ES2025 specification as well as a large subset of the Web API. Not supported is WebCrypto, IndexedDB, CacheStorage, or DOM APIs. The `Node` or `Deno` namespaces are also not available, and `npm` packages are not supported.
 
There are three types of plugins. Plugins implement a particular interface via their exports, but a specific plugin may also export non-interface functions which themes can use to implement specific functionality. Plugins also have a general `onPluginLoad` callback which is called once on the first time a plugin is loaded at runtime, which may be used to set up reusable state.

#### `theme` plugins

Theme plugins simply package a SPA bundle into a plugin bundle and don't have special access to plugin host APIs. These plugins don't interact with the plugin host and instead use the `Polymer` frontend JS API, and can use the full build-time NodeJS ecosystem if desired.

They declare a `main` entry point that is an `html` that is rendered by the GUI. Optionally, for debug purposes, they can declare a `theme.debug` section including a `serve` and `url` key which will run the particular NPM command on start if the GUI is started with the `--debug` flag. 

#### `emulator` plugins

Emulator plugins are used to orchestrate the execution of guest emulator processes. Emulator plugins expect a TypeScript `main` which supports the following exports.

* `onLaunch` -- Called when launching the emulator, and returns an `EmulatorLaunchConfig`. Emulator plugins MUST export this function.
   
  `onLaunch` returns the state of the virtualized filesystem, configuration options, and emulator launch arguments required to start the game via the emulator. It is responsible for configuring the emulator via the `configuration` API, setting up the state of the virtualized filesystem via the `filesystem` API, declaring the controller topology, as well as the graphics and input IPC hooks to use as well as the surfaces to track. 

  The emulator will launch in a sandboxed and isolated desktop and filesystem, without access to the external filesystem. For this reason the state of the filesystem must be properly declared or the guest process will fail to start properly.

  `onLaunch` receives the following arguments in its `args` object
  * `launchPath` -- The path of the game "launchable" on disk. This should not be passed directly to the emulator launch arguments, but instead mounted to the virtual filesystem.
  * `console` -- The consolespec console ID being launched. Use this to disambiguate which platform to set up for mulit-platform emulators.
  * `package` -- Details retrieved from the game package, if available, such as title id or serial.
  * `controllerTopology` -- The declared controller topology at time of launch.
  * `configuration` -- An instance of the configuration as declared in the configuration TOML files.
  * `filesystem` -- An instance of the virtual file system that the emulator will launch on.
  * `storage` -- The console storage resources for this system, such as the emulated NAND or any memory cards.
  * `userConfiguration` -- User provided configuration options as declared at time of launch
  * `saveStates` -- Save states for quick resume, if present.
  
* `onConfigure`  -- Called when retrieving user-facing configuration options. Returns the set of options exposed to the user.

  `onConfigure` returns the schema and values of a plugin-defined configuration. This is separate from the emulator's `configuration` object which represents the full configuration tree for the emulator, but instead declares the user-facing options that should be applied at `onLaunch`.  

  Emulator plugins should export this function if they wish to expose user-facing configuration options.

  `onConfigure` receives the following arguments in its `args` object.

  * `game` -- Details about the game to configure
  * `userConfiguration` -- The previously configured state 
  
* `onPauseResume` -- Called when pausing or resuming the emulating instance.
  
  Generally, a preconfigured `action` is used to toggle pausing and resuming the emulation instance. 

  Emulator plugins should export this function to support pausing the emulation while the pause menu is available.

  `onPauseResume` receives the following arguments in its `args` object.

  * `pauseState` -- Either `"pause"` or `"resume"` depending on the state.
* `onStateSave` -- Called when saving a savestate. 
  
  Called when a save state is requested by the user. 

  Emulator plugins should export this function if to supports save states through the GUI. `onStateResume` MUST be exported to as well as this function to support save states.

  `onStateSave` receives the following arguments in its `args` object

  *  `slot` -- The save state slot
  *  `filesystem` -- The filesystem instance
  *  `directory` -- ??
* `onStateResume` -- Called when resuming a save state
* `onControllerChanged` -- Called when the controller topology changes 
* `onParseHotkey` -- Called to parse a hotkey string into keycodes or gamepad expressions.

  This function takes a raw `string` that is the hotkey expression for the emulator, and returns the key code for the hotkey.
* `onQuit` -- Called immediately before the emulating instance is exited via the GUI.

  The emulator plugin MUST export `onQuit` 
  
  This function returns the save state used as the quick resume save state if supported.
  

##### Configuration

Emulator plugins must define the configuration files of the guest emulator via TOML definitions under the `emulator.configuration.definitions` key. All TOML files under this key are effective concatenated together during evaluation.




#### `scraper` plugins

Scraper plugins are used to interface with scraping databases.

* `updateContents` -- Called to update the database of the scraper. This might be irrelevant if the scraper simply calls an external service, but is useful when it involves a local database or large binary file which needs to be parsed.
* `onScrape` -- Called to scrape a game.

### Frontend Runtime (GUI)

The frontend runtime glues all these parts together and can really be split into deeper parts.


#### IPC
The IPC protocol interacts with the **Input IPC** and **Graphics IPC** in order to ferry information between the guest process and the frontend host.

