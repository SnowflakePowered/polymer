use super::*;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct TrackedSurfaceId(pub u32);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TrackedSurfaceConfig {
    pub id: TrackedSurfaceId,
    pub render_target_index: u32,
    pub window_pattern: Option<String>,
    pub rotation: i32,
}

#[derive(Clone, Debug)]
pub struct EmulatorLaunchConfig {
    pub session: String,
    pub command: String,
    pub working_directory: Option<PathBuf>,
    pub env: Vec<(String, String)>,
    pub arguments: Vec<String>,
    pub artifact_root: PathBuf,
    pub hooks: Hooks,
    pub consumer: ConsumerPreference,
    pub isolate: bool,
    pub tracked_surfaces: Vec<TrackedSurfaceConfig>,
    pub tracking_enabled: bool,
    pub skip_surfaces: u32,
}

impl EmulatorLaunchConfig {
    pub fn render_configuration(&self) -> crate::RenderConfiguration {
        crate::RenderConfiguration {
            surfaces: self
                .tracked_surfaces
                .iter()
                .map(|surface| crate::SurfaceRequest {
                    index: surface.render_target_index,
                    pattern: surface.window_pattern.clone(),
                    rotation: surface.rotation,
                })
                .collect(),
        }
    }

    pub(super) fn validate(&self) -> Result<(), LaunchError> {
        if self.session.is_empty() || self.session.contains(['/', '\\']) {
            return Err(LaunchError::InvalidConfig(
                "session must be a non-empty socket name".to_owned(),
            ));
        }
        if self.command.trim().is_empty() {
            return Err(LaunchError::InvalidConfig(
                "target command must not be empty".to_owned(),
            ));
        }
        if self.hooks.graphics.is_empty() {
            return Err(LaunchError::InvalidConfig(
                "at least one graphics hook is required".to_owned(),
            ));
        }
        self.render_configuration()
            .validate()
            .map_err(LaunchError::InvalidConfig)?;
        let mut ids = HashSet::new();
        if self
            .tracked_surfaces
            .iter()
            .any(|surface| !ids.insert(surface.id))
        {
            return Err(LaunchError::InvalidConfig(
                "tracked surface ids must be unique".to_owned(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug)]
pub enum LaunchError {
    InvalidConfig(String),
    Io(std::io::Error),
}

impl fmt::Display for LaunchError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidConfig(message) => formatter.write_str(message),
            Self::Io(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for LaunchError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidConfig(_) => None,
            Self::Io(error) => Some(error),
        }
    }
}

impl From<std::io::Error> for LaunchError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InstanceExit {
    Natural,
    Terminated,
    Failed(Arc<str>),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InstanceState {
    Starting,
    Running,
    Closing,
    Exited(InstanceExit),
}
