use anyrender::PaintScene as _;
use blitz_dom::node::{SpecialElementData, Widget};
use blitz_dom::{BaseDocument, NodeId, local_name};
use peniko::{Blob, ImageAlphaType, ImageData, ImageFormat};
use std::sync::Arc;

pub(super) struct BlurhashWidget {
    image: Option<ImageData>,
}

impl BlurhashWidget {
    const SIZE: u32 = 16;
    pub(super) const ATTRIBUTE: &str = "data-blurhash";

    pub(super) fn attach(document: &mut BaseDocument, node: NodeId, hash: &str) {
        let pending = document
            .get_node(node)
            .and_then(|node| node.element_data())
            .is_some_and(|element| {
                element.name.local == local_name!("img")
                    && matches!(element.special_data, SpecialElementData::None)
            });
        if pending {
            document.set_custom_widget(
                node,
                Box::new(Self {
                    image: Self::decode(hash),
                }),
            );
        }
    }

    fn decode(hash: &str) -> Option<ImageData> {
        let pixels = blurhash::decode(hash, Self::SIZE, Self::SIZE, 1.0).ok()?;
        Some(ImageData {
            data: Blob::new(Arc::new(pixels)),
            format: ImageFormat::Rgba8,
            alpha_type: ImageAlphaType::Alpha,
            width: Self::SIZE,
            height: Self::SIZE,
        })
    }
}

impl Widget for BlurhashWidget {
    fn attribute_changed(&mut self, name: &str, _old_value: Option<&str>, new_value: Option<&str>) {
        if name == Self::ATTRIBUTE {
            self.image = new_value.and_then(Self::decode);
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
        if let Some(image) = &self.image {
            let brush =
                peniko::ImageBrush::new(image.clone()).with_quality(peniko::ImageQuality::Medium);
            let size = f64::from(Self::SIZE);
            scene.draw_image(
                brush.as_ref(),
                kurbo::Affine::scale_non_uniform(f64::from(width) / size, f64::from(height) / size),
            );
        }
        scene
    }
}
