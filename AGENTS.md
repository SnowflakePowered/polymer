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

Read ARCHITECTURE.md for a high level overview of the architecture.