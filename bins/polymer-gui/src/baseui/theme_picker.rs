use super::{ACCENT, navigation::NavAction};
use eframe::egui::{self, Align2, Color32, CursorIcon, FontId, Rounding, Sense, Stroke, Vec2};
use polymer_pluginhost::{loading::PluginRegistry, theme::ThemePluginInstance};


const CARD_HEIGHT: f32 = 84.0;
const BADGE_SIZE: f32 = 52.0;

struct ThemeCard {
    id: String,
    version: String,
    name: String,
    author: Option<String>,
    description: Option<String>,
}

impl ThemeCard {
    fn new(theme: &dyn ThemePluginInstance) -> Self {
        let meta = theme.meta();
        Self {
            id: theme.id().to_owned(),
            version: theme.version().to_owned(),
            name: meta.name.clone().unwrap_or_else(|| theme.id().to_owned()),
            author: meta.author.clone(),
            description: meta.description.clone(),
        }
    }

    fn paint(&self, ui: &mut egui::Ui, current: bool, focused: bool) -> egui::Response {
        let (rect, response) =
            ui.allocate_exact_size(Vec2::new(ui.available_width(), CARD_HEIGHT), Sense::click());
        let hovered = response.hovered() || focused;
        if hovered {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        }
        let fill = Color32::from_white_alpha(if hovered { 28 } else { 16 });
        let stroke = if focused {
            Stroke::new(2.0_f32, Color32::WHITE)
        } else if current {
            Stroke::new(1.5_f32, ACCENT)
        } else {
            Stroke::new(1.0_f32, Color32::from_white_alpha(24))
        };
        let painter = ui.painter();
        painter.rect(rect, Rounding::same(10.0), fill, stroke);

        let badge = egui::Rect::from_min_size(
            rect.min + Vec2::splat((CARD_HEIGHT - BADGE_SIZE) / 2.0),
            Vec2::splat(BADGE_SIZE),
        );
        painter.rect_filled(badge, Rounding::same(12.0), ACCENT.gamma_multiply(0.35));
        painter.text(
            badge.center(),
            Align2::CENTER_CENTER,
            self.name.chars().next().unwrap_or('?').to_uppercase(),
            FontId::proportional(26.0),
            Color32::WHITE,
        );

        let text_x = badge.right() + 12.0;
        let mut y = rect.top() + 14.0;
        let lines = [
            (self.name.clone(), FontId::proportional(16.0), Color32::WHITE),
            (
                format!("{} · v{}", self.id, self.version),
                FontId::proportional(12.0),
                Color32::from_white_alpha(150),
            ),
            (
                self.author
                    .as_ref()
                    .map_or_else(|| "Unknown author".to_owned(), |author| format!("by {author}")),
                FontId::proportional(12.0),
                Color32::from_white_alpha(190),
            ),
            (
                self.description.clone().unwrap_or_default(),
                FontId::proportional(11.0),
                Color32::from_white_alpha(130),
            ),
        ];
        for (text, font, color) in lines {
            let galley = painter.layout_no_wrap(text, font, color);
            let height = galley.size().y;
            painter.galley(egui::pos2(text_x, y), galley, color);
            y += height + 2.0;
        }
        response
    }
}

pub(super) struct ThemePicker {
    cards: Vec<ThemeCard>,
    current: String,
    focus: usize,
}

impl ThemePicker {
    pub(super) fn new(plugins: &PluginRegistry, current: String) -> Self {
        let cards: Vec<ThemeCard> = plugins
            .plugins::<dyn ThemePluginInstance>()
            .iter()
            .map(|theme| ThemeCard::new(theme.as_ref()))
            .collect();
        let focus = cards.iter().position(|card| card.id == current).unwrap_or(0);
        Self { cards, current, focus }
    }

    pub(super) fn show(&mut self, ui: &mut egui::Ui, actions: &[NavAction]) -> Option<String> {
        let last = self.cards.len().saturating_sub(1);
        let mut chosen = None;
        for action in actions {
            match action {
                NavAction::Up => self.focus = self.focus.saturating_sub(1),
                NavAction::Down => self.focus = (self.focus + 1).min(last),
                NavAction::Confirm => chosen = self.cards.get(self.focus).map(|card| card.id.clone()),
                _ => {}
            }
        }
        let pointer_moved = ui.input(|input| input.pointer.delta() != Vec2::ZERO);
        for (index, card) in self.cards.iter().enumerate() {
            let response = card.paint(ui, card.id == self.current, index == self.focus);
            if pointer_moved && response.hovered() {
                self.focus = index;
            }
            if response.clicked() {
                chosen = Some(card.id.clone());
            }
        }
        chosen
    }
}
