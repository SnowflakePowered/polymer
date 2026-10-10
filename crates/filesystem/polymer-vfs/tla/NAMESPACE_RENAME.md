# Namespace rename and remove

`NamespaceRename.tla` models `PreparedFilesystem`'s `tree_files` cache racing
a rename or remove of the same paths. A cached node pins a host handle that
follows its object. Host handles share delete, so a file rename succeeds while
open, a directory rename fails while any descendant is open, and a non-POSIX
delete stays pending until the last handle closes. A resolve is a host lookup
followed by `remember_tree_file`, whose `or_insert_with` prefers an existing
entry over the node the lookup just found.

`Design` selects when the mutation evicts the cache:

| Design | Behavior |
|---|---|
| `after` | Evict after the host mutation (original rename and remove) |
| `before` | Evict before the host mutation |
| `both` | Evict before and after |
| `fenced` | Evict before and after, refuse to cache under an in-flight mutation, and refuse to cache a lookup that began before a mutation completed (implemented) |

## Results

| Scenario | `after` | `before` | `both` | `fenced` |
|---|---|---|---|---|
| Directory rename | Blocked by cache, stale resolve, incoherent | Blocked by cache, stale resolve, incoherent | Blocked by cache, stale resolve, incoherent | Pass (222) |
| File rename | Stale create, stale resolve, incoherent | Stale create, stale resolve, incoherent | Stale create, stale resolve, incoherent | Pass (180) |
| File replace | Stale create, stale resolve, incoherent | Stale create, stale resolve, incoherent | Stale create, stale resolve, incoherent | Pass (102) |
| File remove | Incoherent, delete pending forever | Incoherent, delete pending forever | Incoherent, delete pending forever | Pass (206) |

- **Blocked by cache** (`RenameBlockedOnlyByRealHandles`): an entry left by a
  completed lookup makes a directory rename fail with access denied. Under
  `after`, any earlier lookup under the directory is enough; this broke
  PPSSPP's firmware install. Evicting first only narrows it to a lookup that
  completes between the eviction and the rename.
- **Stale create** (`CreateReturnsFresh`): a lookup at the source finishes on
  the host before the rename and caches after the eviction. A later
  `create_file` at the source hands back the moved file, so the writes land
  in the renamed file.
- **Stale resolve** and **incoherent** (`ResolveSeesNamespace`,
  `CacheCoherentAtRest`): the same overlap leaves an entry that names an
  object the path no longer holds.
- **Delete pending forever** (`PendingDeleteCompletes`): a lookup that
  overlaps a remove caches the deleted file. Its handle is never released, so
  the name stays delete-pending and a new file can't be created there.

A lookup that overlaps a mutation still holds its own transient handle, so a
concurrent open can make a directory rename fail. NTFS has the same rule, and
the invariant allows it.

## Validation

```powershell
$env:TLC_WORKERS = '2'
./scripts/tlc.ps1 crates/filesystem/polymer-vfs/tla/NamespaceRename.tla -Config MCNamespaceRename.cfg
./scripts/tlc.ps1 crates/filesystem/polymer-vfs/tla/NamespaceRename.tla -Config MCNamespaceRenameFile.cfg
./scripts/tlc.ps1 crates/filesystem/polymer-vfs/tla/NamespaceRename.tla -Config MCNamespaceRenameReplace.cfg
./scripts/tlc.ps1 crates/filesystem/polymer-vfs/tla/NamespaceRename.tla -Config MCNamespaceRenameRemove.cfg
cargo test -p polymer-tests --test filesystem_plan
```

`MCNamespaceRenameEvictAfter.cfg`, `MCNamespaceRenameEvictAfterRemove.cfg`,
`MCNamespaceRenameEvictBefore.cfg` and `MCNamespaceRenameEvictBoth.cfg`
reproduce the counterexamples for the earlier designs.
