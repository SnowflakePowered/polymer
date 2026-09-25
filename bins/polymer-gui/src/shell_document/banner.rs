use anyrender::PaintScene as _;
use base64::Engine as _;
use blitz_dom::local_name;
use blitz_dom::node::Widget;
use polymer_banner::{Banner, BannerImages};
use style::properties::generated::longhands::object_fit::computed_value::T as ObjectFit;
use style::values::computed::CSSPixelLength;
use style::values::specified::image::ImageRendering;

/// `<ps2-icon src="data:application/x-ps2-icon;base64,…">`,
/// `<wii-banner src="data:application/x-wii-banner;base64,…">`,
/// `<ctr-banner src="data:application/x-3ds-banner;base64,…">` and
/// `<animated-image src="data:image/webp;base64,…">` paint a placeholder image
/// that the shell rasterizer replaces with the rendered banner. Placement and
/// sampling follow the element's `object-fit`, `object-position` and
/// `image-rendering`, as they would for an `<img>`.
#[derive(Default)]
pub(super) struct BannerWidget {
    banner: Option<Banner>,
    image: Option<peniko::ImageData>,
}

impl BannerWidget {
    pub(super) const USER_AGENT_CSS: &str =
        "ps2-icon, wii-banner, animated-image { pointer-events: none; }";

    pub(super) fn is_banner_element(element: &blitz_dom::node::ElementData) -> bool {
        matches!(
            element.name.local.as_ref(),
            "ps2-icon" | "wii-banner" | "ctr-banner" | "animated-image"
        )
    }

    pub(super) fn from_element(element: &blitz_dom::node::ElementData) -> Self {
        let mut widget = Self::default();
        widget.attribute_changed("src", None, element.attr(local_name!("src")));
        widget
    }

    fn fit(
        styles: &blitz_dom::node::ComputedStyles,
        width: u32,
        height: u32,
        image: &peniko::ImageData,
    ) -> kurbo::Affine {
        let (container_width, container_height) = (f64::from(width), f64::from(height));
        let (image_width, image_height) = (f64::from(image.width), f64::from(image.height));
        let contain = (container_width / image_width).min(container_height / image_height);
        let (x_scale, y_scale) = match styles.clone_object_fit() {
            ObjectFit::Fill => (
                container_width / image_width,
                container_height / image_height,
            ),
            ObjectFit::Contain => (contain, contain),
            ObjectFit::Cover => {
                let cover = (container_width / image_width).max(container_height / image_height);
                (cover, cover)
            }
            ObjectFit::None => (1.0, 1.0),
            ObjectFit::ScaleDown => (contain.min(1.0), contain.min(1.0)),
        };
        let position = styles.clone_object_position();
        let offset = |free: f64, axis: &style::values::computed::LengthPercentage| {
            f64::from(axis.resolve(CSSPixelLength::new(free as f32)).px())
        };
        kurbo::Affine::translate((
            offset(
                container_width - image_width * x_scale,
                &position.horizontal,
            ),
            offset(
                container_height - image_height * y_scale,
                &position.vertical,
            ),
        ))
        .pre_scale_non_uniform(x_scale, y_scale)
    }

    fn release(&mut self) {
        if let Some(image) = self.image.take() {
            BannerImages::release(&image);
        }
    }
}

impl Drop for BannerWidget {
    fn drop(&mut self) {
        self.release();
    }
}

impl Widget for BannerWidget {
    fn attribute_changed(&mut self, name: &str, _old_value: Option<&str>, new_value: Option<&str>) {
        if name != "src" {
            return;
        }
        self.banner = new_value
            .and_then(|value| value.strip_prefix("data:"))
            .and_then(|value| value.split_once(";base64,"))
            .and_then(|(media_type, data)| {
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(data)
                    .ok()?;
                Banner::parse(media_type, &bytes)
            });
        self.release();
    }

    fn requires_redraw(&self) -> bool {
        self.image.is_some()
    }

    fn paint(
        &mut self,
        _render_ctx: &mut dyn anyrender::RenderContext,
        styles: &blitz_dom::node::ComputedStyles,
        width: u32,
        height: u32,
        scale: f64,
    ) -> anyrender::Scene {
        let mut scene = anyrender::Scene::new();
        let Some(banner) = self.banner.clone() else {
            return scene;
        };
        let pixels = |length: u32| ((f64::from(length) * scale).ceil() as u32).max(1);
        let (target_width, target_height) = banner
            .native_size()
            .unwrap_or_else(|| (pixels(width), pixels(height)));
        if self
            .image
            .as_ref()
            .is_none_or(|image| (image.width, image.height) != (target_width, target_height))
        {
            self.release();
            self.image = Some(BannerImages::placeholder(
                banner,
                target_width,
                target_height,
            ));
        }
        if let Some(image) = &self.image {
            let brush = peniko::ImageBrush::new(image.clone()).with_quality(
                match styles.clone_image_rendering() {
                    ImageRendering::Auto => peniko::ImageQuality::Medium,
                    ImageRendering::CrispEdges | ImageRendering::Pixelated => {
                        peniko::ImageQuality::Low
                    }
                },
            );
            scene.draw_image(brush.as_ref(), Self::fit(styles, width, height, image));
        }
        scene
    }
}
