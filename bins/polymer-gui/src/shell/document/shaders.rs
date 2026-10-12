use super::{ShellDocument, enclosing_surface_group, is_tracked_surface_element};
use crate::shell::protocol::{ShaderTarget, ShellEvent, ShellOutbox, SurfaceGroupBounds};
use blitz_dom::{NodeId, local_name};
use blitz_traits::net::Url;
use librashader::presets::{ShaderFeatures, ShaderPreset, ShaderPresetPack, context::VideoDriver};
use polymer_composition::{ShaderControl, SurfaceShader};
use polymer_renderer::ShaderViewport;
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, path::PathBuf};

#[derive(Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum PresetViewport {
    #[default]
    Auto,
    Content,
    Element,
}

#[derive(Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PresetOptions {
    #[serde(default)]
    viewport: PresetViewport,
    #[serde(default)]
    aspect_ratio: Option<f32>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PresetParameter {
    id: String,
    description: String,
    initial: f32,
    minimum: f32,
    maximum: f32,
    step: f32,
    value: f32,
}

/// A shader preset read from disk, with every pass and lookup texture loaded.
pub(crate) struct LoadedPreset {
    pack: ShaderPresetPack,
    parameters: Vec<PresetParameter>,
    viewport: ShaderViewport,
    aspect_ratio: f32,
}

impl LoadedPreset {
    pub(crate) fn load(path: PathBuf, options: PresetOptions) -> Result<Self, String> {
        let features =
            ShaderFeatures::ORIGINAL_ASPECT_UNIFORMS | ShaderFeatures::FRAMETIME_UNIFORMS;
        let preset =
            ShaderPreset::try_parse_with_driver_context(&path, features, VideoDriver::Direct3D12)
                .map_err(|error| error.to_string())?;
        let pack =
            ShaderPresetPack::load_from_preset::<Box<dyn std::error::Error + Send + Sync>>(preset)
                .map_err(|error| error.to_string())?;
        let mut parameters = Vec::<PresetParameter>::new();
        for parameter in pack
            .passes
            .iter()
            .flat_map(|pass| pass.data.parameters.values())
        {
            if parameters
                .iter()
                .any(|existing| existing.id == *parameter.id)
            {
                continue;
            }
            let value = pack
                .parameters
                .iter()
                .find(|meta| meta.name == parameter.id)
                .map_or(parameter.initial, |meta| meta.value);
            parameters.push(PresetParameter {
                id: parameter.id.to_string(),
                description: parameter.description.clone(),
                initial: parameter.initial,
                minimum: parameter.minimum,
                maximum: parameter.maximum,
                step: parameter.step,
                value,
            });
        }
        let viewport = match options.viewport {
            PresetViewport::Content => ShaderViewport::Content,
            PresetViewport::Element => ShaderViewport::Element,
            #[cfg(windows)]
            PresetViewport::Auto => {
                polymer_composition_windows::ShaderStage::probe_viewport(pack.clone())
                    .unwrap_or_else(|error| {
                        tracing::warn!(%error, "polymer: probing a shader preset failed");
                        ShaderViewport::Content
                    })
            }
            #[cfg(not(windows))]
            PresetViewport::Auto => ShaderViewport::Content,
        };
        Ok(Self {
            pack,
            parameters,
            viewport,
            aspect_ratio: options.aspect_ratio.unwrap_or_default(),
        })
    }

    pub(crate) fn parameters(&self) -> &[PresetParameter] {
        &self.parameters
    }

    pub(crate) const fn viewport(&self) -> ShaderViewport {
        self.viewport
    }
}

/// Loaded presets waiting to be attached, and the elements that own a chain
/// along with the id of the preset attached to them.
pub(crate) struct ShaderPresets {
    outbox: ShellOutbox,
    next: u32,
    presets: HashMap<u32, LoadedPreset>,
    attached: HashMap<NodeId, (ShaderTarget, u32)>,
}

impl ShaderPresets {
    pub(super) fn new(outbox: ShellOutbox) -> Self {
        Self {
            outbox,
            next: 0,
            presets: HashMap::new(),
            attached: HashMap::new(),
        }
    }

    fn send(&self, target: ShaderTarget, control: ShaderControl) {
        self.outbox.send(ShellEvent::Shader { target, control });
    }
}

impl ShellDocument {
    /// Resolves an absolute preset URL to the file it names.
    pub(crate) fn shader_preset_path(&self, url: &str) -> Result<PathBuf, String> {
        let url = Url::parse(url).map_err(|error| format!("{url} is not a URL: {error}"))?;
        match url.scheme() {
            "file" => url.to_file_path().ok(),
            _ => self.urls.local_file(&url),
        }
        .ok_or_else(|| format!("shader presets load from file: or theme: URLs, not {url}"))
    }

    pub(crate) fn insert_shader_preset(&mut self, preset: LoadedPreset) -> u32 {
        let shaders = &mut self.shaders;
        shaders.next = shaders.next.wrapping_add(1);
        shaders.presets.insert(shaders.next, preset);
        shaders.next
    }

    pub(crate) fn drop_shader_preset(&mut self, preset: u32) {
        self.shaders.presets.remove(&preset);
    }

    /// Moves a loaded preset onto a tracked surface or surface group.
    pub(crate) fn attach_shader(&mut self, node: NodeId, preset: u32) -> Result<(), String> {
        let element = self
            .document
            .as_ref()
            .get_node(node)
            .and_then(blitz_dom::Node::element_data)
            .ok_or("shaders attach to elements")?;
        let target = if &*element.name.local == "tracked-surface-group" {
            ShaderTarget::Group(SurfaceGroupBounds::id_for(node))
        } else if is_tracked_surface_element(element) && element.attr(local_name!("src")).is_some()
        {
            if enclosing_surface_group(self.document.as_ref(), node).is_some() {
                return Err("attach shaders to the enclosing <tracked-surface-group>".into());
            }
            ShaderTarget::Surface(node.as_u64())
        } else {
            return Err("shaders attach to <tracked-surface> or <tracked-surface-group>".into());
        };
        let id = preset;
        let preset = self
            .shaders
            .presets
            .remove(&id)
            .ok_or("this ShaderPreset was already attached")?;
        self.shaders.attached.insert(node, (target, id));
        let shader = SurfaceShader {
            pack: preset.pack,
            viewport: preset.viewport,
            aspect_ratio: preset.aspect_ratio,
            parameters: Vec::new(),
            passes: None,
        };
        self.shaders
            .send(target, ShaderControl::Attach(Box::new(shader)));
        Ok(())
    }

    /// Whether the preset `attachment` is still the one attached to `node`.
    pub(crate) fn shader_attached(&self, node: NodeId, attachment: u32) -> bool {
        self.shaders
            .attached
            .get(&node)
            .is_some_and(|(_, attached)| *attached == attachment)
    }

    pub(crate) fn control_shader(&mut self, node: NodeId, attachment: u32, control: ShaderControl) {
        if !self.shader_attached(node, attachment) {
            return;
        }
        let target = match control {
            ShaderControl::Detach => self.shaders.attached.remove(&node),
            _ => self.shaders.attached.get(&node).copied(),
        };
        if let Some((target, _)) = target {
            self.shaders.send(target, control);
        }
    }

    /// Whether `node` lies inside an element whose shader can draw anywhere in its box.
    pub(super) fn is_within_shaded_element(&self, node: NodeId) -> bool {
        let document = self.document.as_ref();
        std::iter::successors(Some(node), |node| {
            document.get_node(*node).and_then(|node| node.parent)
        })
        .any(|node| self.shaders.attached.contains_key(&node))
    }

    /// Detaches every shader in the subtree at `root`, or only those below it.
    pub(super) fn detach_shaders(&mut self, root: NodeId, inclusive: bool) {
        let document = self.document.as_ref();
        let removed = self
            .shaders
            .attached
            .iter()
            .filter(|(node, _)| {
                std::iter::successors(Some(**node), |node| {
                    document.get_node(*node).and_then(|node| node.parent)
                })
                .skip(usize::from(!inclusive))
                .any(|node| node == root)
            })
            .map(|(&node, &(_, attachment))| (node, attachment))
            .collect::<Vec<_>>();
        for (node, attachment) in removed {
            self.control_shader(node, attachment, ShaderControl::Detach);
        }
    }
}
