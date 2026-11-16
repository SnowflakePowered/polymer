use super::{
    ACCENT, ElevatedJob, ElevatedTask, NavAction, Setup, SetupState,
    driver::{DeviceNode, DriverPackage},
    winusb::{DeviceList, WinUsbInstaller},
};
use eframe::egui::{
    self, Align, Color32, CursorIcon, FontId, Frame, Margin, RichText, Rounding, Sense, Shape,
    Stroke, Vec2, pos2, text::LayoutJob,
};
use libwdi_sys::wdi_device_info;
use std::{
    ffi::{CStr, c_char},
    time::{Duration, Instant},
};
use windows::Devices::Radios::{Radio, RadioKind, RadioState};

const LIBWDI_NOTICE: &str = "libwdi - Windows Driver Installer library, Copyright (c) Pete Batard";
const LIBWDI_REPOSITORY: &str = "https://github.com/pbatard/libwdi";
const BLUETOOTH_CLASS: &str = "CLASS_E0&SUBCLASS_01&PROT_01";
const ASSIGN_LABEL: &str = "Assign Bluetooth Adapter to Polymer";
const REFRESH_INTERVAL: Duration = Duration::from_secs(2);
const TILE_SIZE: Vec2 = Vec2::new(104.0, 116.0);
const READY: Color32 = Color32::from_rgb(120, 220, 150);

pub(super) struct BluetoothAdapter {
    pub(super) name: String,
    driver: String,
    pub(super) device_id: String,
    pub(super) composite: bool,
    pub(super) package: Option<DriverPackage>,
}

impl BluetoothAdapter {
    pub(super) fn list() -> Result<Vec<Self>, i32> {
        Ok(DeviceList::create()?
            .devices()
            .filter_map(Self::from_device)
            .collect())
    }

    pub(super) fn from_device(device: *mut wdi_device_info) -> Option<Self> {
        let device = unsafe { &*device };
        let text = |pointer: *mut c_char| {
            if pointer.is_null() {
                String::new()
            } else {
                unsafe { CStr::from_ptr(pointer) }
                    .to_string_lossy()
                    .into_owned()
            }
        };
        let compatible_id = text(device.compatible_id).to_ascii_uppercase();
        let bluetooth = compatible_id
            .strip_suffix(BLUETOOTH_CLASS)
            .is_some_and(|prefix| prefix.ends_with(['\\', '&']));
        if !bluetooth {
            return None;
        }
        let driver = text(device.driver);
        let device_id = text(device.device_id);
        let node = DeviceNode::locate(&device_id);
        Some(Self {
            name: node
                .as_ref()
                .and_then(DeviceNode::name)
                .unwrap_or_else(|| text(device.desc)),
            package: node
                .as_ref()
                .filter(|_| Self::winusb(&driver))
                .and_then(DriverPackage::of),
            driver,
            device_id,
            composite: device.is_composite != 0,
        })
    }

    fn winusb(driver: &str) -> bool {
        driver.eq_ignore_ascii_case("WinUSB")
    }

    pub(super) fn uses_winusb(&self) -> bool {
        Self::winusb(&self.driver)
    }

    fn classic(&self) -> bool {
        self.uses_winusb() && !self.composite
    }

    fn low_energy(&self) -> bool {
        !self.uses_winusb() && !self.driver.is_empty()
    }

    pub(super) fn removable(&self) -> bool {
        self.package.as_ref().is_some_and(DriverPackage::removable)
    }

    fn change(&self) -> Option<(&'static str, ElevatedTask)> {
        let device_id = self.device_id.clone();
        if !self.composite && !self.uses_winusb() {
            Some((ASSIGN_LABEL, ElevatedTask::InstallWinUsb { device_id }))
        } else if self.removable() {
            Some((
                "Uninstall Polymer Bluetooth Handling",
                ElevatedTask::RemoveWinUsb { device_id },
            ))
        } else {
            None
        }
    }

    fn tile(&self, ui: &mut egui::Ui, selected: bool) -> egui::Response {
        let (rect, response) = ui.allocate_exact_size(TILE_SIZE, Sense::click());
        if response.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        }
        let stroke = if selected {
            Stroke::new(2.0_f32, Color32::WHITE)
        } else {
            Stroke::new(1.0_f32, Color32::from_white_alpha(24))
        };
        let icon = if self.uses_winusb() {
            ACCENT
        } else {
            Color32::WHITE
        };
        let (width, height) = (7.0, 14.0);
        let center = rect.center_top() + Vec2::new(0.0, 36.0);
        let rune = [
            (-width, -height / 2.0),
            (width, height / 2.0),
            (0.0, height),
            (0.0, -height),
            (width, -height / 2.0),
            (-width, height / 2.0),
        ]
        .map(|(x, y)| center + Vec2::new(x, y));
        let mut job = LayoutJob::simple(
            self.name.clone(),
            FontId::proportional(12.0),
            Color32::from_white_alpha(220),
            rect.width() - 16.0,
        );
        job.halign = Align::Center;
        job.wrap.max_rows = 3;
        let galley = ui.fonts(|fonts| fonts.layout_job(job));
        let painter = ui.painter();
        painter.rect(
            rect,
            Rounding::same(10.0),
            Color32::from_white_alpha(if selected { 28 } else { 16 }),
            stroke,
        );
        painter.add(Shape::line(rune.to_vec(), Stroke::new(2.5_f32, icon)));
        painter.galley(
            pos2(rect.center().x, rect.top() + 64.0),
            galley,
            Color32::WHITE,
        );
        response
    }
}

pub(crate) struct BluetoothPanel {
    adapters: Vec<BluetoothAdapter>,
    error: Option<String>,
    radio: Result<(), &'static str>,
    selected: Option<String>,
    refreshed: Instant,
    job: ElevatedJob,
    continues: bool,
}

impl BluetoothPanel {
    pub(crate) fn new(runtime: tokio::runtime::Handle, continues: bool) -> Self {
        let mut panel = Self {
            adapters: Vec::new(),
            error: None,
            radio: Ok(()),
            selected: None,
            refreshed: Instant::now(),
            job: ElevatedJob::new(runtime),
            continues,
        };
        panel.refresh();
        panel
    }

    pub(crate) fn busy(&self) -> bool {
        self.job.busy()
    }

    fn refresh(&mut self) {
        match BluetoothAdapter::list() {
            Ok(adapters) => {
                self.adapters = adapters;
                self.error = None;
            }
            Err(code) => self.error = Some(WinUsbInstaller::describe(code)),
        }
        self.radio = Self::windows_radio();
        if self.index().is_none() {
            self.selected = None;
        }
        self.refreshed = Instant::now();
    }

    pub(crate) fn show(&mut self, ui: &mut egui::Ui, actions: &[NavAction]) -> bool {
        let mut state = self.job.state();
        if matches!(state, SetupState::Installed) {
            self.job.reset();
            self.refresh();
            state = SetupState::Ready;
        }
        let installing = self.job.busy();
        if !installing && self.refreshed.elapsed() >= REFRESH_INTERVAL {
            self.refresh();
        }
        ui.ctx().request_repaint_after(REFRESH_INTERVAL);
        let (mut task, mut proceed) = if installing {
            (None, false)
        } else {
            self.navigate(actions)
        };
        self.show_readiness(ui);
        self.show_adapters(ui);
        state.show(ui);
        if matches!(state, SetupState::RestartRequired) {
            ui.label("Restart Windows to finish changing the adapter's driver.");
        }
        let change = self.target();
        let label = change.as_ref().map_or(ASSIGN_LABEL, |(label, _)| label);
        if Setup::button(ui, label, change.is_some() && !installing, false) {
            task = change.map(|(_, task)| task);
        }
        if self.continues && Setup::button(ui, "Continue", !installing, self.selected.is_none()) {
            proceed = true;
        }
        if let Some(task) = task {
            self.job.start(ui.ctx(), task);
        }
        Setup::notice(ui, LIBWDI_NOTICE, LIBWDI_REPOSITORY);
        proceed
    }

    fn windows_radio() -> Result<(), &'static str> {
        let radios: Vec<Radio> = Radio::GetRadiosAsync()
            .and_then(|operation| operation.join())
            .map(|radios| {
                radios
                    .into_iter()
                    .filter(|radio| radio.Kind() == Ok(RadioKind::Bluetooth))
                    .collect()
            })
            .unwrap_or_default();
        if radios.is_empty() {
            Err("Needs an adapter that Windows uses")
        } else if radios
            .iter()
            .any(|radio| radio.State() == Ok(RadioState::On))
        {
            Ok(())
        } else {
            Err("Turn on Bluetooth in Windows settings")
        }
    }

    fn show_readiness(&self, ui: &mut egui::Ui) {
        let low_energy = self.radio.map(|()| {
            self.adapters
                .iter()
                .find(|adapter| adapter.low_energy())
                .map_or("Windows Bluetooth", |adapter| adapter.name.as_str())
        });
        let classic = self
            .adapters
            .iter()
            .find(|adapter| adapter.classic())
            .map(|adapter| adapter.name.as_str())
            .ok_or("Needs an adapter assigned to Polymer");
        let ready = low_energy.is_ok() && classic.is_ok();
        Frame::none()
            .fill(Color32::from_white_alpha(16))
            .rounding(Rounding::same(10.0))
            .inner_margin(Margin::same(12.0))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                let (title, color) = if ready {
                    ("Ready", READY)
                } else {
                    ("Not ready", Setup::ERROR)
                };
                ui.label(RichText::new(title).size(16.0).color(color));
                Self::requirement(ui, "Bluetooth LE", low_energy);
                Self::requirement(ui, "Bluetooth Classic", classic);
            });
    }

    fn requirement(ui: &mut egui::Ui, role: &str, status: Result<&str, &str>) {
        ui.horizontal(|ui| {
            let (dot, _) = ui.allocate_exact_size(Vec2::splat(10.0), Sense::hover());
            let color = if status.is_ok() { READY } else { Setup::ERROR };
            ui.painter().circle_filled(dot.center(), 4.0, color);
            ui.label(RichText::new(role).color(Color32::WHITE));
            let (Ok(detail) | Err(detail)) = status;
            ui.label(
                RichText::new(detail)
                    .size(12.0)
                    .color(Color32::from_white_alpha(160)),
            );
        });
    }

    fn show_adapters(&mut self, ui: &mut egui::Ui) {
        if let Some(error) = &self.error {
            ui.label(RichText::new(error).color(Setup::ERROR));
        } else if self.adapters.is_empty() {
            ui.label(
                RichText::new("No USB Bluetooth adapters were found.")
                    .color(Color32::from_white_alpha(170)),
            );
        }
        let mut clicked = None;
        ui.horizontal_wrapped(|ui| {
            for adapter in &self.adapters {
                let selected = self.selected.as_ref() == Some(&adapter.device_id);
                if adapter.tile(ui, selected).clicked() {
                    clicked = Some(adapter.device_id.clone());
                }
            }
        });
        if clicked.is_some() {
            self.selected = clicked;
        }
    }

    fn index(&self) -> Option<usize> {
        let selected = self.selected.as_ref()?;
        self.adapters
            .iter()
            .position(|adapter| &adapter.device_id == selected)
    }

    fn target(&self) -> Option<(&'static str, ElevatedTask)> {
        self.adapters[self.index()?].change()
    }

    fn navigate(&mut self, actions: &[NavAction]) -> (Option<ElevatedTask>, bool) {
        let mut chosen = (None, false);
        for action in actions {
            let index = match (action, self.index()) {
                (NavAction::Up, Some(index)) => Some(index.saturating_sub(1)),
                (NavAction::Up, None) => self.adapters.len().checked_sub(1),
                (NavAction::Down, Some(index)) => {
                    Some(index + 1).filter(|next| *next < self.adapters.len())
                }
                (NavAction::Down, None) => None,
                (NavAction::Confirm, _) => {
                    chosen = self.confirm();
                    continue;
                }
                _ => continue,
            };
            self.selected = index.map(|index| self.adapters[index].device_id.clone());
        }
        chosen
    }

    fn confirm(&self) -> (Option<ElevatedTask>, bool) {
        match self.index() {
            None => (None, self.continues),
            Some(_) => (self.target().map(|(_, task)| task), false),
        }
    }
}
