mod navigation;
mod plugin_list;
mod theme_picker;

use eframe::egui::{self, Align2, Color32, FontId, Frame, Margin, Sense, Stroke, Vec2};
use navigation::{GamepadNavigation, NavAction};
use plugin_list::PluginList;
use std::{cell::RefCell, error::Error, rc::Rc};
use theme_picker::ThemePicker;

const WINDOW_SIZE: [f32; 2] = [380.0, 520.0];
const WINDOW_PADDING: f32 = 16.0;
const TAB_HEIGHT: f32 = 32.0;
const ACCENT: Color32 = Color32::from_rgb(120, 170, 255);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Themes,
    Plugins,
}

impl Tab {
    const ALL: [Tab; 2] = [Tab::Themes, Tab::Plugins];

    fn title(self) -> &'static str {
        match self {
            Tab::Themes => "Themes",
            Tab::Plugins => "Plugins",
        }
    }

    fn shifted(self, offset: isize) -> Self {
        let index = Self::ALL.iter().position(|tab| *tab == self).unwrap_or(0) as isize;
        Self::ALL[(index + offset).rem_euclid(Self::ALL.len() as isize) as usize]
    }

    fn bar(&mut self, ui: &mut egui::Ui) {
        let width = ui.available_width() / Self::ALL.len() as f32;
        let (strip, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), TAB_HEIGHT), Sense::hover());
        let painter = ui.painter();
        painter.hline(
            strip.x_range(),
            strip.bottom(),
            Stroke::new(1.0_f32, Color32::from_white_alpha(30)),
        );
        for (index, tab) in Self::ALL.into_iter().enumerate() {
            let rect = egui::Rect::from_min_size(
                strip.min + Vec2::new(width * index as f32, 0.0),
                Vec2::new(width, TAB_HEIGHT),
            );
            let response = ui.interact(rect, ui.id().with(tab.title()), Sense::click());
            let active = *self == tab;
            let color = match (active, response.hovered()) {
                (true, _) => Color32::WHITE,
                (false, true) => Color32::from_white_alpha(200),
                (false, false) => Color32::from_white_alpha(130),
            };
            if active {
                ui.painter().rect_filled(rect, 0.0, Color32::from_white_alpha(10));
                ui.painter().hline(
                    rect.x_range(),
                    rect.bottom() - 1.0,
                    Stroke::new(2.5_f32, ACCENT),
                );
            }
            ui.painter().text(
                rect.center(),
                Align2::CENTER_CENTER,
                tab.title(),
                FontId::proportional(14.0),
                color,
            );
            if response.clicked() {
                *self = tab;
            }
        }
    }
}

pub(crate) struct BaseUi {
    tab: Tab,
    themes: ThemePicker,
    plugins: PluginList,
    chosen: Rc<RefCell<Option<String>>>,
    gamepads: Option<GamepadNavigation>,
}

impl BaseUi {
    pub(crate) fn run(runtime: &tokio::runtime::Runtime) -> Result<(), Box<dyn Error>> {
        let directories = polymer_library::Directories::discover()?;
        let registry = crate::cli::ShellArguments::discover_plugins(&directories)?;
        let storage = runtime.block_on(polymer_deno::webstorage::LocalStorage::open(&directories))?;
        let current = runtime
            .block_on(storage.get_item(crate::theme::STORAGE_KEY))?
            .unwrap_or_default();
        let chosen = Rc::new(RefCell::new(None));
        let app_chosen = chosen.clone();
        let options = eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_title("Polymer")
                .with_inner_size(WINDOW_SIZE)
                .with_resizable(false)
                .with_maximize_button(false)
                .with_transparent(true),
            ..Default::default()
        };
        eframe::run_native(
            "Polymer",
            options,
            Box::new(move |creation| {
                #[cfg(windows)]
                if let Err(error) = window_vibrancy::apply_mica(creation, Some(true)) {
                    tracing::warn!(%error, "polymer: could not apply the Mica backdrop");
                }
                #[cfg(target_os = "macos")]
                if let Err(error) = window_vibrancy::apply_vibrancy(
                    creation,
                    window_vibrancy::NSVisualEffectMaterial::HudWindow,
                    None,
                    None,
                ) {
                    tracing::warn!(%error, "polymer: could not apply the window vibrancy");
                }
                creation.egui_ctx.set_visuals(egui::Visuals::dark());
                Ok(Box::new(Self {
                    tab: Tab::Themes,
                    themes: ThemePicker::new(&registry, current),
                    plugins: PluginList::new(&registry),
                    chosen: app_chosen,
                    gamepads: GamepadNavigation::new(),
                }))
            }),
        )
        .map_err(|error| error.to_string())?;
        let Some(theme) = chosen.take() else {
            return Ok(());
        };
        runtime.block_on(storage.set_item(crate::theme::STORAGE_KEY, &theme))?;
        std::process::Command::new(std::env::current_exe()?)
            .args(std::env::args_os().skip(1).filter(|argument| argument != "--pick-theme"))
            .spawn()?;
        Ok(())
    }
}

impl eframe::App for BaseUi {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0; 4]
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let mut actions = ctx.input(NavAction::from_keyboard);
        if let Some(gamepads) = &mut self.gamepads {
            actions.extend(gamepads.poll());
            ctx.request_repaint_after(std::time::Duration::from_millis(16));
        }
        for action in &actions {
            match action {
                NavAction::PrevTab => self.tab = self.tab.shifted(-1),
                NavAction::NextTab => self.tab = self.tab.shifted(1),
                NavAction::Cancel => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
                _ => {}
            }
        }
        egui::CentralPanel::default()
            .frame(Frame::none().inner_margin(Margin::same(WINDOW_PADDING)))
            .show(ctx, |ui| {
                self.tab.bar(ui);
                ui.add_space(8.0);
                egui::ScrollArea::vertical().show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 8.0;
                    match self.tab {
                        Tab::Themes => {
                            if let Some(theme) = self.themes.show(ui, &actions) {
                                *self.chosen.borrow_mut() = Some(theme);
                                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                            }
                        }
                        Tab::Plugins => {
                            self.plugins.show(ui);
                            for action in &actions {
                                match action {
                                    NavAction::Up => ui.scroll_with_delta(Vec2::new(0.0, 80.0)),
                                    NavAction::Down => ui.scroll_with_delta(Vec2::new(0.0, -80.0)),
                                    _ => {}
                                }
                            }
                        }
                    }
                });
            });
    }
}
