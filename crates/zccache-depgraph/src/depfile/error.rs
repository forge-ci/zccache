//! Error type for depfile parsing.

/// Errors that can occur while parsing a `.d` file.
#[derive(Debug, thiserror::Error)]
pub enum DepfileError {
    /// I/O error reading the file.
    #[error("depfile I/O error: {0}")]
    Io(#[source] std::io::Error),
    /// The depfile content is malformed (empty or missing colon separator).
    #[error("malformed depfile: {0}")]
    Malformed(String),
}

impl From<std::io::Error> for DepfileError {
    fn from(e: std::io::Error) -> Self {
        DepfileError::Io(e)
    }
}
