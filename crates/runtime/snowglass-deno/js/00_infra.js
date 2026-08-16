const surfaceAttachments = new WeakMap();

export function registerSurface(surface, attachment) {
  if ((typeof surface !== "object" && typeof surface !== "function") || surface === null) {
    throw new TypeError("A Snowglass surface attachment must be an object");
  }
  attachment = Number(attachment);
  if (!Number.isSafeInteger(attachment) || attachment < 0) {
    throw new TypeError("A Snowglass surface attachment ID must be a non-negative safe integer");
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
