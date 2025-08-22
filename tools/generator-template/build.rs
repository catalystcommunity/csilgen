//! Build script for WASM generator template
//!
//! This build script sets up the environment for compiling to WASM
//! and provides helpful output about the build process.

use std::env;

fn main() {
    // Print cargo instructions for WASM target
    println!("cargo:rerun-if-env-changed=CARGO_CFG_TARGET_ARCH");

    let target_arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();

    if target_arch == "wasm32" {
        println!("cargo:rustc-cfg=wasm_target");
        println!("cargo:rustc-link-arg=--export=generate");
        println!("cargo:rustc-link-arg=--export=get_metadata");
        println!("cargo:rustc-link-arg=--export=allocate");
        println!("cargo:rustc-link-arg=--export=deallocate");
        println!("cargo:rustc-link-arg=--no-entry");
    }

    // Check for required target
    if env::var("CARGO_CFG_TARGET_OS").unwrap_or_default() == "unknown" {
        // This is likely a WASM build
        println!("cargo:warning=Building for WASM target");
    }
}
