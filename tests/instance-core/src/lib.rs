//! Runs the actual instance implementation without a display or SDL dependency.
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct Error(String);
impl Error {
    pub fn new(text: impl Into<String>) -> Self {
        Self(text.into())
    }
}
pub type Result<T> = std::result::Result<T, Error>;
#[derive(Clone)]
pub struct Waker {
    pub(crate) wake_type: u32,
    pub(crate) pending: Arc<AtomicBool>,
}
impl Waker {
    pub fn wake(&self) -> Result<()> {
        let _ = self.wake_type;
        self.pending.store(true, Ordering::Release);
        Ok(())
    }
}
#[path = "../../../rust/gos-windowing/src/instance.rs"]
pub mod instance;
