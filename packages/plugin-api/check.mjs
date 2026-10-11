import { spawnSync } from "node:child_process";
import { existsSync, readdirSync } from "node:fs";
import { join } from "node:path";

const plugins = join(import.meta.dirname, "..", "..", "plugins");
const configs = readdirSync(plugins).map(name => join(plugins, name, "tsconfig.json")).filter(existsSync);
const tsc = join(import.meta.dirname, "node_modules", ".bin", process.platform === "win32" ? "tsc.cmd" : "tsc");
let failed = 0;
for (const config of configs) {
  const result = spawnSync(tsc, ["-p", config, "--pretty", "false"], { stdio: "inherit", shell: process.platform === "win32" });
  if (result.status !== 0) failed++;
}
console.log(`${configs.length - failed} of ${configs.length} plugins type-check`);
process.exit(failed ? 1 : 0);
