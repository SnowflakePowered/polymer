deno_core::extension!(
    snowglass_emulator,
    esm_entry_point = "ext:snowglass_emulator/00_emulator.js",
    esm = [dir "emulator/js", "00_emulator.js"],
);

pub(crate) fn extension() -> deno_core::Extension {
    snowglass_emulator::init()
}
