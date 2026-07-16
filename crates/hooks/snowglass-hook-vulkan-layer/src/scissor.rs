//! Capture-region policy derived from Vulkan viewport/scissor state.

use ash::vk;
use dashmap::DashMap;
use std::sync::LazyLock;

static PRESENT_RECT: LazyLock<DashMap<vk::Image, (vk::Rect2D, u64)>> =
    LazyLock::new(Default::default);

pub(crate) fn tracking_enabled() -> bool {
    std::env::var_os("SNOWGLASS_TRACK_RENDER_TARGET").is_some()
}

pub(crate) fn clamp(rect: vk::Rect2D, bounds: vk::Extent2D) -> Option<vk::Rect2D> {
    let left = rect.offset.x.max(0) as u32;
    let top = rect.offset.y.max(0) as u32;
    let right = (i64::from(rect.offset.x) + i64::from(rect.extent.width))
        .clamp(0, i64::from(bounds.width)) as u32;
    let bottom = (i64::from(rect.offset.y) + i64::from(rect.extent.height))
        .clamp(0, i64::from(bounds.height)) as u32;
    (right > left && bottom > top).then_some(vk::Rect2D {
        offset: vk::Offset2D {
            x: left as i32,
            y: top as i32,
        },
        extent: vk::Extent2D {
            width: right - left,
            height: bottom - top,
        },
    })
}

pub(crate) fn record(image: vk::Image, rect: vk::Rect2D, draw_area: u64) {
    let replace = {
        PRESENT_RECT
            .get(&image)
            .is_none_or(|current| draw_area >= current.1)
    };
    if replace {
        {
            PRESENT_RECT.insert(image, (rect, draw_area))
        };
    }
}

pub(crate) fn capture_rect(image: vk::Image, bounds: vk::Extent2D) -> Option<vk::Rect2D> {
    tracking_enabled()
        .then(|| PRESENT_RECT.get(&image).map(|entry| entry.0))
        .flatten()
        .and_then(|rect| clamp(rect, bounds))
}

pub(crate) fn remove(image: vk::Image) {
    {
        PRESENT_RECT.remove(&image)
    };
}
