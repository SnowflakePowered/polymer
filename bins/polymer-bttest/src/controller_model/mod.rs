mod asset;
mod math;

use asset::ModelAsset;
use eframe::egui;
use math::{Projector, Quaternion, Vector3};

pub struct GameCubeControllerModel {
    asset: Result<ModelAsset, String>,
    texture: Option<egui::TextureHandle>,
    transformed_positions: Vec<Vector3>,
    depth_order: Vec<(f32, usize)>,
}

impl GameCubeControllerModel {
    pub fn new() -> Self {
        Self {
            asset: ModelAsset::load(),
            texture: None,
            transformed_positions: Vec::new(),
            depth_order: Vec::new(),
        }
    }

    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        orientation: [f32; 4],
        reference: [f32; 4],
    ) -> egui::Response {
        let relative = Quaternion::from_array(reference)
            .conjugate()
            .multiplied(Quaternion::from_array(orientation));
        let desired = egui::vec2(ui.available_width().clamp(300.0, 560.0), 350.0);
        let (rect, response) = ui.allocate_exact_size(desired, egui::Sense::hover());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 8.0, egui::Color32::from_rgb(16, 19, 25));
        painter.rect_stroke(
            rect,
            8.0,
            egui::Stroke::new(1.0_f32, egui::Color32::from_gray(65)),
        );
        let projector = Projector::new(
            egui::pos2(rect.center().x, rect.center().y - 10.0),
            (rect.width() / 6.2).min(58.0),
        );
        self.draw_axes(&painter, &projector);
        self.draw_asset(ui.ctx(), &painter, &projector, relative);

        let [pitch, roll, yaw] = relative.physical_euler_degrees();
        painter.text(
            egui::pos2(rect.center().x, rect.bottom() - 12.0),
            egui::Align2::CENTER_BOTTOM,
            format!("Pitch {pitch:+6.1}°   Roll {roll:+6.1}°   Yaw {yaw:+6.1}°"),
            egui::FontId::monospace(13.0),
            egui::Color32::from_gray(215),
        );
        response
    }

    fn draw_asset(
        &mut self,
        context: &egui::Context,
        painter: &egui::Painter,
        projector: &Projector,
        orientation: Quaternion,
    ) {
        let asset = match &self.asset {
            Ok(asset) => asset,
            Err(error) => {
                painter.text(
                    painter.clip_rect().center(),
                    egui::Align2::CENTER_CENTER,
                    error,
                    egui::FontId::proportional(14.0),
                    egui::Color32::LIGHT_RED,
                );
                return;
            }
        };
        let texture = self.texture.get_or_insert_with(|| {
            context.load_texture(
                "ngc.glb/base-color",
                asset.texture.clone(),
                egui::TextureOptions::LINEAR_REPEAT
                    .with_mipmap_mode(Some(egui::TextureFilter::Linear)),
            )
        });

        self.transformed_positions.clear();
        self.transformed_positions.extend(
            asset
                .vertices
                .iter()
                .map(|vertex| orientation.rotate(vertex.position)),
        );
        self.depth_order.clear();
        self.depth_order.reserve(
            asset
                .triangles
                .len()
                .saturating_sub(self.depth_order.capacity()),
        );
        self.depth_order
            .extend(asset.triangles.iter().enumerate().map(|(index, triangle)| {
                let depth = triangle
                    .iter()
                    .map(|vertex| projector.depth(self.transformed_positions[*vertex as usize]))
                    .sum::<f32>()
                    / 3.0;
                (depth, index)
            }));
        self.depth_order
            .sort_unstable_by(|left, right| right.0.total_cmp(&left.0));

        let light = Vector3::new(-0.35, -0.45, 1.0).normalized();
        let mut mesh = egui::Mesh::with_texture(texture.id());
        mesh.vertices.reserve(asset.vertices.len());
        mesh.vertices
            .extend(asset.vertices.iter().zip(&self.transformed_positions).map(
                |(vertex, position)| {
                    let normal = orientation.rotate(vertex.normal);
                    let brightness = normal.dot(light).max(0.0).mul_add(0.68, 0.32);
                    let shade = (brightness * 255.0) as u8;
                    egui::epaint::Vertex {
                        pos: projector.project(*position),
                        uv: vertex.texture_coordinate,
                        color: egui::Color32::from_gray(shade),
                    }
                },
            ));
        mesh.indices.reserve(asset.triangles.len() * 3);
        for (_, triangle_index) in &self.depth_order {
            mesh.indices
                .extend_from_slice(&asset.triangles[*triangle_index]);
        }
        painter.add(egui::Shape::mesh(mesh));
    }

    fn draw_axes(&self, painter: &egui::Painter, projector: &Projector) {
        let origin = projector.project(Vector3::new(-2.40, -1.90, -1.25));
        for (axis, color, label) in [
            (Vector3::new(0.65, 0.0, 0.0), egui::Color32::LIGHT_RED, "X"),
            (
                Vector3::new(0.0, 0.65, 0.0),
                egui::Color32::LIGHT_GREEN,
                "Y",
            ),
            (Vector3::new(0.0, 0.0, 0.65), egui::Color32::LIGHT_BLUE, "Z"),
        ] {
            let endpoint =
                projector.project(Vector3::new(-2.40 + axis.x, -1.90 + axis.y, -1.25 + axis.z));
            painter.line_segment([origin, endpoint], egui::Stroke::new(2.0_f32, color));
            painter.text(
                endpoint,
                egui::Align2::CENTER_CENTER,
                label,
                egui::FontId::monospace(11.0),
                color,
            );
        }
    }
}
