use librashader::presets::ShaderPresetPack;
use polymer_renderer::ShaderViewport;

/// A shader preset applied to one captured surface or surface group.
#[derive(Clone)]
pub struct SurfaceShader {
    pub pack: ShaderPresetPack,
    pub viewport: ShaderViewport,
    /// Aspect ratio of the source, or zero to infer it from the composed canvas.
    pub aspect_ratio: f32,
    pub parameters: Vec<(String, f32)>,
    pub passes: Option<usize>,
}

/// Where a shader drew the surfaces it composes, in its presenter's box pixels.
pub struct ShaderContent {
    pub presenter: u64,
    pub content: Option<[f32; 4]>,
}

pub enum ShaderControl {
    Attach(Box<SurfaceShader>),
    Detach,
    Parameter(String, f32),
    Passes(usize),
}
