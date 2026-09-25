# Windows emulator plugins

Install the pinned releases from the repository root:

```powershell
python plugins/support/install_runtimes.py
```

Pass plugin IDs to install a subset, for example `azahar melonds`. The installer
verifies the downloaded archives against `support/runtimes.json`, preserves
existing installations, and extracts into each plugin's ignored `bin/win-x64`
directory. Python 3.11+ and Windows `tar.exe` are required. Restart Polymer after
adding plugins so its registry discovers their manifests.

| Plugin | Release | Graphics / input | Storage and firmware |
| --- | --- | --- | --- |
| Azahar | Standalone MSVC 2126.1.1 | Vulkan / SDL2; separate top and bottom windows | Select a 3DS NAND and save repository for `ctrnand`. The composed volume supplies both `nand/` and `sdmc/`. |
| melonDS | 1.1 | OpenGL / SDL2 | Nintendo DS direct boot with internal BIOS; managed ROM battery saves. DSi is not advertised. |
| RPCS3 | 0.0.42-20032-3efbf060 | Vulkan / SDL3 | Select a PS3 NAND and save repository for `hdd`, with installed system software. An optional `flash` selection overrides the firmware partitions. Installed PKGs boot from the HDD's `USRDIR/EBOOT.BIN`; licenses come from the composed keystore layer. |
| Ymir | 0.3.3 AVX2 | D3D11 / SDL3 | Import a Saturn BIOS. Internal backup memory persists in the plugin's `saves` cache. Requires an AVX2-capable CPU. |
| ares | v148 | OpenGL / SDL3 | Uses the selected console's firmware store and v148 firmware hashes. PS1 card 1, Game Gear/Master System RAM and Neo Geo Pocket flash use managed storage when supplied. Other cartridge, RTC and system save files persist in the plugin's `saves` cache. |

Ares and Ymir's `saves` caches contain actual saves, not disposable shader data;
retain them when clearing caches. Ares's multiple save-file formats cannot all
be represented by the host's single-file battery storage. Ymir's internal backup
memory does not currently have a Polymer console-storage implementation.

Configuration definitions use Polymer's normal controller projection. melonDS
1.1 imports the generated legacy INI into a fresh portable TOML configuration.
Ymir uses TOML literals emitted through the INI serializer, with native table
names expanded by the launcher. RPCS3 converts projected INI settings to JSON,
which its YAML parser accepts. Native UI configuration remains launch-local.

These launchers do not advertise quick resume, save states, pause callbacks or
live controller-topology changes. `ARCHITECTURE.md` describes `onQuit` as mandatory,
but the current runtime treats it as optional, as do the existing standalone
plugins; these plugins use host-driven process shutdown.

Validation: TypeScript type checking, native startup probes of all five release
executables, native configuration readback for Azahar/melonDS/Ymir, and host
serialization of 60 platform/controller configurations (including unplugged
ports). Full game playback, graphics capture and controller input through Polymer
still require testing with compatible games and firmware.
