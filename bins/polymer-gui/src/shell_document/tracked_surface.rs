use super::{GamePlacement, GameViewStyle};
use anyrender::PaintScene as _;
use blitz_dom::LocalName;
use blitz_dom::local_name;
use blitz_dom::node::Widget;
use std::collections::HashMap;

#[derive(Default)]
pub(super) struct TrackedSurfaceWidget {
    source_id: Option<u64>,
}

impl TrackedSurfaceWidget {
    fn new(source_id: Option<u64>) -> Self {
        Self { source_id }
    }
}

impl Widget for TrackedSurfaceWidget {
    fn attribute_changed(&mut self, name: &str, _old_value: Option<&str>, new_value: Option<&str>) {
        if name == "src" {
            self.source_id = new_value.and_then(|value| value.parse().ok());
        }
    }

    fn paint(
        &mut self,
        _render_ctx: &mut dyn anyrender::RenderContext,
        _styles: &blitz_dom::node::ComputedStyles,
        width: u32,
        height: u32,
        _scale: f64,
    ) -> anyrender::Scene {
        let mut scene = anyrender::Scene::new();
        if let Some(source_id) = self.source_id {
            let paint = anyrender::Paint::Resource(peniko::ImageBrush {
                image: anyrender::ResourceId::from_ffi(source_id),
                sampler: Default::default(),
            });
            scene.fill(
                peniko::Fill::NonZero,
                kurbo::Affine::IDENTITY,
                &paint,
                None,
                &kurbo::Rect::new(0.0, 0.0, f64::from(width), f64::from(height)),
            );
        }
        scene
    }
}

pub(super) fn apply_tracked_surface_styles(
    document: &blitz_dom::BaseDocument,
    placements: &mut [GamePlacement],
) {
    let styles = document
        .tree()
        .iter()
        .filter_map(|(node_id, node)| {
            let element = node.element_data()?;
            if !is_tracked_surface_element(element) {
                return None;
            }
            let source = element.attr(local_name!("src"))?.parse::<u64>().ok()?;
            let radius = element
                .attr(LocalName::from("data-corner-radius"))
                .and_then(|value| value.parse::<f32>().ok())
                .unwrap_or_default()
                .max(0.0);
            let styles = node.primary_styles()?;
            Some((
                source,
                (
                    node_id,
                    radius,
                    GameViewStyle {
                        object_fit: styles.clone_object_fit(),
                        object_position: styles.clone_object_position(),
                    },
                ),
            ))
        })
        .collect::<HashMap<_, _>>();
    for placement in placements {
        let maximum = (placement.rect.width().min(placement.rect.height()) * 0.5) as f32;
        placement.corner_radius = styles
            .get(&placement.source_id)
            .map(|(_, radius, _)| *radius)
            .unwrap_or_default()
            .min(maximum);
        placement.style = styles
            .get(&placement.source_id)
            .map(|(_, _, style)| style.clone());
        placement.node_id = styles
            .get(&placement.source_id)
            .map(|(node_id, _, _)| *node_id);
        placement.opacity = placement.opacity.clamp(0.0, 1.0);
    }
}

pub(super) fn initialise_tracked_surfaces(document: &mut blitz_html::HtmlDocument) {
    let surfaces = document
        .as_mut()
        .tree()
        .iter()
        .filter_map(|(node, current)| {
            let element = current.element_data()?;
            if element.name.local.as_ref() != "tracked-surface" {
                return None;
            }
            let source = element.attr(local_name!("src"))?.parse::<u64>().ok()?;
            Some((node, source))
        })
        .collect::<Vec<_>>();
    for (node, source) in surfaces {
        document
            .as_mut()
            .set_custom_widget(node, Box::new(TrackedSurfaceWidget::new(Some(source))));
    }
}

pub(super) fn is_tracked_surface_element(element: &blitz_dom::node::ElementData) -> bool {
    element.name.local.as_ref() == "tracked-surface"
}
