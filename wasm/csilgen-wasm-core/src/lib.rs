//! Core csilgen functionality as WASM module

use csilgen_core::{parse_csil, validate_spec};
use wasm_bindgen::prelude::*;

// Import the `console.log` function from the `console` module
#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = console)]
    fn log(s: &str);
}

// Define a macro to make console logging easier
macro_rules! console_log {
    ($($t:tt)*) => (log(&format_args!($($t)*).to_string()))
}

/// Parse CSIL from string (WASM export)
#[wasm_bindgen]
pub fn parse_csil_wasm(input: &str) -> Result<String, JsValue> {
    console_log!("Parsing CSIL interface definition");

    let spec = parse_csil(input).map_err(|e| JsValue::from_str(&e.to_string()))?;

    serde_json::to_string(&spec).map_err(|e| JsValue::from_str(&e.to_string()))
}

/// Validate CSIL interface definition (WASM export)
#[wasm_bindgen]
pub fn validate_csil_wasm(spec_json: &str) -> Result<(), JsValue> {
    console_log!("Validating CSIL interface definition");

    let spec = serde_json::from_str(spec_json).map_err(|e| JsValue::from_str(&e.to_string()))?;

    validate_spec(&spec).map_err(|e| JsValue::from_str(&e.to_string()))
}
