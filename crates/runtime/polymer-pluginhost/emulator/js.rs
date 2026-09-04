deno_core::extension!(
    polymer_emulator,
    esm_entry_point = "ext:polymer_emulator/00_emulator.js",
    esm = [dir "emulator/js", "00_emulator.js"],
);

pub(crate) fn extension() -> deno_core::Extension {
    polymer_emulator::init()
}
