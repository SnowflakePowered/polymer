(() => {
  "use strict";

  globalThis.__polymerInstallFileSystem = (ops, registerHandle) => {
    const paths = new WeakMap();
    const names = new WeakMap();
    const pathOf = (handle) => {
      const path = paths.get(handle);
      if (path === undefined) throw new TypeError("Illegal invocation");
      return path;
    };
    const baseName = (path) => path.split(/[\\/]/).filter(Boolean).at(-1) ?? path;
    const make = (Kind, path) => {
      const handle = Object.create(Kind.prototype);
      paths.set(handle, path);
      names.set(handle, baseName(path));
      registerHandle(handle, path);
      return handle;
    };
    const illegal = () => { throw new TypeError("Illegal constructor"); };
    const aborted = () => new DOMException("The user aborted a request.", "AbortError");

    class FileSystemHandle {
      constructor() { illegal(); }
      get name() { pathOf(this); return names.get(this); }
      async isSameEntry(other) { return paths.get(other) === pathOf(this) && other.kind === this.kind; }
      async queryPermission() { pathOf(this); return "granted"; }
      async requestPermission() { pathOf(this); return "granted"; }
    }

    class FileSystemWritableFileStream extends WritableStream {
      constructor() { illegal(); }
      write(data) {
        const writer = this.getWriter();
        const written = writer.write(data);
        writer.releaseLock();
        return written;
      }
      seek(position) { return this.write({ type: "seek", position }); }
      truncate(size) { return this.write({ type: "truncate", size }); }
    }

    const bytesOf = async (data) => {
      if (typeof data === "string") return new TextEncoder().encode(data);
      if (data instanceof Blob) return new Uint8Array(await data.arrayBuffer());
      if (data instanceof ArrayBuffer) return new Uint8Array(data);
      if (ArrayBuffer.isView(data)) return new Uint8Array(data.buffer, data.byteOffset, data.byteLength);
      throw new TypeError("Data must be a string, Blob, BufferSource, or write command");
    };
    const resize = (state, size) => {
      const bytes = new Uint8Array(size);
      bytes.set(state.bytes.subarray(0, Math.min(size, state.bytes.length)));
      state.bytes = bytes;
    };
    const apply = async (state, chunk) => {
      const command = chunk !== null && typeof chunk === "object" && typeof chunk.type === "string" ? chunk : { type: "write", data: chunk };
      if (command.type === "seek") {
        state.position = Number(command.position);
      } else if (command.type === "truncate") {
        resize(state, Number(command.size));
        state.position = Math.min(state.position, state.bytes.length);
      } else if (command.type === "write") {
        if (command.position !== undefined && command.position !== null) state.position = Number(command.position);
        const bytes = await bytesOf(command.data);
        const end = state.position + bytes.length;
        if (end > state.bytes.length) resize(state, end);
        state.bytes.set(bytes, state.position);
        state.position = end;
      } else {
        throw new TypeError(`Unknown write command '${command.type}'`);
      }
    };

    class FileSystemFileHandle extends FileSystemHandle {
      get kind() { pathOf(this); return "file"; }
      async getFile() {
        const path = pathOf(this);
        const [bytes, lastModified] = await Promise.all([ops.op_fs_read(path), ops.op_fs_modified(path)]);
        return new File([bytes], names.get(this), { lastModified });
      }
      async createWritable(options = {}) {
        const path = pathOf(this);
        let bytes = new Uint8Array(0);
        if (options.keepExistingData) {
          try {
            bytes = new Uint8Array(await ops.op_fs_read(path));
          } catch (error) {
            if (error?.name !== "NotFoundError") throw error;
          }
        }
        const state = { bytes, position: 0 };
        return Reflect.construct(WritableStream, [{
          write: (chunk) => apply(state, chunk),
          close: () => ops.op_fs_write(path, state.bytes),
        }], FileSystemWritableFileStream);
      }
    }

    class FileSystemDirectoryHandle extends FileSystemHandle {
      get kind() { pathOf(this); return "directory"; }
      async *entries() {
        const path = pathOf(this);
        for (const entry of await ops.op_fs_entries(path)) {
          let child;
          try {
            child = await ops.op_fs_child(path, entry.name, entry.kind, false);
          } catch (error) {
            if (error?.name === "NotFoundError") continue;
            throw error;
          }
          yield [entry.name, make(entry.kind === "directory" ? FileSystemDirectoryHandle : FileSystemFileHandle, child)];
        }
      }
      async *keys() { for await (const [name] of this.entries()) yield name; }
      async *values() { for await (const [, handle] of this.entries()) yield handle; }
      [Symbol.asyncIterator]() { return this.entries(); }
      async getFileHandle(name, options = {}) {
        return make(FileSystemFileHandle, await ops.op_fs_child(pathOf(this), String(name), "file", !!options.create));
      }
      async getDirectoryHandle(name, options = {}) {
        return make(FileSystemDirectoryHandle, await ops.op_fs_child(pathOf(this), String(name), "directory", !!options.create));
      }
      async removeEntry(name, options = {}) {
        await ops.op_fs_remove(pathOf(this), String(name), !!options.recursive);
      }
      async resolve(possibleDescendant) {
        const root = pathOf(this), descendant = pathOf(possibleDescendant);
        if (descendant === root) return [];
        const prefix = /[\\/]$/.test(root) ? root : `${root}${root.includes("\\") ? "\\" : "/"}`;
        return descendant.startsWith(prefix) ? descendant.slice(prefix.length).split(/[\\/]/) : null;
      }
    }

    const request = (kind, options) => {
      const types = Array.isArray(options.types) ? options.types : [];
      const filters = types.map((type) => ({
        description: String(type?.description ?? ""),
        extensions: Object.values(type?.accept ?? {}).flat().map((extension) => String(extension).replace(/^\./, "")),
      }));
      if (filters.length && !options.excludeAcceptAllOption) filters.push({ description: "All files", extensions: ["*"] });
      const start = options.startIn !== null && typeof options.startIn === "object" ? paths.get(options.startIn) : undefined;
      return {
        kind,
        multiple: !!options.multiple,
        suggestedName: options.suggestedName === undefined ? null : String(options.suggestedName),
        startIn: start === undefined ? null : options.startIn.kind === "file" ? start.replace(/[\\/][^\\/]*$/, "") : start,
        filters,
      };
    };
    const pick = async (kind, options) => {
      const picked = await ops.op_fs_pick(request(kind, options ?? {}));
      if (!picked.length) throw aborted();
      return picked;
    };

    const define = (name, value) => Object.defineProperty(globalThis, name, { value, writable: true, configurable: true });
    define("FileSystemHandle", FileSystemHandle);
    define("FileSystemFileHandle", FileSystemFileHandle);
    define("FileSystemDirectoryHandle", FileSystemDirectoryHandle);
    define("FileSystemWritableFileStream", FileSystemWritableFileStream);
    define("showOpenFilePicker", async function showOpenFilePicker(options) {
      return (await pick("open", options)).map((path) => make(FileSystemFileHandle, path));
    });
    define("showSaveFilePicker", async function showSaveFilePicker(options) {
      return make(FileSystemFileHandle, (await pick("save", options))[0]);
    });
    define("showDirectoryPicker", async function showDirectoryPicker(options) {
      return make(FileSystemDirectoryHandle, (await pick("directory", options))[0]);
    });
  };
})();
