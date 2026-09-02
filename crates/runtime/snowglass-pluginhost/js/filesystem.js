// Plugin-worker filesystem API. This is intentionally separate from Snowglass.*.
import {
  op_vfs_mount_cache,
  op_vfs_mount_host_file,
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
  op_vfs_resource_entries,
  op_vfs_resources,
  op_vfs_storage,
  op_vfs_set_profile,
  op_vfs_set_working_directory,
  op_vfs_flush,
} from "ext:core/ops";

const handles = new WeakMap();
const storageHandles = new WeakSet();
const configurationMounts = [];

function resource(description) {
  if (description == null) return undefined;
  const value = Object.freeze({ kind: description.kind, path: description.path });
  handles.set(value, description);
  return value;
}

function readableEntry(description, path, entry, root = false) {
  const current = root ? path : [...path, entry.name];
  if (entry.kind === "directory") {
    return Object.freeze({
      name: entry.name,
      kind: "directory",
      async *[Symbol.asyncIterator]() {
        for (const child of op_vfs_resource_entries(description.kind, description.handle, current)) {
          yield readableEntry(description, current, child);
        }
      },
    });
  }
  return Object.freeze({
    name: entry.name,
    kind: "file",
    async bytes() {
      return op_vfs_resource_bytes(description.kind, description.handle, current);
    },
    async text() {
      return new TextDecoder().decode(op_vfs_resource_bytes(description.kind, description.handle, current));
    },
  });
}

function readableResource(description) {
  const value = readableEntry(description, [], {
    name: description.name,
    kind: description.kind === "tree" ? "directory" : "file",
  }, true);
  handles.set(value, description);
  return value;
}

const namedResources = Object.create(null);
for (const description of op_vfs_resources()) {
  Object.defineProperty(namedResources, description.name, {
    value: readableResource(description), enumerable: true, writable: false,
  });
}
Object.freeze(namedResources);

const storageResources = Object.create(null);
for (const description of op_vfs_storage()) {
  const value = resource(description);
  storageHandles.add(value);
  Object.defineProperty(storageResources, description.name, {
    value, enumerable: true, writable: false,
  });
}
Object.freeze(storageResources);

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
  mountCache(cacheKey) {
    return resource(op_vfs_mount_cache(String(cacheKey)));
  },

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

Object.defineProperty(globalThis, "__snowglassFilesystem", {
  value: Object.freeze({
    filesystem,
    resources: namedResources,
    importResource(name) {
      if (!Object.hasOwn(namedResources, name)) {
        throw new TypeError(`resource '${name}' is not declared by this plugin`);
      }
      return namedResources[name];
    },
    storage: storageResources,
    flush: op_vfs_flush,
    hasConfigurationMounts: () => configurationMounts.length !== 0,
    refreshConfigurations,
  }),
  configurable: false,
  enumerable: false,
  writable: false,
});
