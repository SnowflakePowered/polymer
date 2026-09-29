use super::FrontendServices;
use polymer_scraping::Package;
use serde_json::Value;

impl FrontendServices {
    /// Bind only launch entries, leaving configuration and other plugin data alone.
    pub(super) async fn prepare_launch_packages(
        mut arguments: Vec<Value>,
    ) -> Result<Vec<Value>, String> {
        tokio::task::spawn_blocking(move || {
            let Some(argument) = arguments.first_mut() else {
                return Ok(arguments);
            };
            let argument = argument
                .as_object_mut()
                .ok_or("onLaunch requires an argument object")?;
            if argument.contains_key("launchSet") {
                let index = argument
                    .get("initialDiscIndex")
                    .map_or(Some(0), Value::as_u64)
                    .and_then(|index| usize::try_from(index).ok())
                    .ok_or("initialDiscIndex must be a nonnegative integer")?;
                let discs = argument
                    .get_mut("launchSet")
                    .and_then(Value::as_array_mut)
                    .ok_or("launchSet must be an array")?;
                if index >= discs.len() {
                    return Err("initialDiscIndex is outside the launch set".to_owned());
                }
                for disc in discs.iter_mut() {
                    Self::prepare_launch_entry(disc)?;
                }
                let selected = discs[index].clone();
                argument.insert("launchPath".into(), selected["launchPath"].clone());
                argument.insert("package".into(), selected["package"].clone());
            } else {
                Self::prepare_launch_entry(&mut arguments[0])?;
            }
            Ok(arguments)
        })
        .await
        .map_err(|error| error.to_string())?
    }

    fn prepare_launch_entry(entry: &mut Value) -> Result<(), String> {
        let Some(value) = entry
            .get("package")
            .filter(|value| value.get("uri").is_some())
        else {
            return Ok(());
        };
        let mut package: Package =
            serde_json::from_value(value.clone()).map_err(|error| error.to_string())?;
        package
            .prepare_launch()
            .map_err(|error| error.to_string())?;
        if let (Some(entry), Value::Object(launch)) =
            (entry.as_object_mut(), package.launch_entry())
        {
            entry.extend(launch);
        }
        Ok(())
    }
}
