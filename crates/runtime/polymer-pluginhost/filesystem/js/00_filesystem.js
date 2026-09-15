// Plugin-worker filesystem API. This is intentionally separate from Polymer.*.
import {
  op_vfs_cache_resource,
  op_vfs_mount_host_file,
  op_vfs_mount_package_source,
  op_vfs_mount_host_tree,
  op_vfs_mount_file_bytes,
  op_vfs_mount_file_pkg,
  op_vfs_mount_file_resource,
  op_vfs_mount_file_text,
  op_vfs_mount_scratch,
  op_vfs_mount_storage,
  op_vfs_mount_tree_pkg,
  op_vfs_mount_tree_resource,
  op_vfs_path,
  op_vfs_resource_bytes,
  op_digest,
  op_vfs_resource_entries,
  op_vfs_resource_remove,
  op_vfs_scratch_resource,
  op_vfs_import_package,
  op_vfs_storage,
  op_vfs_quick_resume,
  op_vfs_set_profile,
  op_vfs_set_working_directory,
  op_vfs_flush,
} from "ext:core/ops";

const handles = new WeakMap();
const saveStateEntries = new WeakSet();
const storageHandles = new WeakSet();
const configurationMounts = [];

function resource(description, methods = {}) {
  if (description == null) return undefined;
  const value = Object.freeze({ ...methods, kind: description.kind, path: description.path });
  handles.set(value, description);
  return value;
}

function readableEntry(description, path, entry, root = false, saveState = false) {
  const current = root ? path : [...path, entry.name];
  if (entry.kind === "directory") {
    const value = Object.freeze({
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

function readableResource(description, saveState = false) {
  const value = readableEntry(description, [], {
    name: description.name,
    kind: description.kind === "tree" ? "directory" : "file",
  }, true, saveState);
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

function pkgSource(value) {
  if (value instanceof URL) return value.href;
  return value;
}

const filesystem = Object.freeze({

  mountHostTree(target, source, options) {
    op_vfs_mount_host_tree(String(target), String(source), mode(options));
  },

  mountHostFile(target, source, options) {
    op_vfs_mount_host_file(String(target), String(source), mode(options));
  },

  mountTree(target, source, options) {
    target = String(target);
    const permissions = mode(options);
    const handle = handles.get(source);
    if (handle !== undefined) {
      op_vfs_mount_tree_resource(target, handle.kind, handle.handle, permissions);
      return;
    }
    source = pkgSource(source);
    if (typeof source !== "string" || !source.startsWith("pkg:")) {
      throw new TypeError("mountTree source must be a tree resource or pkg: string/URL");
    }
    op_vfs_mount_tree_pkg(target, source, permissions);
  },

  mountFile(target, source, options) {
    target = String(target);
    const permissions = mode(options);
    const handle = handles.get(source);
    if (handle !== undefined) {
      if (handle.components?.length) {
        throw new TypeError("mountFile does not yet support child file entries");
      }
      op_vfs_mount_file_resource(target, handle.kind, handle.handle, permissions);
      return;
    }
    if (source instanceof URL) source = source.href;
    if (source instanceof Uint8Array) {
      op_vfs_mount_file_bytes(target, source, permissions);
    } else if (typeof source === "string" && source.startsWith("pkg:")) {
      op_vfs_mount_file_pkg(target, source, permissions);
    } else if (typeof source === "string") {
      op_vfs_mount_file_text(target, source, permissions);
    } else {
      throw new TypeError("mountFile source must be text, Uint8Array, file resource, or pkg: string/URL");
    }
  },

  mountConfiguration(root, configuration, options) {
    if (configuration == null || typeof configuration.render !== "function") {
      throw new TypeError("mountConfiguration requires a configuration");
    }
    root = String(root).replace(/\/+$/, "");
    const permissions = mode(options);
    const rendered = configuration.render();
    const names = Object.keys(rendered).sort();
    for (const name of names) {
      filesystem.mountFile(`${root}/${name}`, rendered[name], { mode: permissions });
    }
    configurationMounts.push({ root, configuration, permissions, names });
  },

  flush() {
    refreshConfigurations();
    op_vfs_flush();
  },

  mountStorage(target, source, options) {
    target = String(target);
    const permissions = mode(options);
    if (!storageHandles.has(source)) {
      throw new TypeError("mountStorage source must be a storage resource");
    }
    const handle = handles.get(source);
    const format = options.format;
    if (format !== undefined && typeof format !== "string") {
      throw new TypeError("mountStorage format must be a string");
    }
    op_vfs_mount_storage(target, handle.kind, handle.handle, {
      mode: permissions,
      ...(format === undefined ? {} : { format }),
    });
  },

  mountScratch(target, options) {
    if (options == null || typeof options.backing !== "string") {
      throw new TypeError("mountScratch requires a backing");
    }
    op_vfs_mount_scratch(String(target), mode(options), options.backing);
  },

  path(path) { return op_vfs_path(String(path)); },
  setWorkingDirectory(path) { op_vfs_set_working_directory(String(path)); },
  setProfile(profile) { op_vfs_set_profile(profile ?? {}); },
});

// Each callback receives a filesystem whose package entry is captured by the host.
function forLaunch(launchable) {
  return Object.freeze({
    ...filesystem,
    // Mount a host-prepared package and return its emulator-visible entrypoint.
    mountPackage(target) {
      target = String(target).replace(/\/+$/, "");
      const bundle = launchable.package?.bundle;
      if (bundle == null) {
        return op_vfs_mount_package_source(target, String(launchable.launchPath));
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
        if (file.path != null) filesystem.mountHostFile(`${target}/${file.name}`, file.path, { mode: "r" });
        else filesystem.mountFile(`${target}/${file.name}`, file.text, { mode: "r" });
      }
      return filesystem.path(`${target}/${bundle.entrypoint}`);
    },
  });
}

function refreshConfigurations() {
  for (const mount of configurationMounts) {
    const rendered = mount.configuration.render();
    const names = Object.keys(rendered).sort();
    if (names.length !== mount.names.length || names.some((name, index) => name !== mount.names[index])) {
      throw new TypeError("a mounted configuration cannot change its rendered filename set");
    }
    for (const name of names) {
      filesystem.mountFile(`${mount.root}/${name}`, rendered[name], { mode: mount.permissions });
    }
  }
}

Object.defineProperty(globalThis, "__polymerFilesystem", {
  value: Object.freeze({
    filesystem,
    fileResource: readableResource,
    forLaunch,
    cacheResource(cacheKey, methods) {
      return resource(op_vfs_cache_resource(String(cacheKey)), methods);
    },
    importResource(source) {
      return readableResource(op_vfs_import_package(String(source)));
    },
    storage: storageResources,
    quickResume,
    createSaveStateDirectory() {
      return readableResource(op_vfs_scratch_resource("temporary"), true);
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
