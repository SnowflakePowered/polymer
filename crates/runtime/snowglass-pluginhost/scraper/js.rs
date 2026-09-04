deno_core::extension!(
    snowglass_scraper,
    esm_entry_point = "ext:snowglass_scraper/00_scraper.js",
    esm = [dir "scraper/js", "00_scraper.js"],
);

pub(crate) fn extension() -> deno_core::Extension {
    snowglass_scraper::init()
}
