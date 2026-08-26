use std::collections::HashSet;

#[derive(Clone, Debug)]
pub(super) struct DeviceIdentity {
    pub(super) source_id: String,
    pub(super) guid: String,
    pub(super) vendor: Option<u16>,
    pub(super) product: Option<u16>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct DeviceFingerprint {
    pub(super) name: String,
    pub(super) guid: String,
    pub(super) vendor: Option<u16>,
    pub(super) product: Option<u16>,
    pub(super) serial: Option<String>,
    pub(super) path: Option<String>,
}

#[derive(Clone, Debug)]
struct KnownSource {
    source_id: String,
    fingerprint: DeviceFingerprint,
    instance: Option<u32>,
}

/// Maintains provider-owned source IDs across SDL disconnect/reconnect cycles.
/// SDL instance IDs only identify a live connection and never leave this type.
#[derive(Default)]
pub(super) struct SourceRegistry {
    next_id: u64,
    known: Vec<KnownSource>,
}

impl SourceRegistry {
    pub(super) fn observe_connections(&mut self, instances: &HashSet<u32>) {
        for source in &mut self.known {
            if source
                .instance
                .is_some_and(|instance| !instances.contains(&instance))
            {
                source.instance = None;
            }
        }
    }

    pub(super) fn resolve(&mut self, instance: u32, fingerprint: DeviceFingerprint) -> String {
        if let Some(source) = self
            .known
            .iter_mut()
            .find(|source| source.instance == Some(instance))
        {
            return source.source_id.clone();
        }
        if let Some(source) = self.known.iter_mut().find(|source| {
            source.instance.is_none() && fingerprints_match(&source.fingerprint, &fingerprint)
        }) {
            source.instance = Some(instance);
            source.fingerprint = fingerprint;
            return source.source_id.clone();
        }
        self.next_id = self.next_id.wrapping_add(1);
        let source_id = format!("sdl3:{}", self.next_id);
        self.known.push(KnownSource {
            source_id: source_id.clone(),
            fingerprint,
            instance: Some(instance),
        });
        source_id
    }
}

fn fingerprints_match(candidate: &DeviceFingerprint, observed: &DeviceFingerprint) -> bool {
    candidate.guid == observed.guid
        && match (&candidate.serial, &observed.serial) {
            (Some(left), Some(right)) => left == right,
            _ => match (&candidate.path, &observed.path) {
                (Some(left), Some(right)) => left == right,
                _ => {
                    candidate.name == observed.name
                        && candidate.vendor == observed.vendor
                        && candidate.product == observed.product
                }
            },
        }
}
