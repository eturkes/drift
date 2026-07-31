#![forbid(unsafe_code)]

pub mod adapters;
pub mod aggregate;
pub mod digest;
pub mod error;
pub mod json;
pub mod model;
pub mod observer;
pub mod parse;
pub mod render;
pub mod report_validation;
pub mod validate;

pub use error::{DriftError, Result};
pub use model::*;
