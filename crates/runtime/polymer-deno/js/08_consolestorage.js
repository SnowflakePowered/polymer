import {
  op_consolestorage_platforms,
  op_consolestorage_inspect,
  op_consolestorage_recover,
  op_consolestorage_install,
  op_consolestorage_bootstrap,
  op_consolestorage_uninstall,
  op_consolestorage_plan_removal,
  op_consolestorage_create,
  op_consolestorage_delete,
  op_consolestorage_list,
  op_consolestorage_set_label,
  op_consolestorage_supports,
  op_consolestorage_fork,
  op_consolestorage_saves,
  op_consolestorage_save_history,
  op_consolestorage_save_restore,
  op_consolestorage_save_copy,
  op_consolestorage_save_delete,
} from "ext:core/ops";

export function createConsoleStorage(library) {
  const storageData = new WeakMap();
  const storages = new Set();
  const deleted = new WeakSet();
  const internal = Symbol("Polymer.consolestorage.Storage");

  class Storage extends EventTarget {
    constructor(token, data) {
      super();
      if (token !== internal) throw new TypeError("Use Polymer.consolestorage.Storage.create()");
      storageData.set(this, Object.freeze({ ...data }));
      storages.add(this);
    }

    static async create(kind, platform, options = undefined) {
      if (kind === "battery") {
        throw new TypeError("Battery profiles are created automatically for each game");
      }
      if (!["nand", "saveRepo", "memoryCard"].includes(kind)) {
        throw new TypeError("Unknown console storage kind");
      }
      if (typeof platform !== "string" || platform.length === 0) {
        throw new TypeError("Console storage requires a platform id");
      }
      const label = options?.label === undefined ? null : String(options.label);
      return new Storage(internal, await op_consolestorage_create(kind, platform, label));
    }

    static async supports(kind, platform) {
      if (!["nand", "saveRepo", "memoryCard", "battery"].includes(kind)) {
        throw new TypeError("Unknown console storage kind");
      }
      if (typeof platform !== "string" || platform.length === 0) {
        throw new TypeError("Console storage requires a platform id");
      }
      return await op_consolestorage_supports(kind, platform);
    }

    static async list(options = undefined) {
      const storages = (await op_consolestorage_list()).map((data) => new Storage(internal, data));
      return options?.kind === undefined
        ? storages
        : storages.filter((storage) => storage.kind === options.kind);
    }

    get device() { return { platform: this.platform, id: this.id }; }
    async inspect() { return op_consolestorage_inspect(this.platform, this.id); }
    async recover() { return op_consolestorage_recover(this.platform, this.id); }
    async install(source) {
      if (!source || typeof source.path !== "string"
        || Object.keys(source).some(key => key !== "path")) {
        throw new TypeError("Frontend installations require a local source: { path: string }");
      }
      return op_consolestorage_install(this.platform, this.id, source.path);
    }
    async bootstrap(settings = {}) { return op_consolestorage_bootstrap(this.platform, this.id, settings); }
    get artifacts() {
      return Object.freeze({
        list: async () => (await this.inspect()).artifacts,
        planRemoval: (ids) => op_consolestorage_plan_removal(this.platform, this.id, ids),
        uninstall: async (plan) => {
          const artifacts = await op_consolestorage_uninstall(this.platform, this.id, plan.requested, plan.revision);
          await Promise.all(plan.requested.map(async (id) => {
            const launchable = await library.Library.launchables.find(`artifact://${this.platform}/${this.id}/${id}`);
            if (launchable == null) return;
            await library.Library.launchables.delete(launchable.id);
            library.contentChanged({ rootBinaryId: launchable.parentBinaryId ?? launchable.binaryId });
          }));
          return artifacts;
        },
      });
    }

    get saves() {
      const { kind, platform, id } = storageData.get(this);
      const volume = { kind, platform, id };
      return Object.freeze({
        list: () => op_consolestorage_saves(volume),
        history: (save) => op_consolestorage_save_history(volume, save),
        restore: (save, commit) => op_consolestorage_save_restore(volume, save, commit),
        copyTo: (save, destination) => op_consolestorage_save_copy(volume, save, { kind: destination.kind, platform: destination.platform, id: destination.id }),
        delete: (save) => op_consolestorage_save_delete(volume, save),
      });
    }

    async fork(label = null) {
      const { kind, platform, id } = storageData.get(this);
      return new Storage(internal, await op_consolestorage_fork({ kind, platform, id }, label === null ? null : String(label)));
    }

    get kind() { return storageData.get(this).kind; }
    get id() { return storageData.get(this).id; }
    get platform() { return storageData.get(this).platform; }
    get label() { return storageData.get(this).label; }
    get created() { return new Date(storageData.get(this).created); }
    get path() { return storageData.get(this).path; }

    async setLabel(label) {
      const data = storageData.get(this);
      const next = label === null || label === undefined ? null : String(label);
      return new Storage(internal, await op_consolestorage_set_label(data.kind, data.platform, data.id, next));
    }

    async delete() {
      const data = storageData.get(this);
      await op_consolestorage_delete(data.kind, data.platform, data.id);
      deleted.add(this);
      storages.delete(this);
    }

    toJSON() { return { ...storageData.get(this) }; }
  }

  function gameSaved(detail) {
    for (const storage of storages) {
      const data = storageData.get(storage);
      if (data.kind === detail.kind && data.platform === detail.platform && data.id === detail.id) {
        storage.dispatchEvent(new Event("gamesaved"));
      }
    }
  }

  const installs = new Map();
  function installChanged(detail) {
    const install = Object.freeze({ ...detail });
    if (install.state === "complete" || install.state === "failed") installs.delete(install.id);
    else installs.set(install.id, install);
    ConsoleStorage.dispatchEvent(new CustomEvent("installchange", { detail: install }));
  }

  const ConsoleStorage = new EventTarget();
  Object.defineProperties(ConsoleStorage, {
    Storage: { value: Storage },
    platforms: { value: () => op_consolestorage_platforms() },
    installs: { get: () => [...installs.values()] },
  });
  return Object.freeze({ ConsoleStorage, gameSaved, installChanged });
}
