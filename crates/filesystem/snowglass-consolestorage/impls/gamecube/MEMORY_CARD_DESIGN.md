# GameCube memory-card projections

The canonical representation is a flat collection of GCI files under `card/`. A GCI contains the
complete 64-byte console directory entry followed by its 8 KiB data blocks, so no separate dirent
or card manifest is required.

Canonical storage may contain saves from every region. Each mounted projection is region-pure. The
region is derived from the primary game's six-byte GameCube product code, and only canonical GCIs
whose game code belongs to that region are eligible for the view. Writes carrying another region
are rejected rather than imported.

The `dolphin` projection is a directory of `.gci` files. It follows Dolphin's folder-card behavior:

- every primary-game save is loaded first;
- other same-region saves are added best-effort;
- 10% of the card's usable blocks and 14 directory entries are kept available where possible;
- filenames are derived from maker code, game code, and the console filename;
- a rename to a `.deleted` suffix removes that save from the canonical card.

The `raw` projection is a random-access card image. Its default geometry is MC-251: 16 Mbit,
2 MiB, 256 physical blocks, and 251 usable blocks. The codec itself is capacity-independent. It
infers geometry from the image and header, supports all attested 4/8/16/32/64/128 Mbit sizes, and
supports any larger geometry representable by the 4,091-entry allocation table.

A raw image contains the card header, two directory copies, two block-allocation-table copies, and
the selected GCI payload blocks. Reconciliation validates the header, checksums, active redundant
metadata, allocation chains, duplicate identities, and projection region before replacing canonical
GCIs. Card identity is derived from the storage UUID, keeping the synthesized header stable without
making raw card metadata canonical.
