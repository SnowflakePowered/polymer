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
