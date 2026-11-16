use eframe::egui::{self, Color32, Frame, Margin, RichText, Rounding};
use polymer_pluginhost::{loading::PluginRegistry, plugin::PluginInstance};
use std::collections::BTreeMap;

struct PluginRow {
    name: String,
    id: String,
    version: String,
    author: Option<String>,
}

pub(super) struct PluginList {
    groups: BTreeMap<String, Vec<PluginRow>>,
}

impl PluginList {
    pub(super) fn new(plugins: &PluginRegistry) -> Self {
        let mut groups: BTreeMap<String, Vec<PluginRow>> = BTreeMap::new();
        for plugin in plugins.plugins::<dyn PluginInstance>() {
            let meta = plugin.meta();
            groups
                .entry(plugin.class().to_owned())
                .or_default()
                .push(PluginRow {
                    name: meta.name.clone().unwrap_or_else(|| plugin.id().to_owned()),
                    id: plugin.id().to_owned(),
                    version: plugin.version().to_owned(),
                    author: meta.author.clone(),
                });
        }
        Self { groups }
    }

    pub(super) fn show(&self, ui: &mut egui::Ui) {
        for (class, rows) in &self.groups {
            ui.label(
                RichText::new(format!("{} ({})", class.to_uppercase(), rows.len()))
                    .size(12.0)
                    .color(Color32::from_white_alpha(150)),
            );
            for row in rows {
                Frame::none()
                    .fill(Color32::from_white_alpha(16))
                    .rounding(Rounding::same(8.0))
                    .inner_margin(Margin::symmetric(10.0, 6.0))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.label(RichText::new(&row.name).size(14.0).color(Color32::WHITE));
                        let author = row
                            .author
                            .as_deref()
                            .map_or(String::new(), |author| format!(" · by {author}"));
                        ui.label(
                            RichText::new(format!("{} · v{}{author}", row.id, row.version))
                                .size(11.0)
                                .color(Color32::from_white_alpha(130)),
                        );
                    });
            }
            ui.add_space(8.0);
        }
    }
}
