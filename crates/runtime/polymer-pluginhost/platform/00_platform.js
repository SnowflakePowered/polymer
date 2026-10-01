import { op_platform_resource, op_platform_install, op_platform_depot } from "ext:core/ops";
import { cacheFileLocation } from "ext:polymer_cache/00_cache.js";
import { ambientJob } from "ext:polymer_plugin_runtime/01_jobs.js";

// Installs run in order; `settled` resolves once every install requested so far has.
function platformDevice(descriptor, installable) {
  let pending = Promise.resolve();
  const device = Object.freeze({
    ...descriptor,
    install(source) {
      if (!installable) throw new TypeError("device.install() is unavailable while checking for updates");
      const parent = ambientJob();
      pending = pending.then(async () => {
        if (typeof source === "string") return op_platform_install(source, null, await parent);
        const { key, path } = cacheFileLocation(source);
        return op_platform_install(path, key, await parent);
      });
      return pending;
    },
  });
  return { device, settled: () => pending };
}

function platformContext(context) {
  return Object.freeze({ ...context, firmwareStore: __polymerFirmware.store, keyStore: __polymerKeys.writer });
}

async function bootstrap(receiver, descriptor, context) {
  const { device, settled } = platformDevice(descriptor, true);
  context = platformContext(context);
  const result = await receiver.onBootstrap(device, context);
  await settled();
  return result;
}

async function checkUpdates(receiver, descriptor, title, context) {
  const { device } = platformDevice(descriptor, false);
  const candidates = [];
  for await (const candidate of receiver.onCheckUpdates(title, device, platformContext(context))) candidates.push(candidate);
  return candidates;
}

async function fetchUpdate(receiver, candidate, context) {
  const { key, path } = cacheFileLocation(await receiver.onFetchUpdate(candidate, platformContext(context)));
  return op_platform_depot(path, key);
}

Object.defineProperty(globalThis, "__polymerPlatform", { value: Object.freeze({
  bootstrap,
  checkUpdates,
  fetchUpdate,
  resources: Object.freeze({ inspect: (path) => op_platform_resource(String(path)) }),
}) });
