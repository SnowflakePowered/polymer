use std::thread::JoinHandle;
use velopack::{UpdateCheck, UpdateInfo, UpdateManager, VelopackApp, sources::GithubSource};

pub(crate) struct Updater {
    manager: UpdateManager,
    download: JoinHandle<Option<UpdateInfo>>,
}

impl Updater {
    const REPOSITORY: &str = env!("CARGO_PKG_REPOSITORY");

    pub(crate) fn startup() {
        VelopackApp::build().run();
    }

    pub(crate) fn start() -> Option<Self> {
        let source = GithubSource::new(Self::REPOSITORY, None, false);
        let manager = UpdateManager::new(source, None, None).ok()?;
        let background = manager.clone();
        let download = std::thread::spawn(move || Self::download(&background));
        Some(Self { manager, download })
    }

    fn download(manager: &UpdateManager) -> Option<UpdateInfo> {
        let update = match manager.check_for_updates() {
            Ok(UpdateCheck::UpdateAvailable(update)) => *update,
            Ok(UpdateCheck::NoUpdateAvailable | UpdateCheck::RemoteIsEmpty) => return None,
            Err(error) => {
                tracing::warn!(%error, "polymer: could not check for updates");
                return None;
            }
        };
        if let Err(error) = manager.download_updates(&update, None) {
            tracing::warn!(%error, "polymer: could not download the update");
            return None;
        }
        Some(update)
    }

    pub(crate) fn apply_on_exit(self) {
        if !self.download.is_finished() {
            return;
        }
        let Ok(Some(update)) = self.download.join() else {
            return;
        };
        if let Err(error) =
            self.manager
                .wait_exit_then_apply_updates(&update, true, false, Vec::<String>::new())
        {
            tracing::warn!(%error, "polymer: could not apply the update");
        }
    }
}
