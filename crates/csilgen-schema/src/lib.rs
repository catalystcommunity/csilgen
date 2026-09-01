//! Stable schema descriptors and one-way diagnostic CBOR unmarshalling.

mod cbor;
mod descriptor;
mod diagnostic;

pub use cbor::{CborError, DiagnosticValue, FloatValue, FloatWidth, SpannedValue};
pub use descriptor::*;
pub use diagnostic::*;
