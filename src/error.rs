use std::{fmt, io};

pub type Result<T> = std::result::Result<T, DriftError>;

#[derive(Debug)]
pub struct DriftError {
    pub code: &'static str,
    pub message: String,
}

impl DriftError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub fn io(action: &'static str, error: io::Error) -> Self {
        Self::new("E_IO", format!("{action}: {error}"))
    }
}

impl fmt::Display for DriftError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "error[{}]: {}", self.code, self.message)
    }
}

impl std::error::Error for DriftError {}
