import { caches } from "polymer:cache";
import type { BootstrapContext, BootstrapDevice, PreflightRequest, PreflightResult } from "polymer:api";

const FIRMWARE_URL = "http://dus01.ps3.update.playstation.net/update/ps3/image/us/2026_0318_a2b60b6ac1d2e49e230144345616927c/PS3UPDAT.PUP";

export function onPreflight(request: PreflightRequest): PreflightResult {
  if (request.operation === "bootstrap" ||
      (request.operation === "install" && request.depot_kind === "system")) {
    return { issues: [] };
  }
  return {
    issues: Object.entries(request.devices).flatMap(([name, device]) =>
      device?.minimal ? [] : [{
        code: "NAND_INCOMPLETE",
        message: "Install PlayStation 3 system software before using this NAND.",
        severity: "blocking" as const,
        device: name,
      }]),
  };
}

export async function onBootstrap(device: BootstrapDevice, context: BootstrapContext): Promise<void> {
  const path = `${device.id}/PS3UPDAT.PUP`;
  const message = "Downloading PlayStation 3 system software";
  context.progress({ message, completed: 0 });
  const response = await fetch(FIRMWARE_URL);
  if (!response.ok || !response.body) {
    throw new Error(`PS3 firmware download failed: HTTP ${response.status}`);
  }
  const length = Number(response.headers.get("content-length"));
  const total = Number.isFinite(length) && length > 0 ? length : undefined;
  const file = caches.firmware.open(path, { read: false, write: true, truncate: true });
  if (!file.writable) throw new Error("PS3 firmware cache is not writable");
  let completed = 0;
  let reported = 0;
  await response.body.pipeThrough(new TransformStream<Uint8Array, Uint8Array>({
    transform(chunk, controller) {
      completed += chunk.byteLength;
      if (completed - reported >= 1024 * 1024) {
        context.progress({ message, completed, total: total && Math.max(total, completed) });
        reported = completed;
      }
      controller.enqueue(chunk);
    },
    flush() {
      if (completed === 0 || (total !== undefined && completed !== total)) {
        throw new Error("PS3 firmware download was incomplete");
      }
      context.progress({ message, completed, total: completed });
    },
  })).pipeTo(file.writable);
  context.progress({ message: "Checking and installing PlayStation 3 system software" });
  // The host detects System, runs onPreflight, then drives one native PUP installation.
  await device.install(file);
}
