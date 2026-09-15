# Firmware API

Firmware is user-supplied hardware data such as a BIOS or hardware keys. Downloadable system updates and data obtainable from a NAND dump belong in the existing system-storage flows.

## Recognition

A platform plugin may export `onCheckFirmware(file: FileEntry)`, directly or on its default export. It may be asynchronous. Use the existing `bytes()`, `text()`, and `digest(algorithm)` methods to recognize the input; `name` is the input basename. The host supplies an immutable snapshot.

Return an array of `FirmwareInstallDescriptor` values:

```ts
return [
  { platform: "SONY_PSX", type: "bios", label: "Recognized BIOS variant" },
];
```

Each descriptor stores one file. Omit `contents` to copy the checked input, or supply a `Uint8Array` containing extracted bytes. A file containing several keys can return several descriptors. Platform IDs must be declared by the plugin. Types and labels are nonempty strings; use stable conventions shared with emulator consumers.

`null`, `undefined`, and an empty array mean no match. On drag-and-drop, Gumshoe runs first. If identification rejects a regular file, the host checks platform plugins in plugin-ID order. Exceptions and invalid batches are recorded and skipped. The first valid nonempty batch is installed atomically; an installation failure stops the import.

The exact `(platform, type, label)` tuple identifies a slot. Reimport replaces its contents using that same composite primary key. Use distinct labels for variants that should coexist. No firmware bytes are put in the game catalog or passed to scrapers.

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

`Polymer.library.import({ path })` returns either `{ kind: "games", imports }` using the existing launchable/scrape-job records, or `{ kind: "firmware", firmware }` containing installed descriptions. `launchables.add` remains game-only. Horizon routes dropped files through the general endpoint and reports firmware installation through its activity UI.

Concrete recognizers, emulator BIOS-setting migrations, and a firmware management screen are separate work.
