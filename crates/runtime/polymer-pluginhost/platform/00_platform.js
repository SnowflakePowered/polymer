import { op_platform_resource, op_platform_install } from "ext:core/ops";

async function bootstrap(receiver, descriptor, context = {}) {
  if (typeof receiver.onPreflight !== "function") throw new TypeError("Platform must export onPreflight");
  let pending = Promise.resolve();
  const device = Object.freeze({
    ...descriptor,
    install(source) {
      pending = pending.then(() => op_platform_install(source));
      return pending;
    },
  });
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
