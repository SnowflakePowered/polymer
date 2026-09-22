// Plugin-worker filesystem API. This is intentionally separate from Polymer.*.
import {
  op_vfs_mount,
  op_vfs_cache_resource,
  op_vfs_path,
  op_vfs_resource_bytes,
  op_digest,
  op_vfs_resource_entries,
  op_vfs_resource_remove,
  op_vfs_scratch_resource,
  op_vfs_import_package,
  op_vfs_storage,
  op_vfs_quick_resume,
  op_vfs_flush,
} from "ext:core/ops";

const handles = new WeakMap();
const saveStateEntries = new WeakSet();
const storageHandles = new WeakSet();
const packages = new WeakMap();
const configurationMounts = [];

function resource(description) {
  if (description == null) return undefined;
  const value = Object.freeze({ kind: description.kind, path: description.path });
  handles.set(value, description);
  return value;
}

function readableEntry(description, path, entry, root = false, saveState = false, methods = {}) {
  const current = root ? path : [...path, entry.name];
  if (entry.kind === "directory") {
    const value = Object.freeze({
      ...methods,
      name: entry.name,
      kind: "directory",
      async *[Symbol.asyncIterator]() {
        for (const child of op_vfs_resource_entries(description.kind, description.handle, current)) {
          yield readableEntry(description, current, child, false, saveState);
        }
      },
      async remove(name) {
        name = String(name);
        if (!name || name.includes("/") || name.includes("\\") || name === "." || name === "..") {
          throw new TypeError("resource entry name must be one plain path component");
        }
        op_vfs_resource_remove(description.kind, description.handle, [...current, name]);
      },
    });
    if (saveState) saveStateEntries.add(value);
    handles.set(value, { ...description, components: current });
    return value;
  }
  const value = Object.freeze({
    name: entry.name,
    kind: "file",
    async digest(algorithm) {
      return op_digest({ type: "resource", kind: description.kind, token: description.handle, components: current }, algorithm);
    },
    async bytes() {
      return op_vfs_resource_bytes(description.kind, description.handle, current);
    },
    async text() {
      return new TextDecoder().decode(op_vfs_resource_bytes(description.kind, description.handle, current));
    },
  });
  if (saveState) saveStateEntries.add(value);
  handles.set(value, { ...description, components: current });
  return value;
}

function readableResource(description, saveState = false, methods = {}) {
  const value = readableEntry(description, [], {
    name: description.name,
    kind: description.kind === "tree" ? "directory" : "file",
  }, true, saveState, methods);
  handles.set(value, description);
  return value;
}

const storageResources = Object.create(null);
for (const description of op_vfs_storage()) {
  const value = resource(description);
  storageHandles.add(value);
  Object.defineProperty(storageResources, description.name, {
    value, enumerable: true, writable: false,
  });
}
Object.freeze(storageResources);

const quickResumeDescription = op_vfs_quick_resume();
const quickResume = quickResumeDescription == null ? null : Object.freeze({
  state: readableResource(quickResumeDescription.state),
  screenshot: quickResumeDescription.screenshot == null
    ? null
    : readableResource(quickResumeDescription.screenshot),
});

function mode(options) {
  if (options == null || typeof options.mode !== "string") {
    throw new TypeError("a filesystem mount requires an explicit string mode");
  }
  return options.mode;
}

const filesystem = Object.freeze({

  mount(target, source, options) {
    target = String(target);
    if (arguments.length <= 2) {
      const scratchOptions = source === undefined ? {} : source;
      if (scratchOptions === null || typeof scratchOptions !== "object"
        || Array.isArray(scratchOptions) || ArrayBuffer.isView(scratchOptions)
        || Object.keys(scratchOptions).some(key => key !== "mode")) {
        throw new TypeError("scratch mount options may only contain mode");
      }
      return op_vfs_mount(target, { type: "scratch" }, mode({ mode: scratchOptions.mode === undefined ? "rwx" : scratchOptions.mode }));
    }
    const permissions = mode(options);
    if (storageHandles.has(source)) {
      const format = options.format;
      if (format !== undefined && typeof format !== "string") {
        throw new TypeError("mount format must be a string");
      }
      const handle = handles.get(source);
      return op_vfs_mount(target, { type: "storage", kind: handle.kind, token: handle.handle, format }, permissions);
    }
    if ("format" in options) {
      throw new TypeError("mount format is only supported for storage");
    }
    if (packages.has(source)) {
      return mountPackage(target, packages.get(source), permissions);
    }
    if (typeof source?.render === "function") {
      const root = target.replace(/\/+$/, "");
      const rendered = source.render();
      const names = Object.keys(rendered).sort();
      for (const name of names) {
        filesystem.mount(`${root}/${name}`, rendered[name], { mode: permissions });
      }
      configurationMounts.push({ root, configuration: source, permissions, names });
      return filesystem.path(root || "/");
    }
    if (source instanceof Uint8Array) {
      return op_vfs_mount(target, { type: "bytes", contents: source }, permissions);
    }
    const handle = handles.get(source);
    if (handle === undefined || (source.kind !== "directory" && source.kind !== "file")) {
      throw new TypeError("mount source must be a directory entry, file entry, configuration, package, storage, or Uint8Array");
    }
    return op_vfs_mount(target, {
      type: "entry", kind: handle.kind, token: handle.handle,
      components: handle.components ?? [], directory: source.kind === "directory",
    }, permissions);
  },

  flush() {
    refreshConfigurations();
    op_vfs_flush();
  },

  path(path) { return op_vfs_path(String(path)); },
});

function mountPackage(target, launchable, permissions) {
  if (!/^r?x?$/.test(permissions)) {
    throw new TypeError("packages only support read and execute permissions");
  }
  target = target.replace(/\/+$/, "");
  const bundle = launchable.bundle;
  if (bundle == null) {
    return op_vfs_mount(target || "/", { type: "package", source: launchable.launchPath, name: null }, permissions);
  }
  const validName = name => typeof name === "string" && name.length > 0
    && name !== "." && name !== ".." && !/[\\/:]/.test(name);
  if (!validName(bundle.entrypoint) || !Array.isArray(bundle.files)
    || !bundle.files.some(file => file.name === bundle.entrypoint)) {
    throw new TypeError("invalid package entrypoint");
  }
  const names = new Set();
  for (const file of bundle.files) {
    if (!validName(file.name) || names.has(file.name)
      || (typeof file.path === "string") === (typeof file.text === "string")) {
      throw new TypeError("invalid package file binding");
    }
    names.add(file.name);
  }
  for (const file of bundle.files) {
    if (file.path != null) op_vfs_mount(target || "/", { type: "package", source: file.path, name: file.name }, permissions);
    else filesystem.mount(`${target}/${file.name}`, new TextEncoder().encode(file.text), { mode: permissions });
  }
  return filesystem.path(`${target}/${bundle.entrypoint}`);
}

function refreshConfigurations() {
  for (const mount of configurationMounts) {
    const rendered = mount.configuration.render();
    const names = Object.keys(rendered).sort();
    if (names.length !== mount.names.length || names.some((name, index) => name !== mount.names[index])) {
      throw new TypeError("a mounted configuration cannot change its rendered filename set");
    }
    for (const name of names) {
      filesystem.mount(`${mount.root}/${name}`, rendered[name], { mode: mount.permissions });
    }
  }
}

Object.defineProperty(globalThis, "__polymerFilesystem", {
  value: Object.freeze({
    filesystem,
    fileResource: readableResource,
    bindPackage(value, launchPath) {
      if (value != null) packages.set(value, { launchPath, bundle: structuredClone(value.bundle) });
      return value;
    },
    cacheResource(cacheKey, methods) {
      return readableResource(op_vfs_cache_resource(String(cacheKey)), false, methods);
    },
    importResource(source) {
      return readableResource(op_vfs_import_package(String(source)));
    },
    storage: storageResources,
    quickResume,
    createSaveStateDirectory() {
      return readableResource(op_vfs_scratch_resource(), true);
    },
    async returnedFile(value) {
      if (!saveStateEntries.has(value) || value?.kind !== "file") {
        throw new TypeError("onQuit must return a FileEntry from the managed save-state directory");
      }
      return value.bytes();
    },
    flush: op_vfs_flush,
    hasConfigurationMounts: () => configurationMounts.length !== 0,
    refreshConfigurations,
  }),
  configurable: false,
  enumerable: false,
  writable: false,
});
