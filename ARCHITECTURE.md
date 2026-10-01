# Polymer Architecture

This document is **human-written** and **MUST remain human written**. Agents **MUST NOT** modify this document. If any changes are made where this document deviates from the observed architecture, agents MUST notify the user of the discrepency.

This repository hosts the Polymer emulator frontend platform ("Polymer"),  the "Horizon" theme for Polymer, and various TypeScript plugins to interact with external standalone emulators and scraping services.


## Overview
Polymer is a sui-generis emulator frontend that goes beyond launching a program with arguments. Using library hooking to reimplement core parts of the operating system stack, Polymer is able to integrate with standalone emulators at a deep level.

Polymer currently supports Windows and macOS, with support for Linux planned at a later point.

At its core, Polymer consists of five parts:

### Graphics IPC
  
This is contained in the `consumer` and `producer` directories. These crates hook into an emulator's graphics device, and use GPU shared memory APIs to transfer the emulator's displayed swapchain texture for recomposition in the **GUI**.
Graphics IPC is also responsible for supplying swapchain size information for **Input IPC** in order to do screen-space mouse interpolation. Instead of getting a swapchain managed by the graphics driver, Polymer provides it's own swapchain to the emulator so it can draw directly to the composed display in the **GUI**. This allows Polymer to get frame data from the emulator with zero additional copies (with the exception of OpenGL which requires one copy). It is important for multi-screen handhelds that each screen is rendered to its own dedicated swapchain for proper recomposition in the **GUI**. 

Frame *Producers* interact with the hooked device, and send the texture over to a Frame *Consumer*. We support producing frames from D3D11, D3D12, Vulkan, and OpenGL; and consuming frames from a host D3D12, or Vulkan device. *Producers* are injected into an emulator ("guest process") via a *hook*; graphics hooks are implemented for D3D11, D3D12, OpenGL, and Vulkan. Frames are signaled to the **GUI** via the IPC protocol in `polymer-ipc`. 

The Polymer-owned swapchain will respect resize events at startup. Once created, Polymer will refuse to resize the swapchain. For best results, the emulator guest should be configured to size its window to the internal resolution of the rendering game, and render to one window per screen (i.e. one swapchain per screen). 

The **graphics IPC** system is contained in the following crates

* `polymer-consumer-d3d12` -- D3D12 frame consumer
* `polymer-consumer-vulkan` -- Vulkan frame consumer
* `polymer-hook-d3d11` -- D3D11 entrypoint Hook 
* `polymer-hook-d3d12` -- D3D12 entrypoint Hook 
* `polymer-hook-dxgi` -- DXGI entrypoint Hook 
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

Bluetooth support is handled via two crates, `polymer-bluetooth`, which is a userspace bluetooth driver, and `polymer-input-bluetooth`, which is the HAL for the input IPC. `polymer-input-bluetooth` is responsible for decoding the raw HID messages from the userspace driver and publishing those messages to the `polymer-input` hub. This allows us to support zero-click support for non-standard Bluetooth input devices, such as Wii Remotes, Nintendo Switch and Switch 2 controllers, and DualShock 3/4/DualSense with complete support for their respective features such as pressure sensitive buttons, HD rumble, touchpads, and adaptive triggers. 

By bypassing the OS device manager we can support native-like pairing of these non standard devices. The userspace Bluetooth driver handles and expects two separate Bluetooth radios; the onboard Bluetooth radio used for BLE communication for BLE-class devices, such as Joy-Con 2 and the Nintendo Switch Online GameCube controller, and an external bluetooth radio with the WinUSB driver installed that we take complete control of to handle EDR devices like Wii Remotes, DualShock 3, and Joy-Con 1.
  
Listener crates can also subscribe to raw HID reports, which allow *bluetooth passthrough*. While it is really only used for Dolphin since only Wii Remotes are "unique" enough to require support for this, it is in principle supported for any emulator that requires speaking a custom Bluetooth protocol.  

The **Input IPC** system is contained in the following crates

* `polymer-input` -- The common input hub 
* `polymer-bluetooth` -- Userspace Bluetooth driver
* `polymer-input-sdl3` -- HAL for SDL3
* `polymer-input-bluetooth` -- HAL for Bluetooth

### Filesystem

In order to truly isolate the guest process as well as provide niceties like automatic save backup and restore and seamless game installation, Polymer must emulate the *structure* of a guest console's storage devices and file system, as well as the directory structure expected by the guest process. 

For example, to launch a game from a PKG for PlayStation 3, a user must first install it in the RPCS3 Qt UI. Once installed, there is no user-friendly way to manage the installation like one would on an actual PlayStation 3. Polymer's filesystem abstraction solves this by implementing the PlayStation 3's NAND structure and PKG installation process outside the guest. At runtime, the emulated NAND is mounted via a virtualized filesystem so the guest process sees the emulated NAND as expected. 

Additionally, memory cards are are emulated in the same way. Through the virtual filesystem, Polymer detects writes to a mounted memory card file by the guest process, and redirects them to individual save file directories on the host. This allows "Folder Memory Cards" without giving up compatibility or cross-game save reading such as in Metal Gear Solid 2. 


This is all implemented via the `polymer-consolestorage` crate, which handles each console manually. "Raw" memory cards and "folder" memory card formats are backed by the same storage and are both constructed on the fly at runtime. The card population algorithm ensures that the requested title is always present in the folder memory card; it is when filled with other saves until the card is full up to 10% of remaining space. 

Emulated file systems are backed by a Git repository on the host disk (one per platform). Save files are versioned on a per-game basis and can be rolled-backed or backed up simply by pushing to a Git server. Similarly, package installation is modeled as a single commit, so installed PKGs and WADs can be uninstalled via ostensibly a  `git revert`.  Save files and game contents are kept separately in different git trees in the same repository.

`polymer-vfs` then glues all of this together during runtime, exposing a jailed filesystem that the guest process can't escape easily out of, ensuring all writes and reads are logged. This allows Polymer to control exactly where save files, configuration files, and game data is read and written. 

`consolestorage` also provides reflection on each individual emulated NAND and memory card. It provides endpoints for save and game ("depot") enumeration as well as icons and metadata associated with each save and installed game. This allows the GUI to manipulate NANDs and memory cards within the frontend itself.

During an installation, content tickets and title keys (RAPs, .tik, etc.) are not persisted to the backing storage but instead parsed and saved to a per-platform key store. This per-platform keystore is mounted at emulation launch time to ensure the correct keys are available when starting a game.

The **Filesystem** system is contained in the following crates

* `polymer-consolestorage` -- Console aware NAND, EEPROM/SRAM/etc. battery save, memory card handling, and installation drivers.
* `polymer-vfs` -- Host-agnostic VFS implementation
* `polymer-vfs-winfsp` -- Windows driver for `polymer-vfs`.
* `polymer-vfs-fskit` -- macOS FSKit driver for `polymer-vfs`.


#### IPC
The IPC protocol interacts with the **Input IPC** and **Graphics IPC** in order to ferry information between the guest process and the frontend host. On macOS, the IPC protocol is also used to interact with the filesystem driver.

The IPC protocol is explicitly unstable and can change between versions of Polymer. 

To initialize input over the IPC protocol, an initial handshake is done via a Unix socket on Windows, and NSXPC on macOS (the "transport"). This handshake opens a handle to a memory mapped file containing a ring buffer containing the input state for the lowest-latency input events. Topology change `Blob` messages as well as bidirectional feedback requests (i.e. rumble, speaker, calibration), remain on the transport connection.

To initialize graphics, the transport handshake first sends over the handles for the shared memory textures and signaling timelines, as well as swapchain metadata such as swapchain dimensions. Since Polymer owns the swapchain the dimensions do not change once created. When a frame is complete, the producer signals its ready timeline which can be waited on. The consumer signals its release timeline when its ready to release a frame. The exact type of semaphore used is unspecified and can vary per operating system. On Windows, Direct3D 12 fences are used regardless of producer API. On macOS, only Metal is supported with `MTLSharedEvent`. On Linux, Vulkan timeline semaphores are used (on OpenGL via `GL_NV_timeline_semaphore`).

Raw HID messages can't use shared memory and go directly through the transport as a stream. On macOS, FSKit requests are also carried through the transport. 

### Plugin Host Runtime
The plugin runtime manages plugin registration and execution, which are small TypeScript programs used to configure the behaviour of guest emulator launches, scraping, and theme definitions.

It handles the lifecycle of a plugin, which is run in it's own Deno isolate, and provides the following APIs for plugins.

* `pkg` import namespace -- import resources, `toml` configuration, and JS/TS modules within the package.
  * `toml` files are serialized into JSON-safe objects.
  * `ts` and `js` imports work as JavaScript module imports
  * Raw folder and file imports import as `DirEntry` or `FileEntry` respectively. This is particularly useful when mounting. 
* `polymer` import namespace -- plugin-host APIs
  * `polymer:uiautomation` -- UI automation host, implemented by `polymer-automation`
  * `polymer:cache` -- Plugin-local persistent cache directory
  * `polymer:pluginStorage` -- Plugin-local ephemeral storage, like `AsyncLocalStorage`, but only supports JSON-safe objects.
  * `polymer:database` -- Plugin-local persistent SQLite database, as a replacement for IndexedDB.
  * `polymer:actions` -- Only available to emulator plugins that have defined an `actions.toml`. Allows
* `jsr` imports from the [jsr.io](https://jsr.io) repository. 
  * Plugins get access to most of the ES2025 specification as well as a large subset of the Web API. Not supported is WebCrypto, IndexedDB, CacheStorage, or DOM APIs. The `Node` or `Deno` namespaces are also not available, and `npm` packages are not supported.
 
There are three types of plugins. Plugins implement a particular interface via their exports, but a specific plugin may also export non-interface functions which themes can use to implement specific functionality. Plugins also have a general `onPluginLoad` callback which is called once on the first time a plugin is loaded at runtime, which may be used to set up reusable state.

#### `theme` plugins

Theme plugins simply package a SPA bundle into a plugin bundle and don't have special access to plugin host APIs. These plugins don't interact with the plugin host and instead use the `Polymer` frontend JS API, and can use the full build-time NodeJS ecosystem if desired.

They declare a `main` entry point that is an `html` that is rendered by the GUI. Optionally, for debug purposes, they can declare a `theme.debug` section including a `serve` and `url` key which will run the particular NPM command on start if the GUI is started with the `--debug` flag. 

#### `emulator` plugins

**This section is incomplete**

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

  `onLaunch` must return an object describing the launch shape of the guest process, including which hooks to use, and surfaces to track.

  Because emulator launches live within a virtualized filesystem, the game package as well as the emulator executable itself must be mounted on the filesystem before launch can continue. Additionally, the emulator plugin is responsible for setting configuration options for the emulator. 
  
* `onConfigure`  -- Called when retrieving user-facing configuration options. Returns the set of options exposed to the user.

  `onConfigure` returns the schema and values of a plugin-defined configuration. This is separate from the emulator's `configuration` object which represents the full configuration tree for the emulator, but instead declares the user-facing options that should be applied at `onLaunch`.  

  Emulator plugins should export this function if they wish to expose user-facing configuration options.

  `onConfigure` receives the following arguments.

  * `package` -- Details about the game to configure
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

  The emulator plugin SHOULD export `onQuit` 
  
  This function returns the save state used as the quick resume save state if supported.
  

##### Configuration

Emulator plugins must define the configuration files of the guest emulator via TOML definitions under the `emulator.configuration.definitions` key. This is separate from the `onConfigure`, which specifies a regular JSON object for **user-facing** configuration. The emulator plugin is to take this JSON object and apply it to the `configuration` object at `onLaunch`.

The `configuration` object is created via the TOML definitions specified. By convention, they live in the `config` folder in a plugin. Each TOML file under the  `emulator.configuration.definitions` key are effective concatenated together during evaluation. 

Configuration is specified by *targets* and *sections*. A *target* defines the final serialized output of a set of configuration options. Targets are hierarchical and can have sub-targets, but a root target must have an `output` key which defines the final format and canonical output path of the configuration file. All sub-targets of root targets are serialized into the same file.

A *section* is a description of what ends up serialized in the *target*. By default they are name-matched to a *target*, but this can be overriden with the `target` key such that the serialize will then walk the provided section when serializing the specified target. Sections provide the content; each section contains a collection of *options* which represent the actual configuration option of the configuration file. The `emit` key provides various override behaviours, including `bool_style` which defines the styling of boolean options.

During serialization, the serializer walks these sections and targets to create the set of required configuration files. These configuration options can be modified at `onLaunch` via the `configuration.options()` proxy API, which exposes the configuration graph as a JS object.

Opaque to the configuration graph, there are *actions* and *inputs*. These are used for hotkeys and input mapping. 

The `action` block is a special *section* that also requires defining a `target`. Then `actions.map` defines the expression and hotkey action title map. The expression is not abstracted and is an emulator-native string; the raw string is passed to `onParseHotkey` to map to Polymer input expressions or keycodes before an action is executed.

The `input` block defines input mapping for Polymer's virtual gamepad. Targets must be defined for each gamepad mapping. `input.mapping` defines the mapping between Polymer's input expressions and the emulator-native pad vocabulary for the standard virtual gamepad. Then each `layout` definition defines the mapping between an input expression and the vocabulary for the emulated controller. *input* layouts are also a special case of *section* and can define a set of options as well that is exposed via the controller topology slot, if the given topology aligns with the layout.

`targets` and `sections` can be marked `repeat` to indicate to the serializer that a section can have multiple entries of the same shape. This enables the `append` function for the plugin-side proxy object for the section, which appends a new entry to the repeated section. 

##### Filesystem Mounting

During `onLaunch`, emulator plugins must define the filesytem that the emulator executes in. For this, it provides a `mount` method that can mount various different types of `Mountable` objects.

* `Package` -- The "natural entrypoint" for the game to be launched. This could be a single ROM or ISO file, CUE+BIN bundles, or an installed bundle of files. After a `Package` is mounted, `mount` returns the natural entrypoint that can be passed to the emulator `argv` for the package.
* `Storage` -- A managed storage device, such as savegame memory cards, system NANDs, and other devices for the emulated platform managed by Polymer.
  * Omitting the file format mounts it as its natural directory or file format. 
  * `raw` -- For memory cards, mount it as a raw memory card file structure
  * `dolphin` -- For GCI memory cards, mounts it as a GameCube Memory Card folder in Dolphin's format.
  * `pcsx2` --- For PS2 memory cards, mounts it as a PCSX2 Memory Card folder.
  * Any writes to save storages are persisted in the save repository, and system NAND writes are discarded. 
* `Configuration` -- A configuration file rendered to its target format. This is mounted in memory and can be refreshed with `Configuration.flush()`.
* `Uint8Array` -- Any raw byte buffer stored in memory.
* `DirEntry` -- Any directory or file backed by the real filesystem. The plugin runtime limits external filesystem access to within declared plugin caches or files within the plugin package itself.

Scratch directories can also be mounted by omitting an object to mount. These directories are backed by a temporary directory on disk and are torn down when the emulator exists.
<!--TODO: figure out SD cards-->

<!-- * `mountTree` -- Mounts a `DirEntry` or URL as a tree under the route.
* `mountScratch` -- Mounts a scratch directory that is backed either by memory or by a temporary directory on disk. This directory is deleted after the emulator shuts down.
* `mountStorage` -- Mounts a managed storage device in `args.storage`. Depending on the storage, it may be mounted under the following formats.
  * Omitting the file format mounts it as its natural directory or file format. 
  * `raw` -- For memory cards, mount it as a raw memory card file structure
  * `dolphin` -- For GCI memory cards, mounts it as a GameCube Memory Card folder in Dolphin's format.
  * `pcsx2` --- For PS3 memory cards, mounts it as a PCSX2 Memory Card folder.
* `mountFile` -- Mount either raw bytes which are hosted in memory, or a `FileEntry`.  -->

#### `scraper` plugins

**This section is incomplete**
Scraper plugins are used to interface with scraping databases.

* `updateContents` -- Called to update the database of the scraper. This might be irrelevant if the scraper simply calls an external service, but is useful when it involves a local database or large binary file which needs to be parsed.
  
* `onScrape` -- Called to scrape a game.

  `onScrape` is an async generator function that yields scrape results. Scrape result are keyed under a hierarchy of well-known keys and their string or URL value. In particular, `media` and `banner` keys are always interpreted as URLs, and the runtime will automatically cache these for frontends to dsiplay.

  A `ScrapeResult` is a JSON-safe object `{ key: string, value: string | URL, source: string }`.

 `onScrape` receives previous scrape candidate results for the keys named in its `scraper.requires` and `scraper.optional` configuration keys in its context object, as well as the game package(s) metadata being scraped. Conversely, the keys a scraper returns must be defined in `scraper.provides`. A scraper will only be run if `all` of its `scraper.requires` requirements are fulfilled. Both lists accept `any(...)` groups. A scraper waits for other scrapers that provide a key from one of its unfulfilled requirements.

##### Common scraping keys

Listed is a hierarchy of well known scraping keys that frontends can rely on.
###### `package`
`package` keys is information derived by the game package.

* `package.platform` - The `consolespec` platform ID derived from the game package
* `package.internalname` - Any internal name of the game package
* `package.serial` - Any internal serial of the game package
* `package.titleid` - Any internal title id of the game package

###### `filename`
`filename` keys are information derived by the filename of the ROM or package.

* `filename.raw` - The raw filename of the file.
* `filename.nointro` - If the filename is parseable as NoIntro format, structured information from the filename. Has the subkeys `title`, `version`, `release`, `region`, `media`, `scene`, `language`, `flag`
* `filename.tosec` - If the filename is parseable as NoIntro format, structured information from the filename. Has the subkeys `title`, `version`, `demo`, `date`, `publisher`, `system`, `video`, `region`, `language`, `copyright`, `development`, `dumpinfo`, `media`, `flag`
* `filename.goodtools` - If the filename is parseable as NoIntro format, structured information from the filename. Has the subkeys `title`, `year`, `multilanguage`, `translation`, `version`, `volume`, `ninone`, `dumpcode`, `gamehack`, `media`, `flag`.

###### `meta`
`meta` keys are generic metadata that can be sourced via third party databases

* `meta.title` - The title of the game release. Accepts BCP-47 language subkeys.
* `meta.description` - The description of the game release. Accepts BCP-47 language subkeys.
* `meta.publisher` - The publisher of the game release
* `meta.genre` - A genre of the game release.
* `meta.rating` - The age rating of the game release. Requires the subkeys `esrb`, `cero`, `usk`, `pegi`, `dgsc`, `acb`, `iarc`, `grac`, `rars`, `gmedia` (defined as https://www.globalratings.com/ratings-definitions/).
* `meta.region` - The release region of the game as hyphen-separated ISO 3166 codes, with `EU` for Europe, `AS` for Asia, and `ZZ` for unknown.
* `meta.releasedate` - The release date of the game. Always in ISO8601 YYYY-MM-dd format.

###### `media`
`media` keys are any media sourced via third party databases, in the form of URLs.

* `media.banner` - A horizontal banner, which could be animated.
* `media.boxart` - Boxart images. Requires the subkeys `front`, `back`, `spine`, `full`.
* `media.logo` -  A game title or logo image
* `media.label.disc` - The front label of a disc
* `media.label.cart` - The front label of a game cartridge

###### `banner`
`banner` keys are any media or icon derived from the game package, in the form of URLs.

* `banner.icon` - A game icon embedded in the package
* `banner.background` - A game background embedded in the package
* `banner.logo` - A game title or logo image embedded in the package
* `banner.audio` - An audio file embedded in the package
* `banner.banner` - A horizontal banner embedded in the package, which could be animated.
* `banner.3d` - A 3D model representing the game embedded in the package

###### `runtime`
`runtime` keys are information that is derived possibly from third party sources but may be needed to run the game.

* `media.cue` - An optional scraped cuesheet candidate. This does not replace the descriptor of an identified package.

#### `platform` plugins

Platform plugins handle platform specific logic related to system software and game updates. It exposes the following callbacks.

- `onBootstrap` -- Called to download firmware packages and installing the depot via `onBootstrap`.
- `onCheckUpdates` -- Called to check updates for a title that is added to the library.
- `onFetchUpdate` -- Called to download updates from a server.

### Frontend Runtime (GUI)

**This section is incomplete**
The frontend runtime glues all these parts together and can really be split into deeper parts.


### Library


TODO:



