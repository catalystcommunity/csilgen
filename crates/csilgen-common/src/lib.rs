//! Shared utilities, types, and error handling for csilgen

pub mod error;
pub mod packaging;
pub mod types;

#[cfg(test)]
mod error_message_tests;

#[cfg(any(test, feature = "testing"))]
pub mod testing;

pub use error::*;
pub use packaging::*;
pub use types::*;
