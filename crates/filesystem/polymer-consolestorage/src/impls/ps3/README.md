# PS3 component histories

The PS3 NAND composer owns package identity, file replacement, and version rules.
Git storage persists component revisions and publishes the composed NAND through
the shared recovery journal.

Firmware is a complete `dev_flash` snapshot with its own history. Game packages
and updates share a main history per installation directory. Each DLC content ID
has a separate history. Package revisions retain their extracted files, entry
overwrite flags, digest, and application/target version metadata.

Composition replays selected package histories in their original installation
order. An overwrite entry replaces the current file; another entry writes only
when the path is absent. This also applies to `PARAM.SFO`. Incremental updates
retain files from earlier packages. Rolling back or removing a contribution
replays the remaining operations, so a previously skipped file can become visible.

`TARGET_APP_VER` must match the effective application version when that package
is replayed. Updates without a target cannot lower a known application version.
A DLC's application version is not treated as its package revision or a universal
minimum game version. DLC without package-version metadata is identified by its
digest. Game-specific runtime compatibility is not inferred.

`history(id)` lists the selected component's active ancestry, including base and
update packages for a main history. `restore(id, revision)` selects an ancestor;
later revisions leave that history. Uninstalling an update removes its first
occurrence and the later main-history suffix. `affected` reports other artifacts
lost by that operation and packages whose target-version checks would fail.
Removing a base application requires removing its dependent title components;
removing firmware requires removing the installed packages.

PKG and PUP extraction is staged before publication. Extraction failure or
cancellation leaves the NAND unchanged. Once publication is journaled, recovery
completes that transition. Existing NANDs from the previous global history need
one-time conversion outside the application. License keys remain managed by the
key store, separately from package histories.
