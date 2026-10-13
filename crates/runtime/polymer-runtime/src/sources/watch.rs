use super::{SourceActor, SourceChanged};
use actix::Addr;
use notify_debouncer_full::notify::event::ModifyKind;
use notify_debouncer_full::notify::{EventKind, RecommendedWatcher, RecursiveMode};
use notify_debouncer_full::{DebounceEventResult, Debouncer, RecommendedCache, new_debouncer};
use std::path::Path;
use std::time::Duration;
use uuid::Uuid;

/// A recursive watch over one source root. Dropping it stops the watch.
pub(super) struct SourceWatch {
    _debouncer: Debouncer<RecommendedWatcher, RecommendedCache>,
}

impl SourceWatch {
    const QUIET: Duration = Duration::from_secs(2);

    pub fn start(id: Uuid, root: &Path, actor: Addr<SourceActor>) -> Result<Self, String> {
        let mut debouncer = new_debouncer(Self::QUIET, None, move |result: DebounceEventResult| {
            let change = match result {
                Ok(events) => SourceChanged {
                    id,
                    rescan: events.iter().any(|event| event.need_rescan()),
                    paths: events
                        .iter()
                        .filter(|event| !matches!(event.kind, EventKind::Access(_)))
                        .flat_map(|event| {
                            let modified = matches!(
                                event.kind,
                                EventKind::Modify(
                                    ModifyKind::Data(_) | ModifyKind::Metadata(_) | ModifyKind::Any
                                )
                            );
                            event
                                .paths
                                .iter()
                                .filter(move |path| !(modified && path.is_dir()))
                                .cloned()
                        })
                        .collect(),
                },
                Err(_) => SourceChanged {
                    id,
                    rescan: true,
                    paths: Vec::new(),
                },
            };
            actor.do_send(change);
        })
        .map_err(|error| error.to_string())?;
        debouncer
            .watch(root, RecursiveMode::Recursive)
            .map_err(|error| error.to_string())?;
        Ok(Self {
            _debouncer: debouncer,
        })
    }
}
