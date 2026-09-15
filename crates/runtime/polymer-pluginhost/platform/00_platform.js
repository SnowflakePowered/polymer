import { op_platform_resource, op_platform_install } from "ext:core/ops";
import { cacheFileLocation } from "ext:polymer_cache/00_cache.js";

async function bootstrap(receiver, descriptor, context = {}) {
  let pending = Promise.resolve();
  const device = Object.freeze({
    ...descriptor,
    install(source) {
      pending = pending.then(() => {
        if (typeof source === "string") return op_platform_install({ path: source });
        const { key, path } = cacheFileLocation(source);
        return op_platform_install({ cache: key, path });
      });
      return pending;
    },
  });
  context = Object.freeze({ ...context, firmwareStore: __polymerFirmware.store });
  let result = await receiver.onBootstrap(device, context);
  if (result?.[Symbol.asyncIterator]) {
    for await (const progress of result) context.progress(progress);
    result = undefined;
  }
  await pending;
  return result;
}

Object.defineProperty(globalThis, "__polymerPlatform", { value: Object.freeze({
  bootstrap,
  resources: Object.freeze({ inspect: (path) => op_platform_resource(String(path)) }),
}) });
