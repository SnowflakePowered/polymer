deno_core::extension!(
    polymer_scraper,
    esm_entry_point = "ext:polymer_scraper/00_scraper.js",
    esm = [dir "scraper/js", "00_scraper.js"],
);

pub(crate) fn extension() -> deno_core::Extension {
    polymer_scraper::init()
}
