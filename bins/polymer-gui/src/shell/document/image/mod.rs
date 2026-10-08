mod loader;

use anyrender::PaintScene as _;
use base64::Engine as _;
use blitz_dom::node::{ComputedStyles, Widget};
use blitz_dom::{BaseDocument, IntrinsicSizes, LocalName, NodeId, local_name};
use loader::ImageEntry;
pub(super) use loader::ImageLoader;
use peniko::{Blob, ImageAlphaType, ImageData, ImageFormat, ImageQuality};
use polymer_banner::{Banner, BannerImages};
use polymer_mipmap::MipPlaceholder;
use std::sync::Arc;
use style::properties::generated::longhands::object_fit::computed_value::T as ObjectFit;
use style::values::computed::CSSPixelLength;
use style::values::specified::image::ImageRendering;

/// Owns an `<img>` that shows a `data-blurhash` placeholder until its `src`
/// decodes, or whose `src` is a banner `data:` URL that the shell rasterizer
/// renders in place of a placeholder image. Decoded images are drawn through
/// placeholders at their device size that the rasterizer resamples from the
/// image's mip chain.
pub(super) struct ImageWidget {
    loader: Arc<ImageLoader>,
    blurhash: Option<ImageData>,
    content: Content,
}

enum Content {
    None,
    Raster(Arc<ImageEntry>, Option<(MipPlaceholder, u64)>),
    Banner(Banner, Option<Placeholder>),
}

struct Placeholder(ImageData);

impl Drop for Placeholder {
    fn drop(&mut self) {
        BannerImages::release(&self.0);
    }
}

impl Content {
    fn banner(source: Option<&str>) -> Option<Self> {
        let (media_type, data) = source?.strip_prefix("data:")?.split_once(";base64,")?;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(data)
            .ok()?;
        Some(Self::Banner(Banner::parse(media_type, &bytes)?, None))
    }

    fn new(loader: &ImageLoader, source: Option<&str>) -> Self {
        Self::banner(source).unwrap_or_else(|| {
            source
                .and_then(|source| loader.fetch(source))
                .map_or(Self::None, |entry| Self::Raster(entry, None))
        })
    }
}

impl ImageWidget {
    const BLURHASH: &str = "data-blurhash";
    const BLURHASH_SIZE: u32 = 16;
    const MAX_PLACEHOLDER: f64 = 8192.0;

    pub(super) fn is_trigger(name: &str) -> bool {
        name == "src" || name == Self::BLURHASH
    }

    pub(super) fn attach(document: &mut BaseDocument, loader: &Arc<ImageLoader>, node: NodeId) {
        let Some(element) = document.get_node(node).and_then(|node| node.element_data()) else {
            return;
        };
        if element.name.local != local_name!("img") || element.custom_widget_data().is_some() {
            return;
        }
        let hash = element.attr(LocalName::from(Self::BLURHASH));
        let source = element.attr(local_name!("src"));
        let Some(content) = Content::banner(source)
            .or_else(|| hash.is_some().then(|| Content::new(loader, source)))
        else {
            return;
        };
        let widget = Self {
            loader: loader.clone(),
            blurhash: hash.and_then(Self::decode_blurhash),
            content,
        };
        document.set_custom_widget(node, Box::new(widget));
    }

    fn decode_blurhash(hash: &str) -> Option<ImageData> {
        let size = Self::BLURHASH_SIZE;
        let pixels = blurhash::decode(hash, size, size, 1.0).ok()?;
        Some(ImageData {
            data: Blob::new(Arc::new(pixels)),
            format: ImageFormat::Rgba8,
            alpha_type: ImageAlphaType::Alpha,
            width: size,
            height: size,
        })
    }

    fn fit(styles: &ComputedStyles, width: u32, height: u32, image: (u32, u32)) -> kurbo::Affine {
        let (container_width, container_height) = (f64::from(width), f64::from(height));
        let (image_width, image_height) = (f64::from(image.0), f64::from(image.1));
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

    fn raster_image(
        loader: &ImageLoader,
        entry: &Arc<ImageEntry>,
        placeholder: &mut Option<(MipPlaceholder, u64)>,
        styles: &ComputedStyles,
        (width, height): (u32, u32),
        nearest: bool,
    ) -> Option<(ImageData, kurbo::Affine)> {
        let (image_width, image_height) = entry.size()?;
        let fit = Self::fit(styles, width, height, (image_width, image_height));
        let [x_scale, _, _, y_scale, _, _] = fit.as_coeffs();
        let scale = loader.scale_factor();
        let device = |length: u32, factor: f64| {
            (f64::from(length) * factor * scale)
                .round()
                .clamp(1.0, Self::MAX_PLACEHOLDER) as u32
        };
        let (target_width, target_height) =
            (device(image_width, x_scale), device(image_height, y_scale));
        let (x_ratio, y_ratio) = (
            f64::from(image_width) / f64::from(target_width),
            f64::from(image_height) / f64::from(target_height),
        );
        let level = x_ratio.min(y_ratio).log2().floor().max(0.0) as u32;
        let revision = entry.prepare(loader, level)?;
        let image = match placeholder {
            Some((current, current_revision))
                if *current_revision == revision
                    && (current.image().width, current.image().height)
                        == (target_width, target_height) =>
            {
                current.image().clone()
            }
            _ => {
                let created = entry
                    .source()
                    .placeholder(target_width, target_height, nearest);
                let image = created.image().clone();
                *placeholder = Some((created, revision));
                image
            }
        };
        Some((image, fit.pre_scale_non_uniform(x_ratio, y_ratio)))
    }

    fn banner_image(
        banner: &Banner,
        placeholder: &mut Option<Placeholder>,
        width: u32,
        height: u32,
        scale: f64,
    ) -> ImageData {
        let pixels = |length: u32| ((f64::from(length) * scale).ceil() as u32).max(1);
        let size = banner
            .native_size()
            .unwrap_or_else(|| (pixels(width), pixels(height)));
        match placeholder {
            Some(Placeholder(image)) if (image.width, image.height) == size => image.clone(),
            _ => {
                *placeholder = None;
                let image = BannerImages::placeholder(banner.clone(), size.0, size.1);
                *placeholder = Some(Placeholder(image.clone()));
                image
            }
        }
    }
}

impl Widget for ImageWidget {
    fn attribute_changed(&mut self, name: &str, _old_value: Option<&str>, new_value: Option<&str>) {
        match name {
            "src" => self.content = Content::new(&self.loader, new_value),
            Self::BLURHASH => self.blurhash = new_value.and_then(Self::decode_blurhash),
            _ => {}
        }
    }

    fn requires_redraw(&self) -> bool {
        matches!(self.content, Content::Banner(_, Some(_)))
    }

    fn intrinsic_sizes(&self) -> IntrinsicSizes {
        let size = match &self.content {
            Content::Raster(entry, _) => entry.size(),
            Content::Banner(banner, _) => banner.native_size(),
            Content::None => None,
        };
        size.map_or_else(IntrinsicSizes::default, |(width, height)| IntrinsicSizes {
            width: Some(width as f32),
            height: Some(height as f32),
            ratio: Some(width as f32 / height as f32),
        })
    }

    fn paint(
        &mut self,
        _render_ctx: &mut dyn anyrender::RenderContext,
        styles: &ComputedStyles,
        width: u32,
        height: u32,
        scale: f64,
    ) -> anyrender::Scene {
        let mut scene = anyrender::Scene::new();
        let quality = match styles.clone_image_rendering() {
            ImageRendering::Auto => ImageQuality::Medium,
            ImageRendering::CrispEdges | ImageRendering::Pixelated => ImageQuality::Low,
        };
        let image = match &mut self.content {
            Content::Raster(entry, placeholder) => Self::raster_image(
                &self.loader,
                entry,
                placeholder,
                styles,
                (width, height),
                quality == ImageQuality::Low,
            ),
            Content::Banner(banner, placeholder) => {
                let image = Self::banner_image(banner, placeholder, width, height, scale);
                let transform = Self::fit(styles, width, height, (image.width, image.height));
                Some((image, transform))
            }
            Content::None => None,
        };
        if let Some((image, transform)) = image {
            let brush = peniko::ImageBrush::new(image).with_quality(quality);
            scene.draw_image(brush.as_ref(), transform);
        } else if let Some(blurhash) = &self.blurhash {
            let size = f64::from(Self::BLURHASH_SIZE);
            let brush =
                peniko::ImageBrush::new(blurhash.clone()).with_quality(ImageQuality::Medium);
            scene.draw_image(
                brush.as_ref(),
                kurbo::Affine::scale_non_uniform(f64::from(width) / size, f64::from(height) / size),
            );
        }
        scene
    }
}
