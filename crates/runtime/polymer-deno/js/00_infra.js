const surfaceAttachments = new WeakMap();
const fileSystemPaths = new WeakMap();

export function registerFileSystemHandle(handle, path) {
  fileSystemPaths.set(handle, String(path));
}

/** The host path of a picked file system handle, or a path given directly. */
export function hostPath(value, name) {
  if (typeof value === "string" && value.length) return value;
  const path = fileSystemPaths.get(value);
  if (path === undefined) throw new TypeError(`${name} must be a non-empty string or a FileSystemHandle`);
  return path;
}

export function registerSurface(surface, attachment) {
  if ((typeof surface !== "object" && typeof surface !== "function") || surface === null) {
    throw new TypeError("A Polymer surface attachment must be an object");
  }
  attachment = Number(attachment);
  if (!Number.isSafeInteger(attachment) || attachment < 0) {
    throw new TypeError("A Polymer surface attachment ID must be a non-negative safe integer");
  }
  surfaceAttachments.set(surface, attachment);
}

export function attachmentFor(surface) {
  const attachment = surfaceAttachments.get(surface);
  if (attachment === undefined) {
    throw new TypeError("Emulator surfaces must attach to a registered host surface");
  }
  return attachment;
}
