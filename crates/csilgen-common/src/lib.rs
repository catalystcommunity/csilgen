//! Shared utilities, types, and error handling for csilgen

pub mod choice;
pub mod error;
pub mod hoist;
pub mod packaging;
pub mod rename;
pub mod splice;
pub mod types;

#[cfg(test)]
mod error_message_tests;

#[cfg(any(test, feature = "testing"))]
pub mod testing;

pub use choice::*;
pub use error::*;
pub use hoist::*;
pub use packaging::*;
pub use rename::*;
pub use splice::*;
pub use types::*;
