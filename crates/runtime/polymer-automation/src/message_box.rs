use tokio::sync::mpsc;

use crate::{AutomationError, NodeSnapshot};

#[derive(actix::Message)]
#[rtype(result = "Result<MessageBoxes, AutomationError>")]
pub struct WatchMessageBoxes;

/// Dialog windows reported by accessibility window-open events as they appear.
pub struct MessageBoxes(pub(crate) mpsc::UnboundedReceiver<NodeSnapshot>);

impl MessageBoxes {
    pub async fn next(&mut self) -> Option<NodeSnapshot> {
        self.0.recv().await
    }
}
