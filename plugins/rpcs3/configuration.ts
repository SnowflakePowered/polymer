import type { Configuration, Filesystem } from "polymer:api";

export class Rpcs3Configuration {
    static mount(configuration: Configuration, fs: Filesystem): void {
        const settings: Record<string, Record<string, unknown>> = {};
        const players: Record<string, Record<string, unknown>> = {};
        let section: Record<string, unknown> = {};
        // RPCS3 uses yaml-cpp, which also accepts JSON. Keep controller projection
        // in the host and convert its flat INI output to RPCS3's nested document.
        const ini = new TextDecoder().decode(configuration.render()["runtime.ini"]);
        for (const line of ini.split(/\r?\n/)) {
            const heading = /^\[([^\]]+)\]$/.exec(line.trim());
            if (heading) {
                section = settings[heading[1]] = {};
                continue;
            }
            const equals = line.indexOf("=");
            if (equals < 0) continue;
            const value = line.slice(equals + 1).trim();
            section[line.slice(0, equals).trim()] = value === "true" ? true : value === "false" ? false : value;
        }
        for (let port = 1; port <= 7; port++) {
            const { Handler = "Null", Device = "", ...Config } = settings[`Player${port}`] ?? {};
            players[`Player ${port} Input`] = {
                Handler, Device: Handler === "SDL" ? `Polymer Virtual Gamepad ${Number(Device) + 1}` : "", Config,
            };
            delete settings[`Player${port}`];
        }
        for (const [name, document] of Object.entries({
            "config.yml": settings,
            "config/input_configs/global/Default.yml": players,
        })) {
            fs.mount(`/RPCS3/${name}`, new TextEncoder().encode(JSON.stringify(document)), { mode: "rw" });
        }
    }
}
