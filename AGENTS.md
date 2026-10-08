# Polymer for Agents


## Code Quality

* Prefer functions bound as struct or trait impls instead of free functions in a module
* Split up huge files into multiple sub modules.
* Multi-file modules should use the mod.rs style. Single file modules should be a <module-name>.rs
* If there is common behaviour that can be abstracted over with a trait, use that instead of duplicating interfaces.
* Prefer inlining code inside trait or struct impls instead of shims that call free functions.
* Keep cyclomatic complexity low (less than 10)
* Use zerocopy for parsing binary structures if possible.
* Do not write tests that just test the obvious behaviour of a function or existence tests like struct layouts. Prefer static asserts where applicable.
* Do not write tiny one-off helper functions.
* For concurrent hashmaps, use `dashmap` rather than a RwLock or Mutex around a std HashMap.
* To launch a game winthin `polymer-gui`, you can use Chrome's inspector protocol. Don't forget to pass in the `--debug` flag to start the debug server.
* Do not write tests for .ts plugins. They will never work. 
* "Snowglass" is the old name for this project. If the user types in 'Snowglass' instead of 'Polymer', they meant 'Polymer'.
* Make only the smallest change needed for a request or feature. 
* Follow existing conventions for Actors and JS <-> Rust messaging.
* Do not leave explanatory comments in your code. 
* This is a **pre-release** project that has not shipped to any end users. Do not include migration code in the core app. Do one-time migrations instead to keep test databases updated, and collapse database schema versions.
Read ARCHITECTURE.md for a high level overview of the architecture.

## Testing and Modelling
* Do not write inline tests. Always put tests in the tests/ folder. 
    * Do not write tests that only tests private APIs, unit tests or otherwise.
    * Do not write tests that require mocks. 
    * Do not write tests that test for the shape of an API.
    * Do not write tests against a removal of a feature.
    * Do not write change detector tests. Change detectors provide negative value.
    * Do not write constructors or logic that purely serve tests.
    * Prefer modelling via TLA+ over tests where it makes sense. 
* Do allow inline Kani proofs
* Launching an emulator with `isolate: true` or `isolate: false` only affects whether its launched in a private desktop. Filesystem access is always virtualized.


