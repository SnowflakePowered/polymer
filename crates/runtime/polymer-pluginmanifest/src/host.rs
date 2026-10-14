use std::sync::LazyLock;

/// The plugin API versions this host provides. Themes are versioned against the frontend
/// API, and every other plugin class against the plugin API.
pub struct HostApi;

impl HostApi {
    fn package_version(package: &str) -> semver::Version {
        let version = serde_json::from_str::<serde_json::Value>(package)
            .ok()
            .and_then(|package| package.get("version")?.as_str().map(str::to_owned))
            .expect("API package manifests declare a version");
        semver::Version::parse(&version).expect("API package versions are semver")
    }

    /// The API version a plugin of `class` is checked against.
    pub fn for_class(class: &str) -> &'static semver::Version {
        static PLUGIN: LazyLock<semver::Version> = LazyLock::new(|| {
            HostApi::package_version(include_str!("../../../../packages/plugin-api/package.json"))
        });
        static FRONTEND: LazyLock<semver::Version> = LazyLock::new(|| {
            HostApi::package_version(include_str!("../../../../packages/frontend/package.json"))
        });
        if class == "theme" { &FRONTEND } else { &PLUGIN }
    }
}
