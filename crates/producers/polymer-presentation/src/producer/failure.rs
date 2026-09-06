use std::{fmt, io};
/// Explicit protocol outcomes are tagged so an OS permission error during
/// connection or handle duplication cannot masquerade as viewer rejection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionFailure {
    Connection,
    IncompatibleProtocol,
    Rejected,
}
impl SessionFailure {
    pub fn classify(error: &io::Error) -> Self {
        error
            .get_ref()
            .and_then(|error| error.downcast_ref::<Self>())
            .copied()
            .unwrap_or(Self::Connection)
    }
}
impl fmt::Display for SessionFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Connection => "viewer connection failed",
            Self::IncompatibleProtocol => "incompatible viewer protocol",
            Self::Rejected => "viewer rejected stream",
        })
    }
}
impl std::error::Error for SessionFailure {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::producer::ProducerSession;
    #[test]
    fn os_permission_failure_is_retryable_while_explicit_rejection_parks() {
        let transport = io::Error::new(io::ErrorKind::PermissionDenied, "duplicate handle denied");
        assert!(ProducerSession::setup_result::<()>(Err(transport)).is_err());
        for failure in [
            SessionFailure::Rejected,
            SessionFailure::IncompatibleProtocol,
        ] {
            let error = io::Error::new(io::ErrorKind::PermissionDenied, failure);
            assert_eq!(SessionFailure::classify(&error), failure);
            assert!(
                ProducerSession::setup_result::<()>(Err(error))
                    .unwrap()
                    .is_none()
            );
        }
    }
}
