# Firmware API

Firmware storage holds user-supplied BIOS files identified by consolespec. Key storage is a separate mechanism and is outside this API's import flow. Downloadable system updates and NAND installation retain their existing flows.

## Recognition and installation

On drag-and-drop, Gumshoe runs first. If identification rejects a regular file, the host calculates its complete-file MD5 and SHA-256 hashes and compares them with the BIOS hashes in `consolespec::MachineSpec::all()`.

Either listed hash can identify a BIOS. The first match supplies the platform from `MachineSpec::id()` and the label from `Bios::name()`, with type `bios`. Each file installs exactly one firmware description. Filenames do not affect identification. A file with no matching BIOS hash is rejected.

The host checks and saves the same immutable file snapshot. The exact `(platform, type, label)` tuple is the primary key; reimport replaces that slot. No BIOS bytes are passed to plugins for recognition or sent to scrapers.

## Reading and mounting

`onLaunch` receives `args.firmwareStore`, and `onBootstrap(device, context)` receives `context.firmwareStore`. Both use `FirmwareContext` from `polymer:api`. The host binds the store to the launch console or bootstrap device; callers cannot redirect it by changing callback arguments.

```ts
import type { FirmwareContext, Filesystem } from "polymer:api";

async function configureBios(args: FirmwareContext & { filesystem: Filesystem }) {
  const bios = (await args.firmwareStore.list()).find(entry =>
    entry.type === "bios" && entry.label === "Recognized BIOS variant"
  );
  if (!bios) throw new Error("Add the required BIOS using drag-and-drop.");
  const file = await args.firmwareStore.open(bios.type, bios.label);
  args.filesystem.mountFile("/system/bios.bin", file, { mode: "r" });
}
```

`list()` returns descriptions containing `platform`, `type`, `label`, and `byteLength`. `open(type, label)` returns the existing read-only `FileEntry`, with the label as its name. Lookup uses the bound platform together with the supplied type and label. Missing entries reject. Opened and mounted files keep their original contents across replacement; later opens see the replacement.

The library allocates `<storage>/firmware/firmware.db` for metadata and `<storage>/firmware/<platform>/objects/<blake3>` for immutable contents. Superseded objects are retained in this initial implementation.

## Frontend import

`Polymer.library.import({ path })` returns either `{ kind: "games", imports }` using the existing launchable/scrape-job records, or `{ kind: "firmware", firmware }` containing the single installed description. `launchables.add` remains game-only. Horizon routes dropped files through the general endpoint and reports firmware installation through its activity UI.

Emulator BIOS-setting migrations and a firmware management screen are separate work.
