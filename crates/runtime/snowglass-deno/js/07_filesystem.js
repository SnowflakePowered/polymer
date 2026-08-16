import {
  op_vfs_game,
  op_vfs_mount_file_bytes,
  op_vfs_mount_file_pkg,
  op_vfs_mount_file_resource,
  op_vfs_mount_file_text,
  op_vfs_mount_scratch,
  op_vfs_mount_tree_pkg,
  op_vfs_mount_tree_resource,
  op_vfs_path,
  op_vfs_resources,
  op_vfs_set_profile,
  op_vfs_set_working_directory,
} from "ext:core/ops";

const handles = new WeakMap();

function resource(description) {
  if (description == null) return undefined;
  const value = Object.freeze({ kind: description.kind });
  handles.set(value, description);
  return value;
}

const namedResources = Object.create(null);
for (const description of op_vfs_resources()) {
  Object.defineProperty(namedResources, description.name, {
    value: resource(description), enumerable: true, writable: false,
  });
}
Object.freeze(namedResources);

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

Object.defineProperty(globalThis, "__snowglassFilesystem", {
  value: Object.freeze({ filesystem, resources: namedResources, game: resource(op_vfs_game()) }),
  configurable: false,
  enumerable: false,
  writable: false,
});
