mod host_component;

use std::fmt::Debug;

pub use crate::host_component::AppConfigHostComponent;

type Result<T> = std::result::Result<T, Error>;

/// A variable resolution error.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Invalid variable name.
    #[error("invalid variable name: {0}")]
    InvalidName(String),

    /// Invalid variable template.
    #[error("invalid variable template: {0}")]
    InvalidTemplate(String),

    /// Variable provider error.
    #[error("provider error: {0:?}")]
    Provider(#[source] anyhow::Error),

    /// Undefined variable.
    #[error("undefined variable: {0}")]
    Undefined(String),
}
