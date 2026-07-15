//! TypeScript code generator for CSIL specifications (WASM module).
//!
//! Dispatches on the requested target:
//! - `typescript-typesonly` -> `types.gen.ts`
//! - `typescript-client`     -> `types.gen.ts` + `client.gen.ts`
//! - `typescript-server`     -> `types.gen.ts` + `server.gen.ts`
//! - `typescript` (aggregate) -> all three

mod client;
mod codec;
mod common;
mod package;
mod server;
mod types;

#[cfg(test)]
mod tests;

use csilgen_common::{
    GeneratedFile, GenerationStats, GeneratorCapability, GeneratorMetadata, GeneratorWarning,
    WasmGeneratorInput, WasmGeneratorOutput, wasm_interface::*,
};

/// Get generator metadata (WASM export)
#[unsafe(no_mangle)]
pub extern "C" fn get_metadata() -> *const u8 {
    let metadata = GeneratorMetadata {
        name: "typescript-code-generator".to_string(),
        version: "1.0.0".to_string(),
        description: "TypeScript types, client, and server generator".to_string(),
        target: "typescript".to_string(),
        capabilities: vec![
            GeneratorCapability::BasicTypes,
            GeneratorCapability::ComplexStructures,
            GeneratorCapability::Services,
            GeneratorCapability::FieldMetadata,
            GeneratorCapability::FieldVisibility,
            GeneratorCapability::ValidationConstraints,
        ],
        author: Some("CSIL Team".to_string()),
        homepage: Some(
            "https://github.com/catalystcommunity/csilgen/typescript-generator".to_string(),
        ),
    };

    let metadata_json = match serde_json::to_string(&metadata) {
        Ok(json) => json,
        Err(_) => return std::ptr::null(),
    };

    let bytes = metadata_json.as_bytes();
    let ptr = allocate(bytes.len() + 4);
    if ptr.is_null() {
        return std::ptr::null();
    }

    unsafe {
        let len = bytes.len() as u32;
        std::ptr::write(ptr as *mut u32, len);
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr.add(4), bytes.len());
    }

    ptr
}

/// Memory allocation (WASM export)
#[unsafe(no_mangle)]
pub extern "C" fn allocate(size: usize) -> *mut u8 {
    let mut buf = Vec::with_capacity(size);
    let ptr = buf.as_mut_ptr();
    std::mem::forget(buf);
    ptr
}

/// Memory deallocation (WASM export)
#[unsafe(no_mangle)]
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn deallocate(ptr: *mut u8, size: usize) {
    if !ptr.is_null() && size > 0 {
        unsafe {
            let _ = Vec::from_raw_parts(ptr, 0, size);
        }
    }
}

/// Main generator function (WASM export)
#[unsafe(no_mangle)]
pub extern "C" fn generate(input_ptr: *const u8, input_len: usize) -> *mut u8 {
    let result = process_generation(input_ptr, input_len);

    match result {
        Ok(output) => {
            let output_json = match serde_json::to_string(&output) {
                Ok(json) => json,
                Err(_e) => return std::ptr::null_mut(),
            };

            let bytes = output_json.as_bytes();
            let allocated_ptr = allocate(bytes.len() + 4);
            if allocated_ptr.is_null() {
                return std::ptr::null_mut();
            }

            unsafe {
                let len = bytes.len() as u32;
                std::ptr::write(allocated_ptr as *mut u32, len);
                std::ptr::copy_nonoverlapping(bytes.as_ptr(), allocated_ptr.add(4), bytes.len());
            }

            allocated_ptr
        }
        Err(_code) => std::ptr::null_mut(),
    }
}

fn process_generation(input_ptr: *const u8, input_len: usize) -> Result<WasmGeneratorOutput, i32> {
    if input_ptr.is_null() || input_len == 0 || input_len > MAX_INPUT_SIZE {
        return Err(error_codes::INVALID_INPUT);
    }

    let input_slice = unsafe { std::slice::from_raw_parts(input_ptr, input_len) };
    let input_str = std::str::from_utf8(input_slice).map_err(|_| error_codes::INVALID_INPUT)?;
    let input: WasmGeneratorInput =
        serde_json::from_str(input_str).map_err(|_| error_codes::SERIALIZATION_ERROR)?;

    let files = generate_files(&input).map_err(|_| error_codes::GENERATION_ERROR)?;

    let stats = GenerationStats {
        files_generated: files.len(),
        total_size_bytes: files.iter().map(|f| f.content.len()).sum(),
        services_count: input.csil_spec.service_count,
        fields_with_metadata_count: input.csil_spec.fields_with_metadata_count,
        generation_time_ms: 0,
        peak_memory_bytes: None,
    };

    Ok(WasmGeneratorOutput {
        files,
        warnings: Vec::<GeneratorWarning>::new(),
        stats,
    })
}

/// Produce the set of files appropriate for the requested target. The client
/// and server outputs always travel with the types file they import from.
///
/// Returns `Err(message)` on misconfiguration (e.g. an invalid
/// `ts_bidirectional_transport` value) so the wasm boundary surfaces a clean
/// generation failure instead of silently degrading.
pub fn generate_files(input: &WasmGeneratorInput) -> Result<Vec<GeneratedFile>, String> {
    // Validate options *before* deciding which files to emit so a bad option
    // fails the entire run, regardless of which target is requested.
    let _mode = common::bidi_transport(input)?;
    let _decimal = common::decimal_mapping(input)?;
    let style = common::client_style(input)?;
    let _import_extension = common::import_extension(input)?;

    // Rewrite inline (anonymous) composite field types to synthesized named rules so
    // every emitter sees them as ordinary references and routes them through the
    // named-rule machinery; a spec without inline composites is reproduced unchanged.
    // Every emitter below reads from this single hoisted view so types, codec, client,
    // and server agree on the synthesized declarations. An all-literal choice is left
    // inline (`hoist_all_literal_choices: false`) — the codec already renders a
    // correct bare-literal enum for it in the field position itself.
    let hoisted = {
        let mut cloned = input.clone();
        cloned.csil_spec = csilgen_common::hoist_inline_composites(
            &input.csil_spec,
            csilgen_common::HoistOptions {
                hoist_all_literal_choices: false,
            },
        );
        cloned
    };
    let input = &hoisted;

    let (want_client, want_server) = match input.config.target.as_str() {
        "typescript-typesonly" => (false, false),
        "typescript-client" => (true, false),
        "typescript-server" => (false, true),
        // "typescript" and any alias emit everything
        _ => (true, true),
    };
    // A service-less spec has nothing for `client.gen.ts`/`server.gen.ts` to route
    // to. Both files still unconditionally reference the synthetic `ServiceError`
    // (thrown by the router's default case / used in JSDoc), but `types.gen.ts`
    // only exports it when the spec declares services, so a service-less "full"
    // build previously emitted a `server.gen.ts` (and would emit `client.gen.ts`
    // for any channel-carrying spec) that imported a name the types module never
    // exported. Go and Python already skip client/server emission entirely for
    // service-less specs (see `wasm/csilgen-go-generator/src/lib.rs` and
    // `wasm/csilgen-python-generator/src/lib.rs`); mirror that instead of forcing
    // `types.gen.ts` to export an unused `ServiceError` just to patch the import.
    let (want_client, want_server) = (
        want_client && common::has_services(&input.csil_spec),
        want_server && common::has_services(&input.csil_spec),
    );

    let mut files = vec![GeneratedFile {
        path: "types.gen.ts".to_string(),
        content: types::generate(input),
    }];

    // The codec carries every record's wire (de)serializers. It rides alongside the
    // types for every surface (a typesonly consumer still gets usable codecs, and the
    // client imports its `to<T>Cbor`/`from<T>Cbor` directly).
    if let Some(content) = codec::generate(input) {
        files.push(GeneratedFile {
            path: "codec.gen.ts".to_string(),
            content,
        });
    }

    // `Both` (the default) ships the blocking client at `client.gen.ts` and an async
    // twin at `client.async.gen.ts`; `Async` makes the async client a drop-in at
    // `client.gen.ts`; `Sync` is the original blocking-only output. The async twin
    // carries an `Async` marker on its symbols so both coexist in one barrel.
    if want_client {
        let sync = common::ClientShape {
            is_async: false,
            marker: "",
        };
        let async_drop_in = common::ClientShape {
            is_async: true,
            marker: "",
        };
        let async_twin = common::ClientShape {
            is_async: true,
            marker: "Async",
        };
        match style {
            common::ClientStyle::Sync => files.push(GeneratedFile {
                path: "client.gen.ts".to_string(),
                content: client::generate(input, sync)?,
            }),
            common::ClientStyle::Async => files.push(GeneratedFile {
                path: "client.gen.ts".to_string(),
                content: client::generate(input, async_drop_in)?,
            }),
            common::ClientStyle::Both => {
                files.push(GeneratedFile {
                    path: "client.gen.ts".to_string(),
                    content: client::generate(input, sync)?,
                });
                files.push(GeneratedFile {
                    path: "client.async.gen.ts".to_string(),
                    content: client::generate(input, async_twin)?,
                });
            }
        }
    }
    if want_server {
        files.push(GeneratedFile {
            path: "server.gen.ts".to_string(),
            content: server::generate(input)?,
        });
    }

    // When opted in, turn the output directory into a publishable npm package by
    // appending a barrel + package.json + tsconfig.json. Default output is unchanged.
    package::augment(input, &mut files);

    Ok(files)
}
