//! Simple test WASM module for basic validation

/// Simple test function that just adds two numbers
#[unsafe(no_mangle)]
pub extern "C" fn add(a: i32, b: i32) -> i32 {
    a + b
}

/// Simple test function that returns a fixed result
#[unsafe(no_mangle)]
pub extern "C" fn generate(_input_ptr: i32, _input_len: i32) -> i32 {
    // Just return a success code for testing
    0
}
