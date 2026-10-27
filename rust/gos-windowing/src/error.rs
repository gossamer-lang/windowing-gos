/// A windowing failure: what was attempted and, when SDL gave one, its reason.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct Error(pub String);

impl Error {
    /// An error carrying `message` as is.
    pub fn new(message: impl Into<String>) -> Error {
        Error(message.into())
    }
}

/// The result of a windowing call.
pub type Result<T> = std::result::Result<T, Error>;
