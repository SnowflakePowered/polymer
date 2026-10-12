use super::{GamePlacement, GameViewStyle};
use crate::shell::protocol::{SurfaceGroupBounds, TrackedSource};
use anyrender::PaintScene as _;
use blitz_dom::node::{ElementData, Widget};
use blitz_dom::{LocalName, NodeId, local_name};
use std::collections::HashMap;

const VIDEO_SOURCE_BIT: u64 = 1 << 62;
const SURFACE_GROUP_BIT: u64 = 1 << 61;

impl SurfaceGroupBounds {
    pub(crate) fn id_for(node: NodeId) -> u64 {
        SURFACE_GROUP_BIT | node.as_u64()
    }
}

impl TrackedSource {
    pub(crate) fn video(node: NodeId) -> Self {
        Self::Video(VIDEO_SOURCE_BIT | node.as_u64())
    }

    pub(crate) fn from_id(id: u64) -> Self {
        if id & VIDEO_SOURCE_BIT == 0 {
            Self::Target(id)
        } else {
            Self::Video(id)
        }
    }

    pub(crate) fn id(self) -> u64 {
        match self {
            Self::Target(id) | Self::Video(id) => id,
        }
    }
}

#[derive(Default)]
pub(super) struct TrackedSurfaceWidget {
    source: Option<TrackedSource>,
}

impl TrackedSurfaceWidget {
    pub(super) fn for_element(node: NodeId, element: &ElementData) -> Option<Self> {
        if element.name.local == local_name!("video") {
            return Some(Self {
                source: Some(TrackedSource::video(node)),
            });
        }
        is_tracked_surface_element(element).then(|| Self {
            source: element
                .attr(local_name!("src"))
                .and_then(|source| source.parse().ok())
                .map(TrackedSource::Target),
        })
    }
}

impl Widget for TrackedSurfaceWidget {
    fn attribute_changed(&mut self, name: &str, _old_value: Option<&str>, new_value: Option<&str>) {
        if name == "src" && !matches!(self.source, Some(TrackedSource::Video(_))) {
            self.source = new_value
                .and_then(|value| value.parse().ok())
                .map(TrackedSource::Target);
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
        if let Some(source) = self.source {
            let paint = anyrender::Paint::Resource(peniko::ImageBrush {
                image: anyrender::ResourceId::from_ffi(source.id()),
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
        .filter(|(_, node)| node.flags.is_in_document())
        .filter_map(|(node_id, node)| {
            let element = node.element_data()?;
            let source = TrackedSurfaceWidget::for_element(node_id, element)?
                .source?
                .id();
            let radius = element
                .attr(LocalName::from("data-corner-radius"))
                .and_then(|value| value.parse::<f32>().ok())
                .unwrap_or_default()
                .max(0.0);
            let styles = node.primary_styles()?;
            let origin = node.absolute_position(0.0, 0.0);
            let surface_group = enclosing_surface_group(document, node_id).map(|group| {
                let group_node = document.get_node(group).expect("enclosing group exists");
                let group_origin = group_node.absolute_position(0.0, 0.0);
                let size = group_node.final_layout().size;
                (
                    SurfaceGroupBounds::id_for(group),
                    kurbo::Vec2::new(
                        f64::from(group_origin.x - origin.x),
                        f64::from(group_origin.y - origin.y),
                    ),
                    kurbo::Size::new(f64::from(size.width), f64::from(size.height)),
                )
            });
            Some((
                source,
                TrackedNode {
                    node_id,
                    radius,
                    style: GameViewStyle {
                        object_fit: styles.clone_object_fit(),
                        object_position: styles.clone_object_position(),
                    },
                    origin: kurbo::Point::new(f64::from(origin.x), f64::from(origin.y)),
                    surface_group,
                },
            ))
        })
        .fold(
            HashMap::<u64, Vec<TrackedNode>>::new(),
            |mut styles, (source, entry)| {
                styles.entry(source).or_default().push(entry);
                styles
            },
        );
    for placement in placements {
        let maximum = (placement.rect.width().min(placement.rect.height()) * 0.5) as f32;
        let origin = placement.rect.origin();
        let entry = styles.get(&placement.source_id).and_then(|entries| {
            entries.iter().min_by(|left, right| {
                left.origin
                    .distance(origin)
                    .total_cmp(&right.origin.distance(origin))
            })
        });
        placement.corner_radius = entry
            .map(|entry| entry.radius)
            .unwrap_or_default()
            .min(maximum);
        placement.style = entry.map(|entry| entry.style.clone());
        placement.node_id = entry.map(|entry| entry.node_id);
        placement.surface_group =
            entry
                .and_then(|entry| entry.surface_group)
                .map(|(id, offset, size)| SurfaceGroupBounds {
                    id,
                    bounds: kurbo::Rect::from_origin_size(origin + offset, size),
                    parent: placement.group,
                });
        placement.opacity = placement.opacity.clamp(0.0, 1.0);
    }
}

struct TrackedNode {
    node_id: blitz_dom::NodeId,
    radius: f32,
    style: GameViewStyle,
    origin: kurbo::Point,
    surface_group: Option<(u64, kurbo::Vec2, kurbo::Size)>,
}

pub(super) fn initialise_tracked_surfaces(document: &mut blitz_html::HtmlDocument) {
    let surfaces = document
        .as_mut()
        .tree()
        .iter()
        .filter_map(|(node, current)| {
            let widget = TrackedSurfaceWidget::for_element(node, current.element_data()?)?;
            Some((node, widget))
        })
        .collect::<Vec<_>>();
    for (node, widget) in surfaces {
        document.as_mut().set_custom_widget(node, Box::new(widget));
    }
}

pub(super) fn is_tracked_surface_element(element: &blitz_dom::node::ElementData) -> bool {
    &*element.name.local == "tracked-surface"
}

/// The nearest `<tracked-surface-group>` ancestor that composes this surface.
pub(super) fn enclosing_surface_group(
    document: &blitz_dom::BaseDocument,
    node: blitz_dom::NodeId,
) -> Option<blitz_dom::NodeId> {
    std::iter::successors(document.get_node(node)?.parent, |&parent| {
        document.get_node(parent)?.parent
    })
    .find(|&ancestor| {
        document
            .get_node(ancestor)
            .and_then(blitz_dom::Node::element_data)
            .is_some_and(|element| &*element.name.local == "tracked-surface-group")
    })
}
