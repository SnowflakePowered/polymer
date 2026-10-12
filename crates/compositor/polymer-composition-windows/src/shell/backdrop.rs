//! Native `backdrop-filter: blur()` for promoted shell groups.

use std::collections::HashMap;
use windows::{
    Foundation::{IPropertyValue, PropertyValue},
    Graphics::Effects::{
        IGraphicsEffect, IGraphicsEffect_Impl, IGraphicsEffectSource, IGraphicsEffectSource_Impl,
    },
    UI::Composition::{
        CompositionEffectBrush, CompositionEffectFactory, CompositionEffectSourceParameter,
        Compositor, SpriteVisual,
    },
    Win32::{
        Foundation::{E_BOUNDS, E_INVALIDARG},
        System::WinRT::Graphics::Direct2D::{
            GRAPHICS_EFFECT_PROPERTY_MAPPING, IGraphicsEffectD2D1Interop,
            IGraphicsEffectD2D1Interop_Impl,
        },
    },
    core::{GUID, HSTRING, Interface, PCWSTR, implement},
};

const CLSID_D2D1_GAUSSIAN_BLUR: GUID = GUID::from_u128(0x1feb6d69_2fe6_4ac9_8c58_1d7f93e7a6a5);
const D2D1_GAUSSIANBLUR_OPTIMIZATION_BALANCED: u32 = 1;
/// Samples outside the backdrop clamp to its edge instead of fading to
/// transparent, so the blurred region keeps a solid border.
const D2D1_BORDER_MODE_HARD: u32 = 1;
const SOURCE: &str = "Backdrop";

#[implement(IGraphicsEffect, IGraphicsEffectSource, IGraphicsEffectD2D1Interop)]
struct GaussianBlurEffect {
    deviation: f32,
    source: IGraphicsEffectSource,
}

impl IGraphicsEffect_Impl for GaussianBlurEffect_Impl {
    fn Name(&self) -> windows::core::Result<HSTRING> {
        Ok(HSTRING::from("Blur"))
    }

    fn SetName(&self, _: &HSTRING) -> windows::core::Result<()> {
        Ok(())
    }
}

impl IGraphicsEffectSource_Impl for GaussianBlurEffect_Impl {}

impl IGraphicsEffectD2D1Interop_Impl for GaussianBlurEffect_Impl {
    fn GetEffectId(&self) -> windows::core::Result<GUID> {
        Ok(CLSID_D2D1_GAUSSIAN_BLUR)
    }

    fn GetNamedPropertyMapping(
        &self,
        _: &PCWSTR,
        _: *mut u32,
        _: *mut GRAPHICS_EFFECT_PROPERTY_MAPPING,
    ) -> windows::core::Result<()> {
        Err(E_INVALIDARG.into())
    }

    fn GetPropertyCount(&self) -> windows::core::Result<u32> {
        Ok(3)
    }

    fn GetProperty(&self, index: u32) -> windows::core::Result<IPropertyValue> {
        match index {
            0 => PropertyValue::CreateSingle(self.deviation)?.cast(),
            1 => PropertyValue::CreateUInt32(D2D1_GAUSSIANBLUR_OPTIMIZATION_BALANCED)?.cast(),
            2 => PropertyValue::CreateUInt32(D2D1_BORDER_MODE_HARD)?.cast(),
            _ => Err(E_BOUNDS.into()),
        }
    }

    fn GetSource(&self, index: u32) -> windows::core::Result<IGraphicsEffectSource> {
        if index == 0 {
            Ok(self.source.clone())
        } else {
            Err(E_BOUNDS.into())
        }
    }

    fn GetSourceCount(&self) -> windows::core::Result<u32> {
        Ok(1)
    }
}

/// A sprite placed directly beneath a group, sharing its parent. A backdrop
/// brush inside the group's `LayerVisual` would only sample that layer's own
/// content, never the guest surfaces painted before it.
pub(super) struct BackdropVisual {
    pub visual: SpriteVisual,
    deviation: f32,
}

/// Effect factories are compiled per blur amount, since the deviation is a
/// fixed effect property rather than an animatable one.
#[derive(Default)]
pub(super) struct BackdropBlurs {
    factories: HashMap<u32, CompositionEffectFactory>,
    visuals: HashMap<u64, BackdropVisual>,
}

impl BackdropBlurs {
    pub fn visual(&self, group: u64) -> Option<&SpriteVisual> {
        self.visuals.get(&group).map(|backdrop| &backdrop.visual)
    }

    pub fn update(
        &mut self,
        compositor: &Compositor,
        group: u64,
        deviation: f32,
    ) -> windows::core::Result<&SpriteVisual> {
        if !self
            .visuals
            .get(&group)
            .is_some_and(|backdrop| backdrop.deviation == deviation)
        {
            let brush = self.brush(compositor, deviation)?;
            let visual = match self.visuals.remove(&group) {
                Some(backdrop) => backdrop.visual,
                None => compositor.CreateSpriteVisual()?,
            };
            visual.SetBrush(&brush)?;
            self.visuals
                .insert(group, BackdropVisual { visual, deviation });
        }
        Ok(&self.visuals[&group].visual)
    }

    pub fn retain(&mut self, mut live: impl FnMut(u64) -> bool) {
        self.visuals.retain(|group, _| live(*group));
    }

    fn brush(
        &mut self,
        compositor: &Compositor,
        deviation: f32,
    ) -> windows::core::Result<CompositionEffectBrush> {
        let factory = match self.factories.get(&deviation.to_bits()) {
            Some(factory) => factory.clone(),
            None => {
                let effect: IGraphicsEffect = GaussianBlurEffect {
                    deviation,
                    source: CompositionEffectSourceParameter::Create(&HSTRING::from(SOURCE))?
                        .cast()?,
                }
                .into();
                let factory = compositor.CreateEffectFactory(&effect)?;
                self.factories.insert(deviation.to_bits(), factory.clone());
                factory
            }
        };
        let brush = factory.CreateBrush()?;
        brush.SetSourceParameter(&HSTRING::from(SOURCE), &compositor.CreateBackdropBrush()?)?;
        Ok(brush)
    }
}
