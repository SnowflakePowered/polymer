mod bluetooth;
mod driver;
mod launcher;
mod winfsp;
mod winusb;

pub(crate) use bluetooth::BluetoothPanel;

use super::{
    ACCENT, BaseUi, WINDOW_PADDING,
    navigation::{GamepadNavigation, NavAction},
};
use crate::cli::ShellArguments;
use eframe::egui::{self, Color32, FontId, Frame, Margin, RichText, Rounding, Stroke, Vec2};
use launcher::ElevatedInstall;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::{
    cell::Cell,
    error::Error,
    fs::{File, OpenOptions},
    io::Write,
    rc::Rc,
    sync::Arc,
};
use winfsp::{WinFspAction, WinFspInstaller};
use winusb::WinUsbInstaller;

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum InstallEvent {
    Action { description: String },
    Progress { fraction: f32 },
}

#[derive(Clone)]
enum SetupState {
    Ready,
    Installing {
        fraction: Option<f32>,
        action: String,
    },
    Failed(String),
    RestartRequired,
    Installed,
}

impl SetupState {
    fn apply(&mut self, event: InstallEvent) {
        let Self::Installing { fraction, action } = self else {
            return;
        };
        match event {
            InstallEvent::Action { description } => *action = description,
            InstallEvent::Progress { fraction: value } => *fraction = Some(value),
        }
    }

    fn show(&self, ui: &mut egui::Ui) {
        match self {
            Self::Installing { fraction, action } => {
                let bar = egui::ProgressBar::new(fraction.unwrap_or(0.0)).fill(ACCENT);
                ui.add(match fraction {
                    Some(_) => bar.show_percentage(),
                    None => bar.animate(true),
                });
                ui.label(RichText::new(action).color(Color32::from_white_alpha(170)));
            }
            Self::Failed(message) => {
                ui.label(RichText::new(message).color(Setup::ERROR));
            }
            Self::Ready | Self::RestartRequired | Self::Installed => {}
        }
    }
}

struct ProgressPipe(Option<File>);

impl ProgressPipe {
    fn name(parent: u32) -> String {
        format!(r"\\.\pipe\polymer-setup-{parent}")
    }

    fn connect(parent: u32) -> Self {
        Self(OpenOptions::new().write(true).open(Self::name(parent)).ok())
    }

    fn send(&mut self, event: &InstallEvent) {
        let Some(pipe) = &mut self.0 else {
            return;
        };
        let Ok(mut line) = serde_json::to_vec(event) else {
            return;
        };
        line.push(b'\n');
        if pipe.write_all(&line).is_err() {
            self.0 = None;
        }
    }
}

pub(crate) enum ElevatedTask {
    InstallWinFsp,
    InstallWinUsb { device_id: String },
    RemoveWinUsb { device_id: String },
}

impl ElevatedTask {
    pub(crate) fn run(self, parent: u32) -> i32 {
        match self {
            Self::InstallWinFsp => WinFspInstaller::run(parent),
            Self::InstallWinUsb { device_id } => WinUsbInstaller::run(parent, &device_id),
            Self::RemoveWinUsb { device_id } => WinUsbInstaller::remove(parent, &device_id),
        }
    }

    fn arguments(&self, parent: u32) -> String {
        match self {
            Self::InstallWinFsp => format!("--winfsp-installer {parent}"),
            Self::InstallWinUsb { device_id } => {
                format!("--winusb-installer {parent} --winusb-device \"{device_id}\"")
            }
            Self::RemoveWinUsb { device_id } => {
                format!("--winusb-uninstaller {parent} --winusb-device \"{device_id}\"")
            }
        }
    }

    fn outcome(&self, code: u32) -> SetupState {
        match self {
            Self::InstallWinFsp => WinFspInstaller::outcome(code),
            Self::InstallWinUsb { .. } => WinUsbInstaller::outcome(code),
            Self::RemoveWinUsb { .. } => WinUsbInstaller::removal_outcome(code),
        }
    }
}

struct ElevatedJob {
    state: Arc<Mutex<SetupState>>,
    runtime: tokio::runtime::Handle,
}

impl ElevatedJob {
    fn new(runtime: tokio::runtime::Handle) -> Self {
        Self {
            state: Arc::new(Mutex::new(SetupState::Ready)),
            runtime,
        }
    }

    fn state(&self) -> SetupState {
        self.state.lock().clone()
    }

    fn busy(&self) -> bool {
        matches!(*self.state.lock(), SetupState::Installing { .. })
    }

    fn reset(&self) {
        *self.state.lock() = SetupState::Ready;
    }

    fn start(&self, ctx: &egui::Context, task: ElevatedTask) {
        *self.state.lock() = SetupState::Installing {
            fraction: None,
            action: "Waiting for administrator permission".to_owned(),
        };
        let state = self.state.clone();
        let ctx = ctx.clone();
        self.runtime.spawn(async move {
            let result = match ElevatedInstall::run(&task, &state, &ctx).await {
                Ok(code) => task.outcome(code),
                Err(message) => SetupState::Failed(message),
            };
            *state.lock() = result;
            ctx.request_repaint();
        });
    }
}

pub(crate) struct Setup {
    job: ElevatedJob,
    winfsp_installed: bool,
    gamepads: Option<GamepadNavigation>,
    bluetooth: Option<BluetoothPanel>,
    finished: Rc<Cell<bool>>,
}

impl Setup {
    const ERROR: Color32 = Color32::from_rgb(255, 140, 130);
    const DEPENDENCIES_FLAG: &str = "--install-dependencies";

    pub(crate) fn required() -> bool {
        ShellArguments::dependencies_requested() || !WinFspInstaller::installed()
    }

    pub(crate) fn run(runtime: &tokio::runtime::Runtime) -> Result<(), Box<dyn Error>> {
        let finished = Rc::new(Cell::new(false));
        let app_finished = finished.clone();
        let handle = runtime.handle().clone();
        eframe::run_native(
            "Polymer",
            BaseUi::window_options(),
            Box::new(move |creation| {
                BaseUi::apply_backdrop(creation);
                Ok(Box::new(Self {
                    job: ElevatedJob::new(handle),
                    winfsp_installed: WinFspInstaller::installed(),
                    gamepads: GamepadNavigation::new(),
                    bluetooth: None,
                    finished: app_finished,
                }))
            }),
        )
        .map_err(|error| error.to_string())?;
        if finished.get() {
            std::process::Command::new(std::env::current_exe()?)
                .args(
                    std::env::args_os()
                        .skip(1)
                        .filter(|argument| argument != Self::DEPENDENCIES_FLAG),
                )
                .spawn()?;
        }
        Ok(())
    }

    fn enter_bluetooth(&mut self) {
        self.job.reset();
        self.bluetooth = Some(BluetoothPanel::new(self.job.runtime.clone(), true));
        self.finished.set(true);
    }

    fn busy(&self) -> bool {
        self.job.busy() || self.bluetooth.as_ref().is_some_and(BluetoothPanel::busy)
    }

    fn show_step(&mut self, ui: &mut egui::Ui, actions: &[NavAction]) {
        let title = match self.bluetooth {
            None => "Set up Polymer",
            Some(_) => "Bluetooth adapter",
        };
        ui.label(
            RichText::new(title)
                .font(FontId::proportional(22.0))
                .color(Color32::WHITE),
        );
        if let Some(bluetooth) = &mut self.bluetooth {
            if bluetooth.show(ui, actions) {
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
            }
            return;
        }
        let state = self.job.state();
        let confirm = actions.contains(&NavAction::Confirm);
        match WinFspInstaller::show(ui, &state, confirm, self.winfsp_installed) {
            Some(WinFspAction::Install) => self.job.start(ui.ctx(), ElevatedTask::InstallWinFsp),
            Some(WinFspAction::Continue) => self.enter_bluetooth(),
            Some(WinFspAction::Close) => ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close),
            None => {}
        }
    }

    fn button(ui: &mut egui::Ui, label: &str, enabled: bool, focused: bool) -> bool {
        let stroke = if focused {
            Stroke::new(2.0_f32, Color32::WHITE)
        } else {
            Stroke::new(1.5_f32, ACCENT)
        };
        let button = egui::Button::new(RichText::new(label).size(15.0).color(Color32::WHITE))
            .fill(ACCENT.gamma_multiply(0.6))
            .stroke(stroke)
            .rounding(Rounding::same(8.0))
            .min_size(Vec2::new(ui.available_width(), 40.0));
        ui.add_enabled(enabled, button).clicked()
    }

    fn notice(ui: &mut egui::Ui, notice: &str, link: &str) {
        Frame::none()
            .fill(Color32::from_white_alpha(16))
            .rounding(Rounding::same(10.0))
            .inner_margin(Margin::same(12.0))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(
                    RichText::new(notice)
                        .size(12.0)
                        .color(Color32::from_white_alpha(190)),
                );
                ui.hyperlink_to(RichText::new(link).size(12.0), link);
            });
    }
}

impl eframe::App for Setup {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0; 4]
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let mut actions = ctx.input(NavAction::from_keyboard);
        if let Some(gamepads) = &mut self.gamepads {
            actions.extend(gamepads.poll());
            ctx.request_repaint_after(std::time::Duration::from_millis(16));
        }
        if self.bluetooth.is_none() && matches!(self.job.state(), SetupState::Installed) {
            self.enter_bluetooth();
        }
        if !self.busy() && actions.contains(&NavAction::Cancel) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        egui::CentralPanel::default()
            .frame(Frame::none().inner_margin(Margin::same(WINDOW_PADDING)))
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 12.0;
                    self.show_step(ui, &actions);
                });
            });
    }
}
