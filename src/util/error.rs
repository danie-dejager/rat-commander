//! Crate-wide error type and `Result` alias.

use std::io;

/// The crate-wide error type. Backends and subsystems convert their own errors
/// into this so the UI layer has a single thing to render.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),

    #[error("operation not supported by this filesystem")]
    Unsupported,

    #[error("not found: {0}")]
    NotFound(String),

    #[error("invalid path: {0}")]
    InvalidPath(String),

    #[error("operation cancelled")]
    Cancelled,

    #[error("{0}")]
    Other(String),

    /// An FTPS server's certificate isn't trusted (yet).
    #[error("the server's certificate isn't trusted: {}", .0.reason)]
    UntrustedCertificate(Box<crate::vfs::remote::tls::CertFailure>),
}

impl Error {
    /// The underlying `io::ErrorKind`, when this wraps an I/O failure.
    ///
    /// The ops engine uses it to tell a permission problem (which the user can
    /// answer by escalating) from every other failure. Without this the kind is
    /// lost the moment an error is turned into a message.
    pub fn io_kind(&self) -> Option<io::ErrorKind> {
        match self {
            Error::Io(e) => Some(e.kind()),
            _ => None,
        }
    }

    /// Whether this failed purely because of filesystem permissions.
    pub fn is_permission_denied(&self) -> bool {
        self.io_kind() == Some(io::ErrorKind::PermissionDenied)
    }

    pub fn other(msg: impl Into<String>) -> Self {
        Error::Other(msg.into())
    }
}

pub type Result<T> = std::result::Result<T, Error>;
