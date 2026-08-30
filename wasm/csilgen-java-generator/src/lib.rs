//! Java code generator for csilgen (WASM module).
//!
//! Discovered dynamically as `--target java` from `csilgen_java_generator.wasm`.
//! Emits idiomatic Java 17 source — records for data groups, sealed interfaces for
//! choices, a typed client, a server handler interface, and verbose/compact channel
//! routers — but never wire bytes (the transport library owns the wire).

use convert_case::{Case, Casing};
use csilgen_common::{
    ChoiceClass, CsilControlOperator, CsilGroupEntry, CsilGroupExpression, CsilGroupKey,
    CsilLiteralValue, CsilOccurrence, CsilRuleType, CsilServiceDefinition, CsilServiceDirection,
    CsilServiceOperation, CsilSizeConstraint, CsilTypeExpression, CsilValidationConstraint,
    GeneratedFile, GenerationStats, GeneratorCapability, GeneratorMetadata, WasmGeneratorInput,
    WasmGeneratorOutput, wasm_interface::*,
};

// ---------------------------------------------------------------------------
// WASM exports
// ---------------------------------------------------------------------------

#[unsafe(no_mangle)]
pub extern "C" fn get_metadata() -> *const u8 {
    let metadata = GeneratorMetadata {
        name: "java-generator".to_string(),
        version: "1.0.0".to_string(),
        description: "Java code generator".to_string(),
        target: "java".to_string(),
        capabilities: vec![
            GeneratorCapability::BasicTypes,
            GeneratorCapability::ComplexStructures,
            GeneratorCapability::Services,
            GeneratorCapability::FieldMetadata,
            GeneratorCapability::ValidationConstraints,
        ],
        author: Some("CSIL Team".to_string()),
        homepage: None,
    };
    write_json(&metadata) as *const u8
}

#[unsafe(no_mangle)]
pub extern "C" fn allocate(size: usize) -> *mut u8 {
    let mut buf = Vec::with_capacity(size);
    let ptr = buf.as_mut_ptr();
    std::mem::forget(buf);
    ptr
}

#[unsafe(no_mangle)]
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn deallocate(ptr: *mut u8, size: usize) {
    if !ptr.is_null() && size > 0 {
        unsafe {
            let _ = Vec::from_raw_parts(ptr, 0, size);
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn generate(input_ptr: *const u8, input_len: usize) -> *mut u8 {
    match process_generation(input_ptr, input_len) {
        Ok(output) => write_json(&output),
        Err(_) => std::ptr::null_mut(),
    }
}

fn write_json<T: serde::Serialize>(value: &T) -> *mut u8 {
    let json = match serde_json::to_string(value) {
        Ok(j) => j,
        Err(_) => return std::ptr::null_mut(),
    };
    let bytes = json.as_bytes();
    let ptr = allocate(bytes.len() + 4);
    if ptr.is_null() {
        return std::ptr::null_mut();
    }
    unsafe {
        std::ptr::write(ptr as *mut u32, bytes.len() as u32);
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr.add(4), bytes.len());
    }
    ptr
}

fn process_generation(input_ptr: *const u8, input_len: usize) -> Result<WasmGeneratorOutput, i32> {
    if input_ptr.is_null() || input_len == 0 || input_len > MAX_INPUT_SIZE {
        return Err(error_codes::INVALID_INPUT);
    }
    let input_slice = unsafe { std::slice::from_raw_parts(input_ptr, input_len) };
    let input_str = std::str::from_utf8(input_slice).map_err(|_| error_codes::INVALID_INPUT)?;
    let input: WasmGeneratorInput =
        serde_json::from_str(input_str).map_err(|_| error_codes::SERIALIZATION_ERROR)?;

    let files = generate_java(&input).map_err(|_| error_codes::GENERATION_ERROR)?;

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
        warnings: Vec::new(),
        stats,
    })
}

// ---------------------------------------------------------------------------
// Generation entry
// ---------------------------------------------------------------------------

/// Which surface a (sub-)target emits: the base `java`/`java-server` produces the
/// handler interface + routers; `java-client` produces the typed client; and
/// `java-typesonly` produces the records/sealed interfaces alone.
enum Surface {
    Server,
    Client,
    TypesOnly,
}

struct JavaConfig {
    package: String,
    surface: Surface,
    /// When set, the output directory is laid out as a publishable Maven project:
    /// a `pom.xml` at the root and sources under `src/main/java/<package path>/`.
    /// Triggered by `emit_packages` containing `"java"`; otherwise the flat default
    /// layout is unchanged.
    package_mode: bool,
    group_id: String,
    artifact_id: String,
    version: String,
}

impl JavaConfig {
    fn from_input(input: &WasmGeneratorInput) -> Result<Self, i32> {
        let opt = |key: &str| input.config.options.get(key).and_then(|v| v.as_str());

        let package = opt("java_package")
            .unwrap_or("csilgen.generated")
            .to_string();

        // An unrecognized sub-target is a hard error, never a silent fall-through,
        // mirroring the validate-early discipline of the Go/Python generators.
        let surface = match input.config.target.as_str() {
            "java" | "java-server" => Surface::Server,
            "java-client" => Surface::Client,
            "java-typesonly" => Surface::TypesOnly,
            _ => return Err(error_codes::GENERATION_ERROR),
        };

        // Maven coordinates: groupId is the reversed-domain package itself; artifactId is
        // the explicit `package_name` or a kebab of the package's last segment; version
        // defaults to the conventional first release.
        let group_id = package.clone();
        // A path-style `package_name` is the cross-ecosystem source of truth; the
        // Maven artifactId wants only its tail. See `package_name_last_segment`.
        let artifact_id = opt("package_name")
            .map(|name| csilgen_common::package_name_last_segment(name).to_string())
            .unwrap_or_else(|| derive_artifact_id(&package));
        let version = opt("package_version").unwrap_or("0.1.0").to_string();

        Ok(Self {
            package,
            surface,
            package_mode: wants_java_package(input),
            group_id,
            artifact_id,
            version,
        })
    }

    /// The relative file path for a top-level public class. In package mode the file
    /// lands under Maven's standard `src/main/java/<package path>/` source root; the
    /// default flat layout keeps the class directly under the package dir.
    fn path_for(&self, class: &str) -> String {
        let pkg = self.package.replace('.', "/");
        if self.package_mode {
            format!("src/main/java/{pkg}/{class}.java")
        } else {
            format!("{pkg}/{class}.java")
        }
    }

    /// The file preamble: the generated-code marker plus the package statement.
    fn header(&self) -> String {
        let pkg = &self.package;
        format!("// Code generated by csilgen; DO NOT EDIT.\n\npackage {pkg};\n\n")
    }
}

/// Whether the caller asked for a self-contained publishable Java package. The trigger
/// is the `emit_packages` option containing `"java"`. Parsed defensively because the
/// option can reach us in several shapes (see `emit_targets`).
fn wants_java_package(input: &WasmGeneratorInput) -> bool {
    input
        .config
        .options
        .get("emit_packages")
        .map(emit_targets)
        .is_some_and(|targets| targets.iter().any(|t| t == "java"))
}

/// Reduce an `emit_packages` option value to the set of target names it names. The value
/// is meant to be a JSON array of strings, but a host may instead hand us the array as a
/// JSON-encoded string, or a bare/comma-separated string; each shape collapses to the
/// same name list rather than being rejected.
fn emit_targets(value: &serde_json::Value) -> Vec<String> {
    match value {
        serde_json::Value::Array(items) => items
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect(),
        serde_json::Value::String(s) => {
            match serde_json::from_str::<serde_json::Value>(s) {
                Ok(serde_json::Value::Array(items)) => items
                    .iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect(),
                // Not a JSON array: treat it as a plain (possibly comma-separated) list.
                _ => s
                    .split(',')
                    .map(|p| p.trim().to_string())
                    .filter(|p| !p.is_empty())
                    .collect(),
            }
        }
        _ => Vec::new(),
    }
}

/// Derive a Maven artifactId from a reversed-domain package when none is given: the
/// package's last segment, kebab-cased to the conventional artifactId style.
fn derive_artifact_id(package: &str) -> String {
    let last = package.rsplit('.').next().unwrap_or(package);
    let kebab = last.to_case(Case::Kebab);
    if kebab.is_empty() {
        "generated".to_string()
    } else {
        kebab
    }
}

/// Escape the five XML metacharacters so a coordinate carrying one stays well-formed in
/// the emitted `pom.xml`.
fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c => out.push(c),
        }
    }
    out
}

/// Build the `pom.xml` for package mode: a minimal, dependency-free Maven project pinned
/// to Java 17 via `maven.compiler.release`, with the resolved coordinates. The sources
/// already sit under Maven's standard `src/main/java` layout, so no build-helper plugin
/// is needed for Maven to find them.
fn generate_pom(config: &JavaConfig) -> GeneratedFile {
    let group = xml_escape(&config.group_id);
    let artifact = xml_escape(&config.artifact_id);
    let version = xml_escape(&config.version);
    let content = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!-- Generated by csilgen; DO NOT EDIT. -->\n\
         <project xmlns=\"http://maven.apache.org/POM/4.0.0\"\n\
         \x20        xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\"\n\
         \x20        xsi:schemaLocation=\"http://maven.apache.org/POM/4.0.0 http://maven.apache.org/xsd/maven-4.0.0.xsd\">\n\
         \x20   <modelVersion>4.0.0</modelVersion>\n\
         \n\
         \x20   <groupId>{group}</groupId>\n\
         \x20   <artifactId>{artifact}</artifactId>\n\
         \x20   <version>{version}</version>\n\
         \x20   <packaging>jar</packaging>\n\
         \n\
         \x20   <properties>\n\
         \x20       <maven.compiler.release>17</maven.compiler.release>\n\
         \x20       <project.build.sourceEncoding>UTF-8</project.build.sourceEncoding>\n\
         \x20   </properties>\n\
         </project>\n"
    );
    GeneratedFile {
        path: "pom.xml".to_string(),
        content,
    }
}

/// Which transport sections the consumer wants in `genquickstart.md`. The
/// `genquickstart_transports` option is a JSON array subset of
/// `["rpc","events","datagrams"]`; unknown entries are ignored and an absent or empty
/// value means "all three", so the document never renders empty.
fn java_wanted_transports(input: &WasmGeneratorInput) -> (bool, bool, bool) {
    match input.config.options.get("genquickstart_transports") {
        Some(serde_json::Value::Array(items)) => {
            let names: std::collections::BTreeSet<&str> =
                items.iter().filter_map(|v| v.as_str()).collect();
            let any = ["rpc", "events", "datagrams"]
                .iter()
                .any(|t| names.contains(t));
            if any {
                (
                    names.contains("rpc"),
                    names.contains("events"),
                    names.contains("datagrams"),
                )
            } else {
                (true, true, true)
            }
        }
        _ => (true, true, true),
    }
}

/// The package `genquickstart.md`: a transport-by-transport Quickstart over the official
/// `csilgen-transport` library. The generated codec owns CBOR (de)serialization and the
/// library owns the envelope/framing/lifecycle; you supply only a *carrier* that moves
/// bytes. Each requested section (CSIL-RPC over HTTP, CSIL-Events over TLS, CSIL-Datagrams
/// over UDP) is a complete, copy-paste example. Sections adapt to the emitted surface: RPC
/// needs the typed client (`java-client`), Events' router needs the server surface
/// (`java`); Datagrams ride any surface.
fn generate_readme(input: &WasmGeneratorInput, config: &JavaConfig) -> GeneratedFile {
    let artifact = &config.artifact_id;
    let mut out = format!(
        "# {artifact}\n\n\
         Generated by csilgen. A typed CSIL client: the generated codec owns CBOR\n\
         (de)serialization and the `csilgen-transport` library owns the envelope, framing, and\n\
         connection lifecycle. You supply only a *carrier* that moves bytes, so the same typed\n\
         surface rides HTTP, TLS, a WebSocket, WebTransport, QUIC, or raw UDP unchanged.\n\n\
         ## Install\n\n\
         This package builds to a standard Maven artifact (`mvn install` to your local repo —\n\
         TODO: publish to a shared repository). Depend on it and on the transport library\n\
         (also unpublished — `mvn install` `transports/java` locally for now):\n\n\
         ```xml\n\
         <dependency>\n\
         \x20   <groupId>{group}</groupId>\n\
         \x20   <artifactId>{artifact}</artifactId>\n\
         \x20   <version>{version}</version>\n\
         </dependency>\n\
         <dependency>\n\
         \x20   <groupId>community.catalyst.csilgen</groupId>\n\
         \x20   <artifactId>csilgen-transport</artifactId>\n\
         \x20   <version>0.1.0</version>\n\
         </dependency>\n\
         ```\n\n",
        group = xml_escape(&config.group_id),
        version = xml_escape(&config.version),
    );

    // The README is emitted only in package mode, where a package carries BOTH the typed
    // client and the generated router (see `generate_java`), so the RPC/Datagrams (client)
    // and Events (router) sections all render live code against the one package — unless the
    // surface is types-only, which ships neither.
    let client_surface = matches!(config.surface, Surface::Client)
        || (config.package_mode && !matches!(config.surface, Surface::TypesOnly));
    let server_surface = matches!(config.surface, Surface::Server)
        || (config.package_mode && !matches!(config.surface, Surface::TypesOnly));
    let unary = first_unary_example(input);
    let channel = first_channel_example(input);
    let (rpc, events, datagrams) = java_wanted_transports(input);
    if rpc {
        out.push_str(&java_rpc_section(config, client_surface, unary.as_ref()));
    }
    if events {
        out.push_str(&java_events_section(
            config,
            server_surface,
            channel.as_ref(),
        ));
    }
    if datagrams {
        out.push_str(&java_datagrams_section(config, unary.as_ref()));
    }

    // Named `genquickstart.md` rather than `README.md` so it never collides with a
    // consumer's own hand-written `README.md`; the consumer supplies that themselves.
    GeneratedFile {
        path: "genquickstart.md".to_string(),
        content: out,
    }
}

/// CSIL-RPC over HTTP: a carrier implementing the generated `Transport` byte seam that
/// builds/parses the envelope with the library's `Rpc.Request`/`Rpc.Response` (never
/// hand-rolled) and POSTs it to `{baseUrl}/csil/v1/rpc` with the JDK's blocking
/// `HttpClient`. Live only on a client surface with a `->` op; otherwise a note.
fn java_rpc_section(
    config: &JavaConfig,
    client_surface: bool,
    ex: Option<&UnaryExample>,
) -> String {
    let mut out = String::from("## CSIL-RPC (HTTP)\n\n");
    out.push_str(
        "Request/response. The library owns the envelope (`Rpc.Request`/`Rpc.Response`); you\n\
         bring a carrier that moves bytes. The HTTP carrier below is just one example — swap\n\
         `HttpClient` for any client (it implements the generated byte seam).\n\n",
    );
    if !client_surface {
        out.push_str(
            "This package was generated as a server/types-only surface, so it ships no typed\n\
             client to call. Regenerate with `--target java-client` for the RPC carrier and\n\
             client.\n\n",
        );
        return out;
    }
    let Some(ex) = ex else {
        out.push_str(
            "This package declares no `->` operations, so there is no RPC call to make.\n\n",
        );
        return out;
    };
    out.push_str("```java\n");
    out.push_str(&format!("package {};\n\n", config.package));
    out.push_str(
        "import community.catalyst.csilgen.transport.Rpc;\n\
         import community.catalyst.csilgen.transport.StatusException;\n\
         import java.io.IOException;\n\
         import java.net.URI;\n\
         import java.net.http.HttpClient;\n\
         import java.net.http.HttpRequest;\n\
         import java.net.http.HttpResponse;\n\n",
    );
    out.push_str(JAVA_RPC_CARRIER);
    out.push('\n');
    out.push_str("public final class RpcExample {\n");
    out.push_str("    public static void main(String[] args) {\n");
    out.push_str(&format!(
        "        {0} client = new {0}(new HttpRpcCarrier(\"http://localhost:5080\"));\n",
        ex.client_class
    ));
    if ex.has_request {
        out.push_str(&format!(
            "        {} resp = client.{}({});\n",
            ex.response_class, ex.method, ex.sample
        ));
    } else {
        out.push_str(&format!(
            "        {} resp = client.{}();\n",
            ex.response_class, ex.method
        ));
    }
    out.push_str("        System.out.println(resp);\n    }\n}\n");
    out.push_str("```\n\n");
    out
}

/// CSIL-Events over TLS: a full session over the library's `Carriers.StreamCarrier`
/// (length-prefix framing), the `$hello`/`$hello-ack` handshake, the `$ping`/`$pong`
/// heartbeat, and — on a server surface with a record `<->` op — dispatch into the
/// generated `<Service>Router.route<Service>Channel`. Without a router the handshake +
/// heartbeat are still shown, with a note where the dispatch wiring would go.
fn java_events_section(
    config: &JavaConfig,
    server_surface: bool,
    ch: Option<&ChannelExample>,
) -> String {
    let mut out = String::from("## CSIL-Events (TLS)\n\n");
    out.push_str(
        "Typed, bidirectional event streams over a long-lived connection. The library owns the\n\
         `$hello`/`$hello-ack` handshake, the `$ping`/`$pong` heartbeat, and length-prefix\n\
         framing; the generated router dispatches typed events. The TLS carrier below is just\n\
         one example — a WebSocket/WebTransport/QUIC carrier drops in unchanged.\n\n",
    );
    // The router is a server-surface symbol, so dispatch is only shown when a server
    // package actually emits one.
    let dispatch = if server_surface { ch } else { None };
    out.push_str("```java\n");
    out.push_str(&format!("package {};\n\n", config.package));
    out.push_str(
        "import community.catalyst.csilgen.transport.Carriers;\n\
         import community.catalyst.csilgen.transport.Conventions;\n\
         import community.catalyst.csilgen.transport.Events;\n\
         import community.catalyst.csilgen.transport.FrameCarrier;\n\
         import java.io.IOException;\n\
         import java.util.List;\n\
         import javax.net.ssl.SSLSocket;\n\
         import javax.net.ssl.SSLSocketFactory;\n\n",
    );
    match dispatch {
        Some(ch) => out.push_str(&java_events_session(ch)),
        None => out.push_str(&java_events_no_channel(server_surface)),
    }
    out.push_str("```\n\n");
    out
}

/// The channel session body for an Events connection with a record `<->` op: a handler +
/// a `Codec` backed by the message record's per-type helpers, the handshake, one outbound
/// event via the generated codec, and the recv loop that heartbeats and dispatches into
/// the generated router.
fn java_events_session(ch: &ChannelExample) -> String {
    format!(
        r#"public final class EventsExample {{
    // The carrier's max-frame guard; see the comment at the carrier construction below.
    static final int MAX_FRAME = Conventions.MAX_FRAME_DEFAULT;

    // A handler for the {service} channel; the generated router dispatches decoded events
    // to it. The interface bundles every service op, so the unary ops are stubbed.
    static final class Handlers implements {iface} {{
{handler_methods}    }}

    // Back the generated router's Codec seam with this package's per-type helpers.
    static final class ExampleCodec implements Codec {{
        @Override
        public byte[] encode(Object value) {{
            return CsilCbor.encode{msg}(({msg}) value);
        }}

        @Override
        public <T> T decode(byte[] data, Class<T> type) {{
            return type.cast(CsilCbor.decode{msg}(data));
        }}
    }}

    public static void main(String[] args) throws IOException {{
        // One example carrier: a TLS byte stream framed with CSIL's 4-byte length prefix.
        // The max-frame guard is a carrier setting, not a generated constant: raise MAX_FRAME
        // when a peer accepts payloads larger than the 16 MiB default (the envelope adds
        // framing and request metadata around the payload, so the limit must exceed the
        // largest payload), or lower it to harden an exposed listener. Valid limits are
        // 1..=Conventions.MAX_FRAME_LIMIT and are checked at construction.
        SSLSocket socket = (SSLSocket) SSLSocketFactory.getDefault().createSocket("localhost", 7443);
        FrameCarrier carrier =
            new Carriers.StreamCarrier(socket.getInputStream(), socket.getOutputStream(), MAX_FRAME);
        Handlers handlers = new Handlers();
        Codec codec = new ExampleCodec();

        // $hello / $hello-ack handshake (control plane). The peer's $hello-ack pins the wire
        // profile for the connection's lifetime.
        carrier.sendFrame(new Events.Hello(List.of(1L), List.of("verbose"), "{service}", null).encode());
        byte[] ackFrame = carrier.recvFrame();
        if (ackFrame == null) {{
            throw new IllegalStateException("connection closed during handshake");
        }}
        Events.HelloAck ack = Events.HelloAck.decode(ackFrame);
        Events.Profile profile = Events.Profile.parse(ack.profile());
        if (profile == null) {{
            profile = Events.Profile.VERBOSE;
        }}

        // Send one outbound event via the generated codec, framed as a verbose Event.
        {msg} outbound = {sample};
        carrier.sendFrame(Events.Event.verbose("{service}", "{op}", CsilCbor.encode{msg}(outbound))
            .encode(profile));

        // Recv loop: decode each frame to an Event, answer $ping with $pong, and dispatch the
        // rest to the generated router.
        for (byte[] frame = carrier.recvFrame(); frame != null; frame = carrier.recvFrame()) {{
            Events.Event ev = Events.Event.decode(frame, profile);
            if (Events.PING_NAME.equals(ev.event())) {{
                Events.Heartbeat ping = Events.Heartbeat.decode(ev.payload());
                carrier.sendFrame(Events.Event.verbose("{service}", Events.PONG_NAME,
                    new Events.Heartbeat(ping.nonce(), null).encode()).encode(profile));
                continue;
            }}
            {router}.{route}(handlers, codec, ev.event(), ev.payload());
        }}
    }}
}}
"#,
        service = ch.service_wire,
        iface = ch.iface,
        handler_methods = ch.handler_methods,
        msg = ch.msg_class,
        op = ch.op_wire,
        sample = ch.sample,
        router = ch.router_class,
        route = ch.route_fn,
    )
}

/// The Events session body when no record channel op exists (or the surface has no
/// router): the handshake and heartbeat still apply, with a note where dispatch would go.
/// References only the transport library, so it compiles on any surface.
fn java_events_no_channel(server_surface: bool) -> String {
    let note = if server_surface {
        "        // This package declares no record `<->`/`<-` channel operations, so there is no\n\
         \x20       // generated router to dispatch typed events into; the handshake + heartbeat\n\
         \x20       // below still apply to any connection.\n"
    } else {
        "        // This package ships no generated channel router (it is not a server surface).\n\
         \x20       // Regenerate with `--target java` for a router + handler interface; the\n\
         \x20       // handshake + heartbeat below apply to any connection.\n"
    };
    format!(
        r#"public final class EventsExample {{
    // The carrier's max-frame guard; see the comment at the carrier construction below.
    static final int MAX_FRAME = Conventions.MAX_FRAME_DEFAULT;

    public static void main(String[] args) throws IOException {{
{note}        // One example carrier: a TLS byte stream framed with CSIL's 4-byte length prefix.
        // The max-frame guard is a carrier setting, not a generated constant: raise MAX_FRAME
        // when a peer accepts payloads larger than the 16 MiB default (the envelope adds
        // framing and request metadata around the payload, so the limit must exceed the
        // largest payload), or lower it to harden an exposed listener. Valid limits are
        // 1..=Conventions.MAX_FRAME_LIMIT and are checked at construction.
        SSLSocket socket = (SSLSocket) SSLSocketFactory.getDefault().createSocket("localhost", 7443);
        FrameCarrier carrier =
            new Carriers.StreamCarrier(socket.getInputStream(), socket.getOutputStream(), MAX_FRAME);

        // $hello / $hello-ack handshake (control plane).
        carrier.sendFrame(new Events.Hello(List.of(1L), List.of("verbose"), null, null).encode());
        byte[] ackFrame = carrier.recvFrame();
        if (ackFrame == null) {{
            throw new IllegalStateException("connection closed during handshake");
        }}
        Events.HelloAck ack = Events.HelloAck.decode(ackFrame);
        Events.Profile profile = Events.Profile.parse(ack.profile());
        if (profile == null) {{
            profile = Events.Profile.VERBOSE;
        }}

        // Recv loop: answer $ping with $pong.
        for (byte[] frame = carrier.recvFrame(); frame != null; frame = carrier.recvFrame()) {{
            Events.Event ev = Events.Event.decode(frame, profile);
            if (Events.PING_NAME.equals(ev.event())) {{
                Events.Heartbeat ping = Events.Heartbeat.decode(ev.payload());
                carrier.sendFrame(Events.Event.verbose(null, Events.PONG_NAME,
                    new Events.Heartbeat(ping.nonce(), null).encode()).encode(profile));
            }}
        }}
    }}
}}
"#
    )
}

/// CSIL-Datagrams over UDP: encode a `->` request with the generated codec, wrap it in the
/// library's `Datagrams.Datagram`, and send it fire-and-forget over the library's
/// `UdpDatagramCarrier`; a recv path decodes an inbound datagram's payload into the
/// RESPONSE type. Live when the first `->` op has record request and response types.
fn java_datagrams_section(config: &JavaConfig, ex: Option<&UnaryExample>) -> String {
    let mut out = String::from("## CSIL-Datagrams (UDP)\n\n");
    out.push_str(
        "Unreliable, unordered, message-oriented. The library owns the `Datagram` envelope;\n\
         you bring a datagram carrier. The UDP carrier below is one example — a WebRTC\n\
         unreliable DataChannel or QUIC datagrams drop in unchanged.\n\n",
    );
    let Some(ex) = ex else {
        out.push_str(
            "This package declares no `->` operations, so there is no datagram payload to encode.\n\n",
        );
        return out;
    };
    let (Some(req), Some(res)) = (&ex.req_class, &ex.res_class) else {
        out.push_str(
            "This package's `->` operations have non-record payloads; (de)serialize them manually before framing.\n\n",
        );
        return out;
    };
    out.push_str("```java\n");
    out.push_str(&format!("package {};\n\n", config.package));
    out.push_str(
        "import community.catalyst.csilgen.transport.DatagramCarrier;\n\
         import community.catalyst.csilgen.transport.Datagrams;\n\
         import community.catalyst.csilgen.transport.UdpDatagramCarrier;\n\
         import java.io.IOException;\n\
         import java.net.DatagramSocket;\n\
         import java.net.InetSocketAddress;\n\n",
    );
    out.push_str(&format!(
        r#"public final class DatagramsExample {{
    // The operation's datagram ordinal — its @wire-id, or a channel-agreed number.
    static final long OP_ORD = {ord}L;

    // Fire-and-forget: encode the `->` request via the generated codec, wrap it in the
    // library's Datagram, and send. seq 0 marks an unsequenced datagram.
    static void sendRequest(DatagramCarrier carrier, {req} request) throws IOException {{
        carrier.sendDatagram(Datagrams.Datagram.of(OP_ORD, 0, CsilCbor.encode{req}(request)).encode());
    }}

    // Recv path: a datagram of the RESPONSE type MAY arrive later — or never. There is NO
    // synchronous response; the caller must tolerate loss and reordering.
    static {res} recvResponse(DatagramCarrier carrier) throws IOException {{
        byte[] inbound = carrier.recvDatagram();
        if (inbound == null) {{
            return null;
        }}
        Datagrams.Datagram dg = Datagrams.Datagram.decode(inbound);
        return CsilCbor.decode{res}(dg.payload());
    }}

    public static void main(String[] args) throws IOException {{
        // One example carrier: the library's UDP DatagramCarrier over a connected socket.
        DatagramSocket socket = new DatagramSocket();
        socket.connect(new InetSocketAddress("localhost", 9000));
        DatagramCarrier carrier = new UdpDatagramCarrier(socket);
        sendRequest(carrier, {sample});
        // No synchronous reply: a response datagram may arrive later, or never.
        {res} resp = recvResponse(carrier);
        System.out.println(resp);
    }}
}}
"#,
        ord = ex.op_ord,
        req = req,
        res = res,
        sample = ex.sample,
    ));
    out.push_str("```\n\n");
    out
}

/// The HTTP carrier body — spec-independent, so a constant. It encodes the request with
/// the library's `Rpc.Request`, POSTs it to `{baseUrl}/csil/v1/rpc` with the JDK's blocking
/// `HttpClient`, and returns the success payload bytes the typed client decodes. A non-zero
/// transport status (via `Rpc.Response.asTransportError`) or a typed `ServiceError` arm
/// becomes a `ClientException`.
const JAVA_RPC_CARRIER: &str = r#"// One example carrier: CSIL-RPC over an HTTP POST. The library owns the envelope
// (Rpc.Request/Rpc.Response); the carrier owns only the transport.
final class HttpRpcCarrier implements Transport {
    private final HttpClient http = HttpClient.newHttpClient();
    private final String baseUrl;

    HttpRpcCarrier(String baseUrl) {
        // Trim any trailing slash so the joined path is exactly one "/csil/v1/rpc".
        this.baseUrl = baseUrl.replaceAll("/+$", "");
    }

    @Override
    public byte[] call(String service, String op, byte[] req) throws ClientException {
        // The library builds the request envelope; the carrier never hand-rolls CBOR.
        byte[] envelope = Rpc.Request.of(service, op, req == null ? new byte[0] : req).encode();
        HttpResponse<byte[]> resp;
        try {
            HttpRequest httpReq = HttpRequest.newBuilder()
                .uri(URI.create(baseUrl + "/csil/v1/rpc"))
                .header("Content-Type", "application/cbor")
                .header("Accept", "application/cbor")
                .POST(HttpRequest.BodyPublishers.ofByteArray(envelope))
                .build();
            resp = http.send(httpReq, HttpResponse.BodyHandlers.ofByteArray());
        } catch (IOException e) {
            throw new ClientException("csil-rpc " + service + "/" + op + ": " + e.getMessage(), e);
        } catch (InterruptedException e) {
            Thread.currentThread().interrupt();
            throw new ClientException("csil-rpc " + service + "/" + op + ": interrupted", e);
        }
        if (resp.statusCode() != 200) {
            throw new ClientException(
                "csil-rpc " + service + "/" + op + ": http " + resp.statusCode());
        }
        // Rpc.Response.asTransportError() is non-null for any non-zero transport status,
        // distinct from a typed application error.
        Rpc.Response decoded = Rpc.Response.decode(resp.body());
        StatusException te = decoded.asTransportError();
        if (te != null) {
            throw new ClientException("csil-rpc " + service + "/" + op + ": " + te.getMessage(), te);
        }
        // A typed application error rides as a status-0 "ServiceError" variant; surface it
        // so the typed client decodes success only.
        if ("ServiceError".equals(decoded.variant())) {
            throw new ClientException("csil-rpc " + service + "/" + op + ": ServiceError");
        }
        return decoded.payload();
    }
}
"#;

/// The pieces the RPC + Datagram examples need: the client class + method to call, the
/// typed response class to print, a compiling sample request literal (empty when the op
/// takes no request), the request/response record class names (so the datagram section can
/// name `CsilCbor.encode<Req>`/`decode<Res>`), and the op's datagram ordinal.
struct UnaryExample {
    client_class: String,
    method: String,
    response_class: String,
    has_request: bool,
    sample: String,
    req_class: Option<String>,
    res_class: Option<String>,
    op_ord: u64,
}

/// The first service (in rule order, matching the emitted client) that has a unary `->`
/// operation the typed client actually exposes — success and request both records (or a
/// null request) — reduced to an example call. `None` for a serviceless package.
fn first_unary_example(input: &WasmGeneratorInput) -> Option<UnaryExample> {
    let records = record_names(input);
    for rule in &input.csil_spec.rules {
        let CsilRuleType::ServiceDef(def) = &rule.rule_type else {
            continue;
        };
        for op in &def.operations {
            if !matches!(op.direction, CsilServiceDirection::Unidirectional) {
                continue;
            }
            let success = success_type(&op.output_type);
            let null_input = is_null_input(&op.input_type);
            if !is_record_ref(&success, &records)
                || !(null_input || is_record_ref(&op.input_type, &records))
            {
                continue;
            }
            return Some(UnaryExample {
                client_class: format!("{}Client", service_base(&rule.name)),
                method: op.name.to_case(Case::Camel),
                response_class: record_ref_class(&success),
                has_request: !null_input,
                sample: if null_input {
                    String::new()
                } else {
                    java_sample(input, &op.input_type)
                },
                req_class: is_record_ref(&op.input_type, &records)
                    .then(|| record_ref_class(&op.input_type)),
                res_class: is_record_ref(&success, &records).then(|| record_ref_class(&success)),
                // The datagram ordinal is the op's @wire-id when present; otherwise a
                // channel-agreed placeholder the user fills in.
                op_ord: op.wire_id.unwrap_or(1),
            });
        }
    }
    None
}

/// The pieces the Events session needs: the generated handler interface + router names,
/// the channel message record class, the full handler stub bodies, a sample message
/// literal, and the wire service/op strings.
struct ChannelExample {
    service_wire: String,
    op_wire: String,
    iface: String,
    router_class: String,
    route_fn: String,
    msg_class: String,
    handler_methods: String,
    sample: String,
}

/// The first service (in rule order) with a `<->` op whose input and output are both
/// records (so the generated router + per-type codec helpers exist). `None` when no
/// service has a usable channel op — the Events section then shows the handshake/heartbeat
/// without dispatch wiring. The router dispatches the op's input type, so that is the
/// message record.
fn first_channel_example(input: &WasmGeneratorInput) -> Option<ChannelExample> {
    let records = record_names(input);
    for rule in &input.csil_spec.rules {
        let CsilRuleType::ServiceDef(def) = &rule.rule_type else {
            continue;
        };
        for op in &def.operations {
            if !matches!(op.direction, CsilServiceDirection::Bidirectional) {
                continue;
            }
            let success = success_type(&op.output_type);
            if !is_record_ref(&op.input_type, &records) || !is_record_ref(&success, &records) {
                continue;
            }
            let iface = rule.name.to_case(Case::Pascal);
            return Some(ChannelExample {
                service_wire: rule.name.clone(),
                op_wire: op.name.clone(),
                iface: iface.clone(),
                router_class: format!("{iface}Router"),
                route_fn: format!("route{iface}Channel"),
                msg_class: record_ref_class(&op.input_type),
                handler_methods: java_handler_stub_methods(def),
                sample: java_sample(input, &op.input_type),
            });
        }
    }
    None
}

/// The `@Override` method bodies a `Handlers` class needs to satisfy the *whole* generated
/// service handler interface (the router dispatches into one handler that must implement
/// every op): a unary op is a one-line `throw` stub (so any return type type-checks), a
/// `<->` op prints, and a `<-` op contributes no inbound method. Indented for a nested
/// static class.
fn java_handler_stub_methods(def: &CsilServiceDefinition) -> String {
    let mut out = String::new();
    for op in &def.operations {
        let camel = op.name.to_case(Case::Camel);
        match op.direction {
            CsilServiceDirection::Unidirectional => {
                let output = map_type_boxed(&success_type(&op.output_type));
                if is_null_input(&op.input_type) {
                    out.push_str(&format!(
                        "        @Override\n        public {output} {camel}() {{\n            throw new UnsupportedOperationException();\n        }}\n"
                    ));
                } else {
                    let input = map_type(&op.input_type);
                    out.push_str(&format!(
                        "        @Override\n        public {output} {camel}({input} req) {{\n            throw new UnsupportedOperationException();\n        }}\n"
                    ));
                }
            }
            CsilServiceDirection::Bidirectional => {
                let input = map_type(&op.input_type);
                out.push_str(&format!(
                    "        @Override\n        public void {camel}({input} msg) {{\n            System.out.println(\"event {camel} \" + msg);\n        }}\n"
                ));
            }
            CsilServiceDirection::Reverse => {}
        }
    }
    out
}

/// A compiling Java expression producing a sample value of `ty` for the README example.
/// Records recurse into their canonical constructor; scalars get a representative
/// literal; maps/lists use the empty target-typed factories; shapes a generic sample
/// can't fabricate fall back to `null`, which a reference-typed component accepts.
fn java_sample(input: &WasmGeneratorInput, ty: &CsilTypeExpression) -> String {
    match ty {
        CsilTypeExpression::Builtin(name) => match name.as_str() {
            "text" | "tstr" => "\"example\"".to_string(),
            "bool" => "false".to_string(),
            "bytes" | "bstr" => "new byte[0]".to_string(),
            "int" | "uint" | "nint" => "0L".to_string(),
            "float" | "float64" | "double" => "0.0".to_string(),
            "timestamp" => "java.time.Instant.now()".to_string(),
            "decimal" => "java.math.BigDecimal.ZERO".to_string(),
            _ => "null".to_string(),
        },
        CsilTypeExpression::Array { .. } => "java.util.List.of()".to_string(),
        CsilTypeExpression::Map { .. } => "java.util.Map.of()".to_string(),
        CsilTypeExpression::Constrained { base_type, .. } => java_sample(input, base_type),
        CsilTypeExpression::Reference(name) => match find_record(input, name) {
            Some(group) => record_literal(input, name, group),
            None => match find_alias(input, name) {
                // A transparent alias is a one-component wrapper record over its target.
                Some(underlying) => format!(
                    "new {}({})",
                    name.to_case(Case::Pascal),
                    java_sample(input, &underlying)
                ),
                None => "null".to_string(),
            },
        },
        _ => "null".to_string(),
    }
}

/// `new Class(arg, ...)` over a record's canonical constructor: every named component in
/// declared order, optional components passed as `null`, required ones a typed sample.
fn record_literal(input: &WasmGeneratorInput, name: &str, group: &CsilGroupExpression) -> String {
    let args: Vec<String> = group
        .entries
        .iter()
        .filter(|e| entry_field_name(e).is_some())
        .map(|e| {
            if matches!(e.occurrence, Some(CsilOccurrence::Optional)) {
                "null".to_string()
            } else {
                java_sample(input, &e.value_type)
            }
        })
        .collect();
    format!("new {}({})", name.to_case(Case::Pascal), args.join(", "))
}

/// The record group a reference names, if any: a `Name = { ... }` rule (`TypeDef(Group)`)
/// or a bare group rule (`GroupDef`).
fn find_record<'a>(input: &'a WasmGeneratorInput, name: &str) -> Option<&'a CsilGroupExpression> {
    input.csil_spec.rules.iter().find_map(|r| {
        if r.name != name {
            return None;
        }
        match &r.rule_type {
            CsilRuleType::GroupDef(g) => Some(g),
            CsilRuleType::TypeDef(CsilTypeExpression::Group(g)) => Some(g),
            _ => None,
        }
    })
}

/// The underlying type of a transparent alias a reference names (its wrapper record's one
/// component), or `None` when the name is not such an alias.
fn find_alias(input: &WasmGeneratorInput, name: &str) -> Option<CsilTypeExpression> {
    input.csil_spec.rules.iter().find_map(|r| {
        if r.name != name {
            return None;
        }
        match &r.rule_type {
            CsilRuleType::TypeDef(t) => match t {
                CsilTypeExpression::Group(_) | CsilTypeExpression::Choice(_) => None,
                other => Some(other.clone()),
            },
            _ => None,
        }
    })
}

fn generate_java(input: &WasmGeneratorInput) -> Result<Vec<GeneratedFile>, i32> {
    let config = JavaConfig::from_input(input)?;
    // Inline group/choice fields have no named rule to hang a Java class and codec on, so
    // synthesize one per inline shape up front (the shared `csilgen_common::hoist` pass);
    // everything downstream then sees only named rules and references, no anonymous-
    // composite special cases. An all-literal choice is left inline
    // (`hoist_all_literal_choices: false`) — it already renders as a bare record wrapper
    // via `map_type`/`java_enc_value`/`java_dec_value`'s `Choice` handling, so there is
    // nothing to hoist.
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
    let mut files = Vec::new();

    for rule in &input.csil_spec.rules {
        let doc = &rule.doc_comments;
        match &rule.rule_type {
            CsilRuleType::GroupDef(group) => {
                files.push(generate_record(&config, &rule.name, group, doc));
            }
            CsilRuleType::TypeDef(CsilTypeExpression::Group(group)) => {
                files.push(generate_record(&config, &rule.name, group, doc));
            }
            CsilRuleType::TypeDef(type_expr) => {
                files.push(generate_alias(&config, &rule.name, type_expr, doc));
            }
            CsilRuleType::TypeChoice(choices) => {
                files.push(generate_type_choice(&config, &rule.name, choices, doc));
            }
            CsilRuleType::GroupChoice(choices) => {
                files.push(generate_group_choice(&config, &rule.name, choices, doc));
            }
            CsilRuleType::ServiceDef(_) => {}
        }
    }

    // The self-contained per-record CBOR codec is emitted on every surface whenever the
    // spec has record types: that codec is what every payload (de)serializes through now
    // (the typed client owns the wire; no reflection path remains).
    if let Some(codec) = generate_codec(input, &config) {
        files.push(codec);
    }

    // Service surfaces are dispatched by sub-target.
    let services: Vec<(&str, &CsilServiceDefinition, &[String])> = input
        .csil_spec
        .rules
        .iter()
        .filter_map(|r| match &r.rule_type {
            CsilRuleType::ServiceDef(def) => {
                Some((r.name.as_str(), def, r.doc_comments.as_slice()))
            }
            _ => None,
        })
        .collect();

    // A package's `genquickstart.md` exercises the calling side (RPC + Datagrams, over the
    // typed client) AND the handling side (Events, over the generated router), so a
    // publishable package must carry BOTH surfaces for its own quickstart to compile —
    // regardless of which (sub-)target was requested. This mirrors the OCaml generator,
    // which emits both `client.ml` and `services.ml` in package mode. A flat (non-package)
    // build stays byte-identical: it emits only the requested surface.
    let want_client = matches!(config.surface, Surface::Client)
        || (config.package_mode && !matches!(config.surface, Surface::TypesOnly));
    let want_server = matches!(config.surface, Surface::Server)
        || (config.package_mode && !matches!(config.surface, Surface::TypesOnly));

    if want_client && !services.is_empty() {
        let records = record_names(input);
        let aliases = codec_aliases(input);
        let choices = codec_choices(input);
        files.push(generate_transport_iface(&config));
        files.push(generate_client_error(&config));
        for (name, def, doc) in &services {
            files.push(generate_client(
                &config, name, def, doc, &records, &aliases, &choices,
            ));
        }
    }
    if want_server {
        let any_channel = services.iter().any(|(_, d, _)| service_has_channel_ops(d));
        let any_encoder = services.iter().any(|(_, d, _)| service_has_pushable_ops(d));
        if any_channel {
            files.push(generate_codec_iface(&config));
        }
        if any_encoder {
            files.push(generate_encoded_message(&config));
        }
        for (name, def, doc) in &services {
            files.push(generate_server_interface(&config, name, def, doc));
            if service_has_channel_ops(def) || def.wire_id.is_some() {
                files.push(generate_router(&config, name, def));
            }
        }
    }

    // The emit functions reference JDK types by their fully-qualified name; hoist those to
    // `import` statements and leave simple names behind, the way a Java author writes them.
    let mut files: Vec<GeneratedFile> = files
        .into_iter()
        .map(|mut f| {
            f.content = finalize_file(&f.content);
            f
        })
        .collect();

    // In package mode the output directory is a publishable Maven project: the sources are
    // already laid under `src/main/java/...` by `path_for`, so the only addition is the
    // build descriptor. Emitted after import-hoisting since the pom is not Java source.
    if config.package_mode {
        files.push(generate_pom(&config));
        // Only an explicit `emit_readme: false` suppresses the README; absent or non-bool
        // leaves the publishable package's Quickstart in place.
        if input
            .config
            .options
            .get("emit_readme")
            .and_then(|v| v.as_bool())
            != Some(false)
        {
            files.push(generate_readme(input, &config));
        }
    }

    Ok(files)
}

/// Hoist inline FQNs to imports and drop the blank line a per-member emit leaves before
/// the closing class brace, matching what a formatter would produce.
fn finalize_file(content: &str) -> String {
    let mut out = hoist_imports(content);
    while out.ends_with("\n\n}\n") {
        out.replace_range(out.len() - 4.., "\n}\n");
    }
    out
}

/// The fully-qualified JDK types the emit functions write inline. After a file body is
/// assembled they are lifted into a single alphabetized `import` block and referred to by
/// simple name. Each prefix is an unambiguous class name, so a plain textual replace is
/// safe (none is a substring of a generated identifier we also emit).
const KNOWN_IMPORTS: &[&str] = &[
    "java.io.ByteArrayOutputStream",
    "java.math.BigDecimal",
    "java.math.BigInteger",
    "java.nio.charset.StandardCharsets",
    "java.time.Instant",
    "java.util.ArrayList",
    "java.util.Arrays",
    "java.util.LinkedHashMap",
    "java.util.List",
    "java.util.Map",
    "java.util.Objects",
    "java.util.function.Function",
    "java.util.regex.Pattern",
];

/// Rewrite a finished file so inline FQNs become imports + simple names.
fn hoist_imports(content: &str) -> String {
    let mut used: Vec<&str> = KNOWN_IMPORTS
        .iter()
        .copied()
        .filter(|fqn| content.contains(fqn))
        .collect();
    if used.is_empty() {
        return content.to_string();
    }
    used.sort_unstable();

    let mut body = content.to_string();
    for fqn in &used {
        let simple = fqn.rsplit('.').next().unwrap();
        body = body.replace(fqn, simple);
    }

    // Splice the import block between the package statement and the first declaration.
    let Some(anchor) = body.find(";\n\n") else {
        return body;
    };
    let cut = anchor + 3;
    let imports: String = used.iter().map(|i| format!("import {i};\n")).collect();
    format!("{}{imports}\n{}", &body[..cut], &body[cut..])
}

// ---------------------------------------------------------------------------
// Records
// ---------------------------------------------------------------------------

fn generate_record(
    config: &JavaConfig,
    name: &str,
    group: &CsilGroupExpression,
    doc: &[String],
) -> GeneratedFile {
    let class = name.to_case(Case::Pascal);
    let mut code = config.header();

    let named: Vec<(&CsilGroupEntry, String)> = group
        .entries
        .iter()
        .filter_map(|e| entry_field_name(e).map(|n| (e, n)))
        .collect();

    code.push_str(&type_javadoc(doc, &named));
    code.push_str(&format!("public record {class}(\n"));
    if named.is_empty() {
        // A record needs at least an empty component list; an empty record is legal.
        code.push_str(") {\n}\n");
        return GeneratedFile {
            path: config.path_for(&class),
            content: code,
        };
    }

    let mut comps = Vec::new();
    for (entry, field) in &named {
        let optional = matches!(entry.occurrence, Some(CsilOccurrence::Optional));
        // The CBOR wire keys by the CSIL field name verbatim; the camelCase Java
        // component name is purely the in-memory identifier, so the original name is
        // recorded in a comment for the reader.
        let wire = entry_wire_name(entry).unwrap_or_else(|| field.clone());
        let jtype = if optional {
            map_type_boxed(&entry.value_type)
        } else {
            map_type(&entry.value_type)
        };
        comps.push(format!("    {jtype} {field} /* wire: \"{wire}\" */"));
    }
    code.push_str(&comps.join(",\n"));
    code.push_str("\n) {\n");

    // Validation runs in the canonical constructor: throwing IllegalArgumentException
    // on a violated size/regex/bound is the idiomatic Java guard for a bad value.
    let validation = record_validation(&named);
    if !validation.is_empty() {
        code.push_str(&format!("    public {class} {{\n"));
        code.push_str(&validation);
        code.push_str("    }\n");
    }

    // A record's generated equals/hashCode compare a byte[] component by reference,
    // so a value-equal payload would falsely differ; override them to compare the
    // bytes by content whenever a byte[] component is present.
    if named.iter().any(|(e, _)| {
        !matches!(e.occurrence, Some(CsilOccurrence::Optional))
            && map_type(&e.value_type) == "byte[]"
    }) || named
        .iter()
        .any(|(e, _)| map_type_boxed(&e.value_type) == "byte[]")
    {
        code.push_str(&record_array_equality(&class, &named));
    }

    code.push_str("}\n");
    GeneratedFile {
        path: config.path_for(&class),
        content: code,
    }
}

/// Build the canonical-constructor validation body for a record's named fields.
fn record_validation(named: &[(&CsilGroupEntry, String)]) -> String {
    let mut body = String::new();
    for (entry, field) in named {
        let optional = matches!(entry.occurrence, Some(CsilOccurrence::Optional));
        let jtype = map_type(&entry.value_type);
        // Both constraint systems feed the same guards: `@`-annotations and the
        // inline `.`-control-operators on the field type.
        for meta in &entry.metadata {
            if let csilgen_common::CsilFieldMetadata::Constraint(c) = meta {
                body.push_str(&annotation_guard(field, &jtype, optional, c));
            }
        }
        if let CsilTypeExpression::Constrained { constraints, .. } = &entry.value_type {
            for op in constraints {
                body.push_str(&control_op_guard(field, &jtype, optional, op));
            }
        }
    }
    body
}

/// The length expression for a value of the given Java type: strings use
/// `.length()`, byte arrays `.length`, and lists `.size()`.
fn len_expr(field: &str, jtype: &str) -> String {
    if jtype == "byte[]" {
        format!("{field}.length")
    } else if jtype.starts_with("java.util.List") || jtype.starts_with("java.util.Map") {
        format!("{field}.size()")
    } else {
        format!("{field}.length()")
    }
}

/// A guard short-circuited by a null-check when the field is an optional (boxed)
/// component, so an absent optional is skipped rather than dereferenced.
fn guard(field: &str, optional: bool, cond: &str, message: &str) -> String {
    let msg = java_string(message);
    let test = if optional {
        format!("{field} != null && ({cond})")
    } else {
        cond.to_string()
    };
    format!(
        "        if ({test}) {{\n            throw new IllegalArgumentException({msg});\n        }}\n"
    )
}

fn annotation_guard(
    field: &str,
    jtype: &str,
    optional: bool,
    c: &CsilValidationConstraint,
) -> String {
    let len = len_expr(field, jtype);
    match c {
        // A length/item count is never negative, so a `>= 0` floor is vacuous; skip it.
        CsilValidationConstraint::MinLength(0) => String::new(),
        CsilValidationConstraint::MinLength(n) => guard(
            field,
            optional,
            &format!("{len} < {n}"),
            &format!("field '{field}' must have length >= {n}"),
        ),
        CsilValidationConstraint::MaxLength(n) => guard(
            field,
            optional,
            &format!("{len} > {n}"),
            &format!("field '{field}' must have length <= {n}"),
        ),
        CsilValidationConstraint::MinItems(0) => String::new(),
        CsilValidationConstraint::MinItems(n) => guard(
            field,
            optional,
            &format!("{len} < {n}"),
            &format!("field '{field}' must have at least {n} items"),
        ),
        CsilValidationConstraint::MaxItems(n) => guard(
            field,
            optional,
            &format!("{len} > {n}"),
            &format!("field '{field}' must have at most {n} items"),
        ),
        CsilValidationConstraint::MinValue(v) => {
            ordered_guard(field, jtype, optional, "<", "at least", v)
        }
        CsilValidationConstraint::MaxValue(v) => {
            ordered_guard(field, jtype, optional, ">", "at most", v)
        }
        CsilValidationConstraint::Custom { .. } => String::new(),
    }
}

fn control_op_guard(field: &str, jtype: &str, optional: bool, op: &CsilControlOperator) -> String {
    match op {
        CsilControlOperator::Size(size) => size_guard(field, jtype, optional, size),
        CsilControlOperator::Regex(pattern) => guard(
            field,
            optional,
            &format!(
                "!java.util.regex.Pattern.compile({}).matcher({field}).find()",
                java_string(pattern)
            ),
            &format!("field '{field}' must match pattern {pattern}"),
        ),
        CsilControlOperator::GreaterEqual(v) => ordered_guard(field, jtype, optional, "<", ">=", v),
        CsilControlOperator::LessEqual(v) => ordered_guard(field, jtype, optional, ">", "<=", v),
        CsilControlOperator::GreaterThan(v) => ordered_guard(field, jtype, optional, "<=", ">", v),
        CsilControlOperator::LessThan(v) => ordered_guard(field, jtype, optional, ">=", "<", v),
        CsilControlOperator::Equal(v) => ordered_guard(field, jtype, optional, "!=", "==", v),
        CsilControlOperator::NotEqual(v) => ordered_guard(field, jtype, optional, "==", "!=", v),
        // Defaults and encoding-only operators are not runtime checks.
        _ => String::new(),
    }
}

fn size_guard(field: &str, jtype: &str, optional: bool, size: &CsilSizeConstraint) -> String {
    let len = len_expr(field, jtype);
    match size {
        CsilSizeConstraint::Exact(n) => guard(
            field,
            optional,
            &format!("{len} != {n}"),
            &format!("field '{field}' must have length {n}"),
        ),
        CsilSizeConstraint::Min(0) => String::new(),
        CsilSizeConstraint::Min(n) => guard(
            field,
            optional,
            &format!("{len} < {n}"),
            &format!("field '{field}' must have length >= {n}"),
        ),
        CsilSizeConstraint::Max(n) => guard(
            field,
            optional,
            &format!("{len} > {n}"),
            &format!("field '{field}' must have length <= {n}"),
        ),
        CsilSizeConstraint::Range { min, max } => {
            // A zero floor on a length is vacuous; emit only the meaningful upper bound.
            let mut out = if *min == 0 {
                String::new()
            } else {
                guard(
                    field,
                    optional,
                    &format!("{len} < {min}"),
                    &format!("field '{field}' must have length >= {min}"),
                )
            };
            out.push_str(&guard(
                field,
                optional,
                &format!("{len} > {max}"),
                &format!("field '{field}' must have length <= {max}"),
            ));
            out
        }
    }
}

/// Emit one ordered comparison honoring the field's Java type. `op` is the operator
/// whose truth means the value is invalid; `desc` is the human phrasing. Numeric
/// fields compare directly; `BigDecimal` compares via `compareTo`; an `Instant`
/// compares via `isBefore`/`isAfter`.
fn ordered_guard(
    field: &str,
    jtype: &str,
    optional: bool,
    op: &str,
    desc: &str,
    value: &CsilLiteralValue,
) -> String {
    match jtype {
        // A boolean only admits equality; comparing it to a numeric 0 would not compile,
        // and ordering (`<`/`>`) is meaningless, so only `==`/`!=` produce a guard.
        "boolean" => {
            let expected = match value {
                CsilLiteralValue::Bool(b) => *b,
                _ => return String::new(),
            };
            let cond = match op {
                "!=" => {
                    if expected {
                        format!("!{field}")
                    } else {
                        field.to_string()
                    }
                }
                "==" => {
                    if expected {
                        field.to_string()
                    } else {
                        format!("!{field}")
                    }
                }
                _ => return String::new(),
            };
            guard(
                field,
                optional,
                &cond,
                &format!("field '{field}' must be {desc} {expected}"),
            )
        }
        "java.math.BigDecimal" => {
            let Some(text) = literal_as_text(value) else {
                return String::new();
            };
            let bound = format!("new java.math.BigDecimal({})", java_string(&text));
            guard(
                field,
                optional,
                &format!("{field}.compareTo({bound}) {op} 0"),
                &format!("field '{field}' must be {desc} {text}"),
            )
        }
        "java.time.Instant" => {
            let Some(text) = literal_as_text(value) else {
                return String::new();
            };
            let bound = format!("java.time.Instant.parse({})", java_string(&text));
            let cond = match op {
                "<" => format!("{field}.isBefore({bound})"),
                ">" => format!("{field}.isAfter({bound})"),
                "<=" => format!("!{field}.isAfter({bound})"),
                ">=" => format!("!{field}.isBefore({bound})"),
                "==" => format!("{field}.equals({bound})"),
                "!=" => format!("!{field}.equals({bound})"),
                _ => return String::new(),
            };
            guard(
                field,
                optional,
                &cond,
                &format!("field '{field}' must be {desc} {text}"),
            )
        }
        _ => {
            let v = literal_as_number(value);
            guard(
                field,
                optional,
                &format!("{field} {op} {v}"),
                &format!("field '{field}' must be {desc} {v}"),
            )
        }
    }
}

/// Override `equals`/`hashCode`/`toString` so byte[] components compare by content.
fn record_array_equality(class: &str, named: &[(&CsilGroupEntry, String)]) -> String {
    let mut eq = String::new();
    let mut hashes = Vec::new();
    let mut strs = Vec::new();
    for (entry, field) in named {
        let is_bytes = map_type_boxed(&entry.value_type) == "byte[]";
        if is_bytes {
            eq.push_str(&format!(
                "            && java.util.Arrays.equals({field}, o.{field})\n"
            ));
            hashes.push(format!("java.util.Arrays.hashCode({field})"));
            strs.push(format!("\"{field}=\" + java.util.Arrays.toString({field})"));
        } else {
            eq.push_str(&format!(
                "            && java.util.Objects.equals({field}, o.{field})\n"
            ));
            hashes.push(field.to_string());
            strs.push(format!("\"{field}=\" + {field}"));
        }
    }
    let mut out = String::new();
    out.push_str("    @Override\n    public boolean equals(Object obj) {\n");
    out.push_str("        if (this == obj) return true;\n");
    out.push_str(&format!(
        "        if (!(obj instanceof {class} o)) return false;\n"
    ));
    out.push_str("        return true\n");
    out.push_str(&eq);
    out.push_str("        ;\n    }\n");
    out.push_str("    @Override\n    public int hashCode() {\n");
    out.push_str(&format!(
        "        return java.util.Objects.hash({});\n    }}\n",
        hashes.join(", ")
    ));
    out.push_str("    @Override\n    public String toString() {\n");
    out.push_str(&format!(
        "        return \"{class}[\" + {} + \"]\";\n    }}\n",
        strs.join(" + \", \" + ")
    ));
    out
}

// ---------------------------------------------------------------------------
// Aliases & choices
// ---------------------------------------------------------------------------

/// A non-group `TypeDef` becomes a single-component "newtype" record, the idiomatic
/// Java stand-in for a named scalar/map/array alias.
fn generate_alias(
    config: &JavaConfig,
    name: &str,
    type_expr: &CsilTypeExpression,
    doc: &[String],
) -> GeneratedFile {
    let class = name.to_case(Case::Pascal);
    let jtype = map_type(type_expr);
    let mut code = config.header();
    code.push_str(&javadoc("", &clean_doc(doc), &[]));
    code.push_str(&format!("public record {class}({jtype} value) {{}}\n"));
    GeneratedFile {
        path: config.path_for(&class),
        content: code,
    }
}

/// A type choice `X = A / B / C` becomes a sealed interface with a record per arm,
/// giving exhaustive `switch` at dispatch sites — the standout Java 17 idiom.
fn generate_type_choice(
    config: &JavaConfig,
    name: &str,
    choices: &[CsilTypeExpression],
    doc: &[String],
) -> GeneratedFile {
    let class = name.to_case(Case::Pascal);
    let arms: Vec<(String, String)> = choices
        .iter()
        .map(|c| (choice_arm_name(c), map_type(c)))
        .collect();
    let permits: Vec<String> = arms.iter().map(|(n, _)| format!("{class}.{n}")).collect();

    let mut code = config.header();
    code.push_str(&javadoc("", &clean_doc(doc), &[]));
    code.push_str(&format!(
        "public sealed interface {class} permits {} {{\n",
        permits.join(", ")
    ));
    for (arm, jtype) in &arms {
        code.push_str(&format!(
            "    record {arm}({jtype} value) implements {class} {{}}\n"
        ));
    }
    code.push_str("}\n");
    GeneratedFile {
        path: config.path_for(&class),
        content: code,
    }
}

/// A group choice becomes a sealed interface with one nested record per alternative
/// group shape.
fn generate_group_choice(
    config: &JavaConfig,
    name: &str,
    choices: &[CsilGroupExpression],
    doc: &[String],
) -> GeneratedFile {
    let class = name.to_case(Case::Pascal);
    let variants: Vec<String> = (0..choices.len()).map(|i| format!("Variant{i}")).collect();
    let permits: Vec<String> = variants.iter().map(|v| format!("{class}.{v}")).collect();

    let mut code = config.header();
    code.push_str(&javadoc("", &clean_doc(doc), &[]));
    code.push_str(&format!(
        "public sealed interface {class} permits {} {{\n",
        permits.join(", ")
    ));
    for (i, group) in choices.iter().enumerate() {
        let comps: Vec<String> = group
            .entries
            .iter()
            .filter_map(|e| {
                entry_field_name(e).map(|field| {
                    let optional = matches!(e.occurrence, Some(CsilOccurrence::Optional));
                    let jtype = if optional {
                        map_type_boxed(&e.value_type)
                    } else {
                        map_type(&e.value_type)
                    };
                    format!("{jtype} {field}")
                })
            })
            .collect();
        code.push_str(&format!(
            "    record Variant{i}({}) implements {class} {{}}\n",
            comps.join(", ")
        ));
    }
    code.push_str("}\n");
    GeneratedFile {
        path: config.path_for(&class),
        content: code,
    }
}

// ---------------------------------------------------------------------------
// Per-type CBOR codec (CsilCbor.java)
//
// CSIL is the CBOR Service Interface Language; the canonical wire is a CBOR map
// keyed by the CSIL field name verbatim. Java has no derive/reflection CBOR codec
// in its stdlib, and the transport lib's value model is package-private, so the
// generator emits a self-contained per-record codec (the same shape the C/Zig/
// OCaml/Dart/Swift/Go targets emit) so the bytes are owned by generated code and
// agree byte-for-byte across every language.
// ---------------------------------------------------------------------------

/// The PascalCase names of every record type in the spec (a `GroupDef`, or a
/// `TypeDef` wrapping a `Group`). Only records get a codec, so a `Reference` to one
/// of these is what a field/operation payload (de)serializes through.
fn record_names(input: &WasmGeneratorInput) -> std::collections::HashSet<String> {
    input
        .csil_spec
        .rules
        .iter()
        .filter_map(|rule| match &rule.rule_type {
            CsilRuleType::GroupDef(_) => Some(rule.name.to_case(Case::Pascal)),
            CsilRuleType::TypeDef(CsilTypeExpression::Group(_)) => {
                Some(rule.name.to_case(Case::Pascal))
            }
            _ => None,
        })
        .collect()
}

/// The transparent type aliases the codec resolves through, keyed by PascalCase name:
/// a `TypeDef` whose target is a map / array / scalar / reference / tuple (NOT a record
/// group or a choice, which generate their own classes and have their own handling).
///
/// Java represents such an alias as a wrapper record (`record StringInt64Map(Map<...>
/// value) {}`), so a field typed as the alias holds the wrapper, not the underlying
/// value. The codec therefore unwraps `.value()` on encode and re-wraps on decode
/// rather than emitting the `CborNull`/`null` stub a bare non-record reference yields.
fn codec_aliases(
    input: &WasmGeneratorInput,
) -> std::collections::HashMap<String, CsilTypeExpression> {
    input
        .csil_spec
        .rules
        .iter()
        .filter_map(|rule| match &rule.rule_type {
            CsilRuleType::TypeDef(t) => match t {
                CsilTypeExpression::Group(_) | CsilTypeExpression::Choice(_) => None,
                other => Some((rule.name.to_case(Case::Pascal), other.clone())),
            },
            _ => None,
        })
        .collect()
}

/// Whether a type is a reference to a record the codec can (de)serialize, so a typed
/// client method can call the generated `encode<T>`/`decode<T>` directly.
fn is_record_ref(ty: &CsilTypeExpression, records: &std::collections::HashSet<String>) -> bool {
    matches!(ty, CsilTypeExpression::Reference(name) if records.contains(&name.to_case(Case::Pascal)))
}

/// The PascalCase class name of a record `Reference`. Only called after
/// `is_record_ref` has confirmed the type is a record reference.
fn record_ref_class(ty: &CsilTypeExpression) -> String {
    match ty {
        CsilTypeExpression::Reference(name) => name.to_case(Case::Pascal),
        _ => String::new(),
    }
}

/// Whether `java_enc_value`/`java_dec_value` model an op-boundary type faithfully (so a
/// per-op codec helper is correct rather than silently lossy). Records, scalars,
/// transparent aliases, named choices (enums/unions/literal-narrowed scalars), arrays,
/// maps, and tuples all resolve to real codec expressions. An inline multi-variant
/// choice has no wire discriminator, and an unmodeled reference has no codec, so those
/// keep the skip-with-note path the client falls back to.
fn java_op_boundary_expressible(
    ty: &CsilTypeExpression,
    records: &std::collections::HashSet<String>,
    aliases: &std::collections::HashMap<String, CsilTypeExpression>,
    choices: &std::collections::HashMap<String, Vec<CsilTypeExpression>>,
) -> bool {
    match codec_unwrap_constrained(ty) {
        CsilTypeExpression::Builtin(_) => true,
        CsilTypeExpression::Reference(name) => {
            let pascal = name.to_case(Case::Pascal);
            records.contains(&pascal)
                || aliases.contains_key(&pascal)
                || choices.contains_key(&pascal)
        }
        CsilTypeExpression::Array { element_type, .. } => {
            java_op_boundary_expressible(element_type, records, aliases, choices)
        }
        CsilTypeExpression::Map { key, value, .. } => {
            java_op_boundary_expressible(key, records, aliases, choices)
                && java_op_boundary_expressible(value, records, aliases, choices)
        }
        CsilTypeExpression::Tuple(_) => true,
        _ => false,
    }
}

/// The `<Base><Method>` stem shared by an op's per-op codec helpers and the client
/// method that calls them, so the two never drift.
fn op_codec_stem(service_name: &str, op: &CsilServiceOperation) -> String {
    format!("{}{}", service_base(service_name), pascal_op_name(&op.name))
}

/// The CBOR encoding of a text key. Comparing these byte slices lexicographically is
/// exactly RFC 8949 §4.2.1 canonical key ordering, computed at generation time so the
/// emitted encoder lays a record's map keys down in canonical order.
fn cbor_text_key_bytes(name: &str) -> Vec<u8> {
    let bytes = name.as_bytes();
    let n = bytes.len() as u64;
    let mt = 3u8 << 5;
    let mut head = Vec::new();
    if n < 24 {
        head.push(mt | n as u8);
    } else if n < 0x100 {
        head.push(mt | 24);
        head.push(n as u8);
    } else {
        head.push(mt | 25);
        head.extend_from_slice(&(n as u16).to_be_bytes());
    }
    head.extend_from_slice(bytes);
    head
}

fn codec_unwrap_constrained(ty: &CsilTypeExpression) -> &CsilTypeExpression {
    match ty {
        CsilTypeExpression::Constrained { base_type, .. } => base_type,
        other => other,
    }
}

// `choice_arm_literal` is shared machinery now (see `csilgen_common::choice`, THE
// normative literal-arm/enum-vs-union contract every generator must agree on) — every
// `choice_arm_literal(...)` call site below keeps working unchanged.
use csilgen_common::choice_arm_literal;

/// Collapse a choice the way `map_type` does: a single non-literal arm whose Java type
/// every literal arm also shares narrows to that arm's type (so the codec agrees with
/// the field's declared Java type); a mixed-Java-type union has no precise single model
/// and is carried through the `Object`-wrapper union path instead.
fn codec_collapse_choice(choices: &[CsilTypeExpression]) -> Option<&CsilTypeExpression> {
    let non_literal: Vec<&CsilTypeExpression> = choices
        .iter()
        .filter(|c| choice_arm_literal(c).is_none())
        .collect();
    match non_literal.as_slice() {
        [only]
            if choices
                .iter()
                .all(|c| map_type_boxed(c) == map_type_boxed(only)) =>
        {
            Some(only)
        }
        _ => None,
    }
}

/// The literals of a `Choice` left un-hoisted at a field/array/map/tuple position — by
/// construction of `HoistOptions::hoist_all_literal_choices: false` (see
/// `generate_java`), the only shape this can be once `codec_collapse_choice` has
/// already declined it is a closed, all-literal enum with no name of its own. `None` for
/// a genuine union (>=1 non-literal arm), which `codec_collapse_choice`'s caller already
/// handles or, failing that, falls back to the unmodeled `CborNull`/`null` placeholder.
fn inline_enum_literals(choices: &[CsilTypeExpression]) -> Option<Vec<&CsilLiteralValue>> {
    match csilgen_common::classify_choice(choices) {
        ChoiceClass::Enum(literals) => Some(literals),
        ChoiceClass::Union(_) => None,
    }
}

/// Named `Name = A / B / ...` choice rules (the alias-style `TypeDef(Choice)`), keyed by
/// PascalCase name. These generate a `record Name(... value)` wrapper, and the codec
/// gives each one an `enc<Name>`/`dec<Name>` helper: a bare literal for an all-literal
/// enum, a `[index, value]` tagged sum for a multi-arm union, and a transparent reach
/// through `.value()` for a literal-narrowed scalar. The sealed-interface `TypeChoice`
/// rules are a distinct shape and are intentionally excluded here.
fn codec_choices(
    input: &WasmGeneratorInput,
) -> std::collections::HashMap<String, Vec<CsilTypeExpression>> {
    input
        .csil_spec
        .rules
        .iter()
        .filter_map(|rule| match &rule.rule_type {
            CsilRuleType::TypeDef(CsilTypeExpression::Choice(choices)) => {
                Some((rule.name.to_case(Case::Pascal), choices.clone()))
            }
            _ => None,
        })
        .collect()
}

/// A Java expression building a `CborValue` from `expr` (a typed value of the field's
/// mapped Java type). `depth` keeps nested-lambda parameter names distinct, since Java
/// forbids a lambda parameter shadowing one already in scope.
fn java_enc_value(
    ty: &CsilTypeExpression,
    expr: &str,
    records: &std::collections::HashSet<String>,
    aliases: &std::collections::HashMap<String, CsilTypeExpression>,
    choices: &std::collections::HashMap<String, Vec<CsilTypeExpression>>,
    depth: usize,
) -> String {
    match codec_unwrap_constrained(ty) {
        CsilTypeExpression::Builtin(name) => match name.as_str() {
            "int" | "nint" => format!("new CborInt({expr})"),
            "uint" => format!("new CborUint({expr})"),
            "float" | "float64" | "double" => format!("new CborFloat({expr})"),
            "text" | "tstr" => format!("new CborText({expr})"),
            "bytes" | "bstr" => format!("new CborBytes({expr})"),
            "bool" => format!("new CborBool({expr})"),
            "timestamp" => format!("encTimestamp({expr})"),
            "decimal" => format!("encDecimal({expr})"),
            // `any` already holds the codec's own value tree; it passes through verbatim.
            "any" => expr.to_string(),
            _ => "new CborNull()".to_string(),
        },
        CsilTypeExpression::Reference(name) if records.contains(&name.to_case(Case::Pascal)) => {
            format!("enc{}({expr})", name.to_case(Case::Pascal))
        }
        // A named choice (enum / union / literal-narrowed scalar) has its own helper.
        CsilTypeExpression::Reference(name)
            if choices.contains_key(&name.to_case(Case::Pascal)) =>
        {
            format!("enc{}({expr})", name.to_case(Case::Pascal))
        }
        // A reference to a transparent alias has no codec of its own; encode its
        // underlying value. The field holds the wrapper record, so reach through
        // `.value()` to the underlying map/array/scalar the real encoder expects.
        CsilTypeExpression::Reference(name)
            if aliases.contains_key(&name.to_case(Case::Pascal)) =>
        {
            let pascal = name.to_case(Case::Pascal);
            java_enc_value(
                &aliases[&pascal],
                &format!("({expr}).value()"),
                records,
                aliases,
                choices,
                depth,
            )
        }
        CsilTypeExpression::Array { element_type, .. } => {
            let p = format!("csilElem{depth}");
            let inner = java_enc_value(element_type, &p, records, aliases, choices, depth + 1);
            format!("encArray({expr}, {p} -> {inner})")
        }
        CsilTypeExpression::Map { key, value, .. } => {
            let kp = format!("csilK{depth}");
            let vp = format!("csilV{depth}");
            let kenc = java_enc_value(key, &kp, records, aliases, choices, depth + 1);
            let venc = java_enc_value(value, &vp, records, aliases, choices, depth + 1);
            format!("encMap({expr}, {kp} -> {kenc}, {vp} -> {venc})")
        }
        // A tuple is a positional CBOR array of fixed length; an absent optional element
        // is held as `null` in place (encoded as CBOR null) so the length is preserved.
        CsilTypeExpression::Tuple(group) => {
            let mut elems: Vec<String> = Vec::new();
            for (i, entry) in group.entries.iter().enumerate() {
                let boxed = map_type_boxed(&entry.value_type);
                let elem_expr = format!("({boxed}) ({expr}).get({i})");
                let enc = java_enc_value(
                    &entry.value_type,
                    &elem_expr,
                    records,
                    aliases,
                    choices,
                    depth + 1,
                );
                if matches!(entry.occurrence, Some(CsilOccurrence::Optional)) {
                    elems.push(format!(
                        "(({expr}).get({i}) == null ? new CborNull() : {enc})"
                    ));
                } else {
                    elems.push(enc);
                }
            }
            format!(
                "new CborArray(java.util.Arrays.asList({}))",
                elems.join(", ")
            )
        }
        // A choice left un-hoisted at this field/element position is, by construction of
        // `HoistOptions::hoist_all_literal_choices: false` (see `generate_java`), always
        // either a narrowed-scalar union (>=1 non-literal arm sharing every literal's
        // Java type — `codec_collapse_choice`) or a closed, all-literal enum with no name
        // of its own to hang a wrapper class/codec on (`inline_enum_literals`). The
        // latter dispatches through the same generic `encEnumScalar` helper
        // `emit_mixed_enum_codec` uses for a NAMED mixed-kind enum — kind-agnostic, since
        // an inline enum's value is always one of the same handful of boxed scalar types
        // regardless of which specific literals are declared.
        CsilTypeExpression::Choice(choices_inline) => match codec_collapse_choice(choices_inline) {
            Some(only) => java_enc_value(only, expr, records, aliases, choices, depth),
            None => match inline_enum_literals(choices_inline) {
                Some(_) => format!("encEnumScalar({expr})"),
                None => "new CborNull()".to_string(),
            },
        },
        CsilTypeExpression::Literal(lit) => java_literal_cbor_expr(lit),
        // A type the codec cannot model precisely is carried as null rather than emitting
        // uncompilable code.
        _ => "new CborNull()".to_string(),
    }
}

/// A Java expression decoding a typed value from `expr` (a `CborValue`). Unmodeled
/// shapes (a non-record reference, a tuple, `any`) map to a reference Java type, so
/// `null` is a type-compatible placeholder there.
fn java_dec_value(
    ty: &CsilTypeExpression,
    expr: &str,
    records: &std::collections::HashSet<String>,
    aliases: &std::collections::HashMap<String, CsilTypeExpression>,
    choices: &std::collections::HashMap<String, Vec<CsilTypeExpression>>,
    depth: usize,
) -> String {
    match codec_unwrap_constrained(ty) {
        CsilTypeExpression::Builtin(name) => match name.as_str() {
            "int" | "nint" => format!("asI64({expr})"),
            "uint" => format!("asU64({expr})"),
            "float" | "float64" | "double" => format!("asF64({expr})"),
            "text" | "tstr" => format!("asText({expr})"),
            "bytes" | "bstr" => format!("asBytes({expr})"),
            "bool" => format!("asBool({expr})"),
            "timestamp" => format!("asTimestamp({expr})"),
            "decimal" => format!("asDecimal({expr})"),
            // `any` is the decoded CBOR value tree passed through verbatim.
            "any" => expr.to_string(),
            _ => "null".to_string(),
        },
        CsilTypeExpression::Reference(name) if records.contains(&name.to_case(Case::Pascal)) => {
            format!("dec{}({expr})", name.to_case(Case::Pascal))
        }
        CsilTypeExpression::Reference(name)
            if choices.contains_key(&name.to_case(Case::Pascal)) =>
        {
            format!("dec{}({expr})", name.to_case(Case::Pascal))
        }
        // The underlying decoder yields the unwrapped map/array/scalar value; rewrap it
        // in the alias's generated wrapper record so the field's declared Java type holds.
        CsilTypeExpression::Reference(name)
            if aliases.contains_key(&name.to_case(Case::Pascal)) =>
        {
            let pascal = name.to_case(Case::Pascal);
            let inner = java_dec_value(&aliases[&pascal], expr, records, aliases, choices, depth);
            format!("new {pascal}({inner})")
        }
        CsilTypeExpression::Array { element_type, .. } => {
            let p = format!("csilE{depth}");
            let inner = java_dec_value(element_type, &p, records, aliases, choices, depth + 1);
            format!("decArray({expr}, {p} -> {inner})")
        }
        CsilTypeExpression::Map { key, value, .. } => {
            let kp = format!("csilK{depth}");
            let vp = format!("csilV{depth}");
            let kdec = java_dec_value(key, &kp, records, aliases, choices, depth + 1);
            let vdec = java_dec_value(value, &vp, records, aliases, choices, depth + 1);
            format!("decMap({expr}, {kp} -> {kdec}, {vp} -> {vdec})")
        }
        // A tuple decodes positionally from a fixed-length CBOR array; a CBOR-null element
        // becomes a `null` in the heterogeneous `List<Object>` it reconstructs. Each
        // position re-reads the array (idempotent) so the whole tuple stays one expression.
        CsilTypeExpression::Tuple(group) => {
            let mut elems: Vec<String> = Vec::new();
            for (i, entry) in group.entries.iter().enumerate() {
                let elem = format!("asArray({expr}).get({i})");
                let dec = java_dec_value(
                    &entry.value_type,
                    &elem,
                    records,
                    aliases,
                    choices,
                    depth + 1,
                );
                if matches!(entry.occurrence, Some(CsilOccurrence::Optional)) {
                    elems.push(format!("({elem} instanceof CborNull ? null : {dec})"));
                } else {
                    elems.push(dec);
                }
            }
            format!("java.util.Arrays.<Object>asList({})", elems.join(", "))
        }
        // See the matching comment in `java_enc_value`: the only un-hoisted `Choice`
        // shapes reaching here are a narrowed-scalar union or a closed all-literal enum.
        // The latter reads generically (`asEnumScalar`) then validates against THIS
        // choice's own declared vocabulary via `requireEnumMember`, closing the same
        // membership gap `emit_mixed_enum_codec`/`emit_uniform_enum_codec` close for a
        // NAMED choice — without this, an inline enum field silently decoded to `null`
        // rather than its actual value.
        CsilTypeExpression::Choice(choices_inline) => match codec_collapse_choice(choices_inline) {
            Some(only) => java_dec_value(only, expr, records, aliases, choices, depth),
            None => match inline_enum_literals(choices_inline) {
                Some(literals) => {
                    let p = format!("csilEnumScalar{depth}");
                    let membership = literals
                        .iter()
                        .enumerate()
                        .map(|(i, lit)| java_literal_equals_object_expr(lit, &p, i))
                        .collect::<Vec<_>>()
                        .join(" || ");
                    format!("requireEnumMember(asEnumScalar({expr}), {p} -> {membership})")
                }
                None => "null".to_string(),
            },
        },
        CsilTypeExpression::Literal(lit) => {
            let expected = java_literal_cbor_expr(lit);
            let value = java_literal_value_expr(lit);
            format!("expectLiteral({expr}, {expected}, {value})")
        }
        _ => "null".to_string(),
    }
}

/// Emit the `enc<T>`/`dec<T>` pair plus the public `encode<T>`/`decode<T>` byte
/// wrappers for one record. The encoder lays keys in canonical order; the decoder
/// reads by key in declaration order (order is irrelevant on decode).
fn emit_record_codec(
    name: &str,
    group: &CsilGroupExpression,
    records: &std::collections::HashSet<String>,
    aliases: &std::collections::HashMap<String, CsilTypeExpression>,
    choices: &std::collections::HashMap<String, Vec<CsilTypeExpression>>,
) -> String {
    let class = name.to_case(Case::Pascal);
    // (member, wire, entry) in declaration order, plus a canonical-key-order copy for
    // the encoder so the emitted map is deterministic across languages.
    let named: Vec<(String, String, &CsilGroupEntry)> = group
        .entries
        .iter()
        .filter_map(|e| {
            let member = entry_field_name(e)?;
            let wire = entry_wire_name(e)?;
            Some((member, wire, e))
        })
        .collect();
    let mut canonical: Vec<&(String, String, &CsilGroupEntry)> = named.iter().collect();
    canonical.sort_by_key(|f| cbor_text_key_bytes(&f.1));

    let mut out = String::new();
    out.push_str(&format!("    static CborValue enc{class}({class} v) {{\n"));
    out.push_str(&format!(
        "        java.util.List<CborEntry> csilEntries = new java.util.ArrayList<>({});\n",
        named.len()
    ));
    for (member, wire, entry) in &canonical {
        let wire_lit = java_string(wire);
        let enc = java_enc_value(
            &entry.value_type,
            &format!("v.{member}()"),
            records,
            aliases,
            choices,
            0,
        );
        if matches!(entry.occurrence, Some(CsilOccurrence::Optional)) {
            // An absent optional is omitted from the map entirely (wire contract).
            out.push_str(&format!("        if (v.{member}() != null) {{\n"));
            out.push_str(&format!(
                "            csilEntries.add(new CborEntry(new CborText({wire_lit}), {enc}));\n"
            ));
            out.push_str("        }\n");
        } else {
            out.push_str(&format!(
                "        csilEntries.add(new CborEntry(new CborText({wire_lit}), {enc}));\n"
            ));
        }
    }
    out.push_str("        return new CborMap(csilEntries);\n    }\n\n");

    out.push_str(&format!(
        "    static {class} dec{class}(CborValue csilRoot) {{\n"
    ));
    for (member, wire, entry) in &named {
        let wire_lit = java_string(wire);
        if matches!(entry.occurrence, Some(CsilOccurrence::Optional)) {
            // A missing optional key leaves the field null; a present one decodes into
            // the boxed Java type so absent and present stay distinguishable.
            let bty = map_type_boxed(&entry.value_type);
            let dec = java_dec_value(&entry.value_type, "csilField", records, aliases, choices, 0);
            out.push_str(&format!("        {bty} {member};\n"));
            out.push_str("        {\n");
            out.push_str(&format!(
                "            CborValue csilField = mapGet(csilRoot, {wire_lit});\n"
            ));
            out.push_str(&format!(
                "            {member} = csilField != null ? {dec} : null;\n"
            ));
            out.push_str("        }\n");
        } else {
            let ty = map_type(&entry.value_type);
            let dec = java_dec_value(
                &entry.value_type,
                &format!("require(csilRoot, {wire_lit})"),
                records,
                aliases,
                choices,
                0,
            );
            out.push_str(&format!("        {ty} {member} = {dec};\n"));
        }
    }
    let args: Vec<&str> = named.iter().map(|(m, _, _)| m.as_str()).collect();
    out.push_str(&format!(
        "        return new {class}({});\n    }}\n\n",
        args.join(", ")
    ));

    out.push_str(&format!(
        "    public static byte[] encode{class}({class} v) {{\n        return encode(enc{class}(v));\n    }}\n\n"
    ));
    out.push_str(&format!(
        "    public static {class} decode{class}(byte[] data) {{\n        return dec{class}(decode(data));\n    }}\n\n"
    ));
    out
}

/// The CBOR scalar kind a literal maps to under the single-scalar-read happy path
/// (`asI64`/`asF64`/`asBool`/`asText` plus one `CborXxx` wrapper). `None` for a kind
/// that path cannot represent at all (`Bytes`/`Null`/`Array`), which — like a genuine
/// kind mix — routes to the generic per-member dispatch below.
fn literal_scalar_kind(lit: &CsilLiteralValue) -> Option<&'static str> {
    match lit {
        CsilLiteralValue::Text(_) => Some("text"),
        CsilLiteralValue::Integer(_) => Some("int"),
        CsilLiteralValue::Float(_) => Some("float"),
        CsilLiteralValue::Bool(_) => Some("bool"),
        _ => None,
    }
}

/// The single CBOR scalar kind every literal in an all-literal choice shares, if any.
/// `None` when the vocabulary mixes kinds (`"a" / 1`) or contains a kind the single-
/// scalar-read path cannot represent (`Bytes`/`Null`/`Array`) — either way the closed
/// enum needs `emit_mixed_enum_codec`'s generic per-member dispatch, not a single
/// `CborXxx`/`asXxx` pair picked from one arm and wrongly applied to every member (the
/// confirmed defect this function exists to detect).
fn uniform_literal_kind(literals: &[&CsilLiteralValue]) -> Option<&'static str> {
    let first = literal_scalar_kind(literals.first()?)?;
    literals
        .iter()
        .all(|lit| literal_scalar_kind(lit) == Some(first))
        .then_some(first)
}

/// Emit the `enc<Name>`/`dec<Name>` helper pair for a named `Name = A / B / ...` choice.
/// Two shapes, mirroring the locked wire contract: an all-literal choice is an **enum**
/// and rides as the bare literal (its own discriminant); a choice with one or more
/// non-literal arms is a **union** and rides as a `[variant_index, value]` tagged sum,
/// the index being the arm's 0-based declaration position — this covers both a
/// single-real-arm literal-narrowed scalar (e.g. `text / "a" / "b"`, OrderStatus in
/// examples/real-world-api/e-commerce-api.csil) and a genuine multi-type union. A
/// literal arm's payload is its own declared value (not a placeholder), matches by
/// value equality ahead of any general arm sharing its Java dispatch type on encode,
/// and is validated by equality against the declared literal on decode — mirroring the
/// Go/Python generators' `emit_union_codec`.
fn emit_choice_codec(
    name: &str,
    choices: &[CsilTypeExpression],
    records: &std::collections::HashSet<String>,
    aliases: &std::collections::HashMap<String, CsilTypeExpression>,
    choices_map: &std::collections::HashMap<String, Vec<CsilTypeExpression>>,
) -> String {
    let class = name.to_case(Case::Pascal);
    // The ENUM/UNION split is delegated to the shared, normative classifier (THE
    // contract: EVERY arm a literal, of any kind — even mixed, `"a" / 1` counts — is an
    // Enum; at least one non-literal arm is a Union), not re-derived locally.
    // `choice_arm_literal` (imported above) already sees through a `.default`-suffixed
    // arm's `Constrained` wrapper, so a closed enum's last arm is still recognized here.
    match csilgen_common::classify_choice(choices) {
        ChoiceClass::Enum(literals) => emit_enum_codec(&class, &literals),
        ChoiceClass::Union(_) => {
            // Union (>=1 non-literal arm): the locked tagged sum. A single non-literal
            // arm whose Java type every literal arm also shares collapses the wrapper's
            // value to that one Java type (`map_type` agrees, via `codec_collapse_choice`),
            // so encode needs no runtime type dispatch — only the literal-vs-general value
            // check. A mixed-Java-type union (a lone non-literal the literals don't
            // type-match, e.g. `"none" / 42 / Rec`, or two or more non-literal arms) keeps
            // the wrapper's value `Object` and needs the instanceof-grouped dispatch to
            // tell its arms apart at runtime.
            let mut out = if codec_collapse_choice(choices).is_some() {
                emit_scalar_union_encode(&class, choices, records, aliases, choices_map)
            } else {
                emit_object_union_encode(&class, choices, records, aliases, choices_map)
            };
            out.push_str(&emit_union_decode(
                &class,
                choices,
                records,
                aliases,
                choices_map,
            ));
            out
        }
    }
}

/// Emit `enc<Class>`/`dec<Class>` for a closed literal-only choice (a
/// `ChoiceClass::Enum`). `Color` is a plain `record Color(Object value) {}` wrapper
/// (`generate_alias`), not a closed Java enum type — nothing at the type level rejects a
/// well-typed value outside the declared literal set, so decode must check membership
/// itself (parity with the python/ocaml/php/ruby/elixir codecs' `_csil_decode_enum`-style
/// check). Without this, `decColor` would accept ANY string on the wire, not just
/// "red"/"green"/"blue". A uniform-kind vocabulary keeps the single `CborXxx`/`asXxx`
/// happy path; a mixed-kind vocabulary (`"a" / 1`) has no single wrapper/reader that
/// fits every member and needs the generic per-member dispatch instead — see
/// `emit_mixed_enum_codec`'s docs for the defect this fixes.
fn emit_enum_codec(class: &str, literals: &[&CsilLiteralValue]) -> String {
    match uniform_literal_kind(literals) {
        Some(kind) => emit_uniform_enum_codec(class, kind, literals),
        None => emit_mixed_enum_codec(class, literals),
    }
}

/// The happy path for a closed enum whose literals all share one CBOR scalar kind: a
/// single `CborXxx` wrapper on encode and a single `asXxx` reader on decode, with
/// membership validated against the (necessarily uniform-kind) full literal list.
fn emit_uniform_enum_codec(class: &str, kind: &str, literals: &[&CsilLiteralValue]) -> String {
    let (enc, read) = match kind {
        "int" => (
            "new CborInt((Long) v.value())".to_string(),
            "asI64(csilRoot)".to_string(),
        ),
        "float" => (
            "new CborFloat((Double) v.value())".to_string(),
            "asF64(csilRoot)".to_string(),
        ),
        "bool" => (
            "new CborBool((Boolean) v.value())".to_string(),
            "asBool(csilRoot)".to_string(),
        ),
        _ => (
            "new CborText((String) v.value())".to_string(),
            "asText(csilRoot)".to_string(),
        ),
    };
    let mut out = String::new();
    out.push_str(&format!(
        "    static CborValue enc{class}({class} v) {{\n        return {enc};\n    }}\n\n"
    ));
    let membership = literals
        .iter()
        .map(|lit| java_literal_equals_expr(lit, "csilVal"))
        .collect::<Vec<_>>()
        .join(" || ");
    out.push_str(&format!(
        "    static {class} dec{class}(CborValue csilRoot) {{\n\
         \x20       var csilVal = {read};\n\
         \x20       if (!({membership})) {{\n\
         \x20           throw new CsilCborException(\"csil cbor: {class} value \" + csilVal + \" is not a member of the declared enum\");\n\
         \x20       }}\n\
         \x20       return new {class}(csilVal);\n    }}\n\n"
    ));
    out
}

/// A mixed-kind literal enum (`"a" / 1`, or any literal-only choice whose members don't
/// share one CBOR scalar kind — including a `Bytes`/`Null`/`Array` literal, which the
/// single-scalar-read happy path above cannot represent at all): no single `CborXxx`
/// wrapper or `asXxx` reader fits every member. Before this function existed,
/// `emit_choice_codec` picked ONE kind from the choice's FIRST literal arm and applied
/// it to every member — encoding a member of a different kind cast `v.value()` to the
/// wrong boxed type (a `ClassCastException` at runtime), and decoding silently dropped
/// every member whose kind wasn't the winning one from the membership check (a
/// legitimately-encoded value was rejected as "not a member" even though it was
/// declared). The fix: encode dispatches on the boxed value's own runtime Java type via
/// the shared `encEnumScalar` helper (the same one an un-hoisted inline all-literal
/// choice field uses — see `java_enc_value`'s `Choice` arm — since a mixed-kind enum's
/// value is boxed the same handful of runtime types regardless of which specific
/// literals are declared), and decode reads the CBOR item generically (`asEnumScalar`,
/// keyed on the item's own CBOR major type) then validates the result against the FULL
/// declared vocabulary using each literal's own kind-appropriate equality check. Mirrors
/// the OCaml generator's `MixedEnum` and the TypeScript generator's `asEnumScalar`
/// handling of this same shape.
fn emit_mixed_enum_codec(class: &str, literals: &[&CsilLiteralValue]) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "    static CborValue enc{class}({class} v) {{\n        return encEnumScalar(v.value());\n    }}\n\n"
    ));

    out.push_str(&format!(
        "    static {class} dec{class}(CborValue csilRoot) {{\n"
    ));
    out.push_str("        Object csilVal = asEnumScalar(csilRoot);\n");
    let membership = literals
        .iter()
        .enumerate()
        .map(|(idx, lit)| java_literal_equals_object_expr(lit, "csilVal", idx))
        .collect::<Vec<_>>()
        .join(" || ");
    out.push_str(&format!(
        "        if (!({membership})) {{\n\
         \x20           throw new CsilCborException(\"csil cbor: {class} value \" + csilVal + \" is not a member of the declared enum\");\n\
         \x20       }}\n\
         \x20       return new {class}(csilVal);\n    }}\n\n"
    ));
    out
}

/// The Java equality expression checking `expr` (the union wrapper's dispatched
/// value, already known compatible with `lit`'s kind by construction — see
/// `map_type`'s literal-narrowed-scalar collapse) against a literal arm's own
/// declared value. `Objects.equals` autoboxes a primitive `expr` for free and is
/// null-safe; a byte string needs `Arrays.equals` instead since array equality is
/// reference identity under `Object.equals`.
fn java_literal_equals_expr(lit: &CsilLiteralValue, expr: &str) -> String {
    if let CsilLiteralValue::Bytes(bytes) = lit {
        let values = bytes
            .iter()
            .map(|b| b.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        return format!("java.util.Arrays.equals({expr}, new byte[] {{ {values} }})");
    }
    format!(
        "java.util.Objects.equals({expr}, {})",
        java_literal_value_expr(lit)
    )
}

/// Like `java_literal_equals_expr`, but for comparing a statically-`Object`-typed
/// decoded scalar (`asEnumScalar`'s result, used by a mixed-kind enum's decode — see
/// `emit_mixed_enum_codec`) against one declared literal. Every kind but `Bytes` already
/// works through `java_literal_equals_expr`'s `Objects.equals` unchanged (it autoboxes a
/// bare `Object` operand for free); only `Bytes` needs its own `instanceof`-guarded cast
/// first, since `Arrays.equals(byte[], byte[])` does not accept an `Object` operand the
/// way `Objects.equals` does. `idx` keeps the `instanceof` pattern variable unique
/// across the membership OR-chain's arms (Java requires distinct pattern-variable names
/// among sibling `||` operands sharing one enclosing statement).
fn java_literal_equals_object_expr(lit: &CsilLiteralValue, expr: &str, idx: usize) -> String {
    if let CsilLiteralValue::Bytes(bytes) = lit {
        let values = bytes
            .iter()
            .map(|b| b.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        return format!(
            "({expr} instanceof byte[] csilMB{idx} && java.util.Arrays.equals(csilMB{idx}, new byte[] {{ {values} }}))"
        );
    }
    java_literal_equals_expr(lit, expr)
}

/// Encode body for a union collapsed to a single concrete Java type (exactly one
/// non-literal arm): each literal arm is checked by value equality in declaration
/// order ahead of the general arm's fallback, matching the Go/Python generators'
/// literal-first precedence for a shared dispatch type.
fn emit_scalar_union_encode(
    class: &str,
    choices: &[CsilTypeExpression],
    records: &std::collections::HashSet<String>,
    aliases: &std::collections::HashMap<String, CsilTypeExpression>,
    choices_map: &std::collections::HashMap<String, Vec<CsilTypeExpression>>,
) -> String {
    let vexpr = "v.value()";
    let mut out = format!("    static CborValue enc{class}({class} v) {{\n");
    let mut general_idx = None;
    for (idx, arm) in choices.iter().enumerate() {
        let Some(lit) = choice_arm_literal(arm) else {
            general_idx = Some(idx);
            continue;
        };
        let cmp = java_literal_equals_expr(lit, vexpr);
        let enc = java_enc_value(arm, vexpr, records, aliases, choices_map, 0);
        out.push_str(&format!(
            "        if ({cmp}) {{\n            return new CborArray(java.util.Arrays.asList(new CborUint({idx}L), {enc}));\n        }}\n"
        ));
    }
    let general_idx = general_idx.expect("a scalar union has exactly one non-literal arm");
    let genc = java_enc_value(
        &choices[general_idx],
        vexpr,
        records,
        aliases,
        choices_map,
        0,
    );
    out.push_str(&format!(
        "        return new CborArray(java.util.Arrays.asList(new CborUint({general_idx}L), {genc}));\n    }}\n\n"
    ));
    out
}

/// Encode body for a union with two or more non-literal arms (the wrapper's value
/// stays `Object`): arms are grouped by their dispatch Java type (`map_type_boxed`,
/// which already maps a literal arm to the boxed form of its own scalar kind), so a
/// literal sharing its general arm's type is checked by value equality first and the
/// general arm is the fallback for every other value of that type — mirroring the
/// Go/Python generators' type-switch/`isinstance` grouping.
fn emit_object_union_encode(
    class: &str,
    choices: &[CsilTypeExpression],
    records: &std::collections::HashSet<String>,
    aliases: &std::collections::HashMap<String, CsilTypeExpression>,
    choices_map: &std::collections::HashMap<String, Vec<CsilTypeExpression>>,
) -> String {
    let mut out = format!("    static CborValue enc{class}({class} v) {{\n");
    out.push_str("        Object csilInner = v.value();\n");

    let mut type_order: Vec<String> = Vec::new();
    let mut groups: std::collections::HashMap<String, Vec<usize>> =
        std::collections::HashMap::new();
    for (idx, arm) in choices.iter().enumerate() {
        let ty = map_type_boxed(arm);
        let entry = groups.entry(ty.clone()).or_default();
        if entry.is_empty() {
            type_order.push(ty.clone());
        }
        entry.push(idx);
    }

    for ty in &type_order {
        let idxs = &groups[ty];
        let cast = format!("csilCast{}", idxs[0]);
        if idxs.len() == 1 {
            let idx = idxs[0];
            let arm = &choices[idx];
            let enc = java_enc_value(arm, &cast, records, aliases, choices_map, 0);
            if let Some(lit) = choice_arm_literal(arm) {
                let cmp = java_literal_equals_expr(lit, &cast);
                out.push_str(&format!(
                    "        if (csilInner instanceof {ty} {cast} && {cmp}) {{\n            return new CborArray(java.util.Arrays.asList(new CborUint({idx}L), {enc}));\n        }}\n"
                ));
            } else {
                out.push_str(&format!(
                    "        if (csilInner instanceof {ty} {cast}) {{\n            return new CborArray(java.util.Arrays.asList(new CborUint({idx}L), {enc}));\n        }}\n"
                ));
            }
            continue;
        }
        // Multiple arms share this Java type: literal arms win by value equality
        // ahead of the general arm, which is the fallback for every other value.
        let mut literal_idxs = Vec::new();
        let mut general_idx = None;
        for &idx in idxs {
            if choice_arm_literal(&choices[idx]).is_some() {
                literal_idxs.push(idx);
            } else if general_idx.is_none() {
                // Declaration order must be preserved: a later general arm in the same
                // dispatch group is unreachable dead code once the first one already
                // matches every value of that runtime type, so silently keeping the LAST
                // one instead of the FIRST (the previous behavior here) would
                // non-deterministically change which arm's payload shape callers observe,
                // and would contradict every other generator's declaration-order dispatch.
                general_idx = Some(idx);
            }
        }
        out.push_str(&format!(
            "        if (csilInner instanceof {ty} {cast}) {{\n"
        ));
        for idx in literal_idxs {
            let Some(lit) = choice_arm_literal(&choices[idx]) else {
                unreachable!("filtered to literal arms above")
            };
            let cmp = java_literal_equals_expr(lit, &cast);
            let enc = java_enc_value(&choices[idx], &cast, records, aliases, choices_map, 0);
            out.push_str(&format!(
                "            if ({cmp}) {{\n                return new CborArray(java.util.Arrays.asList(new CborUint({idx}L), {enc}));\n            }}\n"
            ));
        }
        if let Some(gi) = general_idx {
            let enc = java_enc_value(&choices[gi], &cast, records, aliases, choices_map, 0);
            out.push_str(&format!(
                "            return new CborArray(java.util.Arrays.asList(new CborUint({gi}L), {enc}));\n"
            ));
        }
        out.push_str("        }\n");
    }
    out.push_str(&format!(
        "        throw new CsilCborException(\"csil cbor: {class} value matches no variant\");\n    }}\n\n"
    ));
    out
}

/// Decode body shared by both union shapes: read the tagged sum's index and dispatch
/// to the arm declared at that position. A literal arm's `java_dec_value` already
/// validates the payload equals the declared literal (`expectLiteral`), erroring on a
/// mismatch rather than trusting the index alone.
fn emit_union_decode(
    class: &str,
    choices: &[CsilTypeExpression],
    records: &std::collections::HashSet<String>,
    aliases: &std::collections::HashMap<String, CsilTypeExpression>,
    choices_map: &std::collections::HashMap<String, Vec<CsilTypeExpression>>,
) -> String {
    let mut out = format!("    static {class} dec{class}(CborValue v) {{\n");
    out.push_str("        java.util.List<CborValue> csilArr = asArray(v);\n");
    out.push_str("        long csilIdx = asU64(csilArr.get(0));\n");
    out.push_str("        CborValue csilPayload = csilArr.get(1);\n");
    for (idx, arm) in choices.iter().enumerate() {
        let dec = java_dec_value(arm, "csilPayload", records, aliases, choices_map, 0);
        out.push_str(&format!(
            "        if (csilIdx == {idx}L) {{\n            return new {class}({dec});\n        }}\n"
        ));
    }
    out.push_str(&format!(
        "        throw new CsilCborException(\"csil cbor: {class} variant index \" + csilIdx);\n    }}\n\n"
    ));
    out
}

/// Build `CsilCbor.java`: the self-contained canonical-CBOR runtime plus an
/// `encode`/`decode` pair per record. `None` when the spec declares no records.
/// Public per-op CBOR helpers so a server (or the client) can compose a
/// `decode(request)/encode(response)` pair for every op whose boundary is non-record —
/// scalar-id requests and `[]T`/map/scalar responses included, not just record↔record.
/// Records keep their `encode<T>`/`decode<T>` wrappers; these add the op-keyed names for
/// the shapes that have no standalone class to hang a codec on, so client and a
/// consumer-side server share one wire surface for every op.
fn emit_op_codecs(
    input: &WasmGeneratorInput,
    records: &std::collections::HashSet<String>,
    aliases: &std::collections::HashMap<String, CsilTypeExpression>,
    choices: &std::collections::HashMap<String, Vec<CsilTypeExpression>>,
) -> String {
    let mut out = String::new();
    for rule in &input.csil_spec.rules {
        let CsilRuleType::ServiceDef(service) = &rule.rule_type else {
            continue;
        };
        for op in &service.operations {
            // Only unary `->` ops get a typed client method, so only they need per-op
            // byte helpers; channel ops ride the codec-agnostic router surface.
            if !matches!(op.direction, CsilServiceDirection::Unidirectional) {
                continue;
            }
            let success = success_type(&op.output_type);
            let null_input = is_null_input(&op.input_type);
            let req_ok = null_input
                || java_op_boundary_expressible(&op.input_type, records, aliases, choices);
            if !req_ok || !java_op_boundary_expressible(&success, records, aliases, choices) {
                continue;
            }
            let stem = op_codec_stem(&rule.name, op);
            // A record boundary already has its `encode<T>`/`decode<T>` wrapper; only the
            // non-record shapes need a fresh op-keyed pair.
            if !null_input && !is_record_ref(&op.input_type, records) {
                out.push_str(&emit_op_codec_pair(
                    &format!("{stem}Request"),
                    &op.input_type,
                    records,
                    aliases,
                    choices,
                ));
            }
            if !is_record_ref(&success, records) {
                out.push_str(&emit_op_codec_pair(
                    &format!("{stem}Response"),
                    &success,
                    records,
                    aliases,
                    choices,
                ));
            }
        }
    }
    out
}

/// One `encode<Name>`/`decode<Name>` pair over the same value builders the record codec
/// uses for its fields, so an arbitrary op-boundary shape gets the byte seam a record
/// type has. `decode(data)`/`encode(...)` are the runtime CBOR<->value helpers.
fn emit_op_codec_pair(
    helper: &str,
    ty: &CsilTypeExpression,
    records: &std::collections::HashSet<String>,
    aliases: &std::collections::HashMap<String, CsilTypeExpression>,
    choices: &std::collections::HashMap<String, Vec<CsilTypeExpression>>,
) -> String {
    let java_type = map_type_boxed(ty);
    let enc = java_enc_value(ty, "csilV", records, aliases, choices, 0);
    let dec = java_dec_value(ty, "decode(csilData)", records, aliases, choices, 0);
    format!(
        "    public static byte[] encode{helper}({java_type} csilV) {{\n        return encode({enc});\n    }}\n\n\
         \x20   public static {java_type} decode{helper}(byte[] csilData) {{\n        return {dec};\n    }}\n\n"
    )
}

fn generate_codec(input: &WasmGeneratorInput, config: &JavaConfig) -> Option<GeneratedFile> {
    let records = record_names(input);
    if records.is_empty() {
        return None;
    }
    let aliases = codec_aliases(input);
    let choices = codec_choices(input);
    let mut body = String::new();
    for rule in &input.csil_spec.rules {
        let group = match &rule.rule_type {
            CsilRuleType::GroupDef(g) => Some(g),
            CsilRuleType::TypeDef(CsilTypeExpression::Group(g)) => Some(g),
            _ => None,
        };
        if let Some(group) = group {
            body.push_str(&emit_record_codec(
                &rule.name, group, &records, &aliases, &choices,
            ));
        }
    }
    // A `enc<Name>`/`dec<Name>` helper for each named choice the records reference.
    for rule in &input.csil_spec.rules {
        if let CsilRuleType::TypeDef(CsilTypeExpression::Choice(arms)) = &rule.rule_type {
            body.push_str(&emit_choice_codec(
                &rule.name, arms, &records, &aliases, &choices,
            ));
        }
    }
    // Per-op byte helpers for non-record op boundaries, so the client and a
    // consumer-side server share one codec surface for every op, not just record↔record.
    body.push_str(&emit_op_codecs(input, &records, &aliases, &choices));

    let mut code = config.header();
    code.push_str("/**\n");
    code.push_str(
        " * Self-contained canonical-CBOR codec for the generated record types. The wire\n",
    );
    code.push_str(
        " * is owned here, never by reflection: a record is a CBOR map keyed by the CSIL\n",
    );
    code.push_str(" * field name verbatim, with map keys laid down in RFC 8949 canonical order.\n");
    code.push_str(" */\n");
    code.push_str("public final class CsilCbor {\n");
    code.push_str("    private CsilCbor() {}\n\n");
    code.push_str(CODEC_RUNTIME_JAVA);
    // The tagged-core (de)serializers are only worth their JDK imports when the spec
    // actually uses the type; the body references the helper iff a field needs it.
    if body.contains("Timestamp(") {
        code.push('\n');
        code.push_str(CODEC_TIMESTAMP_JAVA);
    }
    if body.contains("Decimal(") {
        code.push('\n');
        code.push_str(CODEC_DECIMAL_JAVA);
    }
    // `asEnumScalar`/`encEnumScalar`/`requireEnumMember` are only referenced by a
    // mixed-kind named enum's codec (`emit_mixed_enum_codec`) or an un-hoisted inline
    // all-literal choice field (`java_enc_value`/`java_dec_value`'s `Choice` arm); most
    // specs have neither, so they stay out of the JDK import surface entirely, matching
    // the Timestamp/Decimal conditionals above.
    if body.contains("asEnumScalar(")
        || body.contains("encEnumScalar(")
        || body.contains("requireEnumMember(")
    {
        code.push('\n');
        code.push_str(CODEC_ENUM_SCALAR_JAVA);
    }
    code.push('\n');
    code.push_str(&body);
    code.push_str("}\n");
    Some(GeneratedFile {
        path: config.path_for("CsilCbor"),
        content: code,
    })
}

// ---------------------------------------------------------------------------
// Client surface
// ---------------------------------------------------------------------------

fn generate_transport_iface(config: &JavaConfig) -> GeneratedFile {
    let mut code = config.header();
    code.push_str(
        "/**\n\
         \x20* The caller-supplied byte carrier: it performs the call named by ({@code service},\n\
         \x20* {@code op}) with the already-encoded request bytes and returns the response bytes,\n\
         \x20* or throws. The generated client owns (de)serialization via the codec; the carrier\n\
         \x20* only moves bytes, so it can be HTTP, a queue, or an in-process loop. Synchronous\n\
         \x20* and blocking — no CompletableFuture.\n\
         \x20*/\n\
         public interface Transport {\n\
         \x20   byte[] call(String service, String op, byte[] req) throws ClientException;\n\
         }\n",
    );
    GeneratedFile {
        path: config.path_for("Transport"),
        content: code,
    }
}

fn generate_client_error(config: &JavaConfig) -> GeneratedFile {
    let mut code = config.header();
    code.push_str(
        "/**\n\
         \x20* Wraps a transport-level failure surfaced by a generated client call. Unchecked\n\
         \x20* because it signals a protocol/transport fault, not a recoverable application\n\
         \x20* error — application errors ride inside the decoded payload, distinct from this.\n\
         \x20*/\n\
         public class ClientException extends RuntimeException {\n\
         \x20   public ClientException(String message) {\n\
         \x20       super(message);\n\
         \x20   }\n\
         \n\
         \x20   public ClientException(String message, Throwable cause) {\n\
         \x20       super(message, cause);\n\
         \x20   }\n\
         }\n",
    );
    GeneratedFile {
        path: config.path_for("ClientException"),
        content: code,
    }
}

fn generate_client(
    config: &JavaConfig,
    name: &str,
    service: &CsilServiceDefinition,
    doc: &[String],
    records: &std::collections::HashSet<String>,
    aliases: &std::collections::HashMap<String, CsilTypeExpression>,
    choices: &std::collections::HashMap<String, Vec<CsilTypeExpression>>,
) -> GeneratedFile {
    let base = service_base(name);
    let class = format!("{base}Client");

    let mut code = config.header();
    let mut prose = clean_doc(doc);
    prose.push(format!("A typed, blocking client for the {name} service."));
    prose.push(
        "The client owns (de)serialization via the codec; the transport only moves bytes."
            .to_string(),
    );
    code.push_str(&javadoc("", &prose, &[]));
    code.push_str(&format!("public final class {class} {{\n"));
    code.push_str("    private final Transport transport;\n\n");
    code.push_str(&format!(
        "    public {class}(Transport transport) {{\n        this.transport = transport;\n    }}\n"
    ));

    for op in &service.operations {
        // Only unary request/response operations belong on the RPC client; channel
        // ops ride the router/encoder surface emitted by the base `java` target.
        if !matches!(op.direction, CsilServiceDirection::Unidirectional) {
            continue;
        }
        let success = success_type(&op.output_type);
        let null_input = is_null_input(&op.input_type);
        let req_ok =
            null_input || java_op_boundary_expressible(&op.input_type, records, aliases, choices);
        // Only a genuinely inexpressible boundary (an inline multi-variant choice with no
        // wire discriminator, or an unmodeled reference) is skipped now; scalar/array/map
        // shapes ride the per-op codec helpers, so every other op gets a method.
        if !req_ok || !java_op_boundary_expressible(&success, records, aliases, choices) {
            code.push('\n');
            code.push_str(&format!(
                "    // operation '{}' has a payload csilgen can't (de)serialize; handle it manually\n",
                op.name
            ));
            continue;
        }
        let camel = op.name.to_case(Case::Camel);
        let stem = op_codec_stem(name, op);
        let resp_type = map_type_boxed(&success);
        // A record success reuses its `decode<T>` wrapper; any other shape uses the op's
        // per-op response decoder.
        let decode_resp = if is_record_ref(&success, records) {
            format!("CsilCbor.decode{}", record_ref_class(&success))
        } else {
            format!("CsilCbor.decode{stem}Response")
        };
        // A null input carries no request body; a record reuses its `encode<T>` wrapper;
        // any other shape uses the op's per-op request encoder.
        let (params, req_bytes) = if null_input {
            (String::new(), "null".to_string())
        } else {
            let input = map_type(&op.input_type);
            let enc = if is_record_ref(&op.input_type, records) {
                format!("CsilCbor.encode{}(req)", record_ref_class(&op.input_type))
            } else {
                format!("CsilCbor.encode{stem}Request(req)")
            };
            (format!("{input} req"), enc)
        };
        code.push('\n');
        code.push_str(&javadoc("    ", &clean_doc(&op.doc_comments), &[]));
        code.push_str(&format!(
            "    public {resp_type} {camel}({params}) throws ClientException {{\n"
        ));
        code.push_str(&format!(
            "        return {decode_resp}(transport.call(\"{name}\", \"{op_name}\", {req_bytes}));\n",
            op_name = op.name
        ));
        code.push_str("    }\n");
    }
    code.push_str("}\n");
    GeneratedFile {
        path: config.path_for(&class),
        content: code,
    }
}

// ---------------------------------------------------------------------------
// Server surface
// ---------------------------------------------------------------------------

fn generate_codec_iface(config: &JavaConfig) -> GeneratedFile {
    let mut code = config.header();
    code.push_str(
        "/**\n\
         \x20* The consumer-supplied (de)serialization layer for channel messages. The generator\n\
         \x20* is codec-agnostic; the host wires this to CBOR, JSON, or whatever its protocol\n\
         \x20* expects.\n\
         \x20*/\n\
         public interface Codec {\n\
         \x20   byte[] encode(Object value);\n\
         \n\
         \x20   <T> T decode(byte[] data, Class<T> type);\n\
         }\n",
    );
    GeneratedFile {
        path: config.path_for("Codec"),
        content: code,
    }
}

fn generate_encoded_message(config: &JavaConfig) -> GeneratedFile {
    let mut code = config.header();
    code.push_str(
        "/**\n\
         \x20* A server-pushed channel message: the wire operation name and the encoded body the\n\
         \x20* host frames onto its connection.\n\
         \x20*\n\
         \x20* @param method the wire operation name\n\
         \x20* @param data the encoded message body\n\
         \x20*/\n\
         public record EncodedMessage(String method, byte[] data) {\n\
         \x20   // A record's generated equals/hashCode compare the byte[] by reference; override\n\
         \x20   // them so two messages with equal bytes compare equal.\n\
         \x20   @Override\n\
         \x20   public boolean equals(Object obj) {\n\
         \x20       if (this == obj) {\n\
         \x20           return true;\n\
         \x20       }\n\
         \x20       return obj instanceof EncodedMessage o\n\
         \x20           && java.util.Objects.equals(method, o.method)\n\
         \x20           && java.util.Arrays.equals(data, o.data);\n\
         \x20   }\n\
         \n\
         \x20   @Override\n\
         \x20   public int hashCode() {\n\
         \x20       return java.util.Objects.hash(method, java.util.Arrays.hashCode(data));\n\
         \x20   }\n\
         }\n",
    );
    GeneratedFile {
        path: config.path_for("EncodedMessage"),
        content: code,
    }
}

fn generate_server_interface(
    config: &JavaConfig,
    name: &str,
    service: &CsilServiceDefinition,
    doc: &[String],
) -> GeneratedFile {
    let iface = name.to_case(Case::Pascal);
    let mut code = config.header();
    let mut prose = clean_doc(doc);
    prose.push(format!(
        "The {name} server handler interface; the host implements it."
    ));
    code.push_str(&javadoc("", &prose, &[]));
    code.push_str(&format!("public interface {iface} {{\n"));
    let mut first = true;
    for op in &service.operations {
        let camel = op.name.to_case(Case::Camel);
        let method = match op.direction {
            CsilServiceDirection::Unidirectional => {
                let output = map_type_boxed(&success_type(&op.output_type));
                let params = if is_null_input(&op.input_type) {
                    String::new()
                } else {
                    format!("{} req", map_type(&op.input_type))
                };
                format!("    {output} {camel}({params});\n")
            }
            CsilServiceDirection::Bidirectional => {
                // Fire-and-forget inbound: the host's plumbing pulls a frame and hands
                // it to the router, which decodes and dispatches here.
                let input = map_type(&op.input_type);
                format!("    void {camel}({input} msg);\n")
            }
            // Server pushes only; no inbound method on the server side.
            CsilServiceDirection::Reverse => continue,
        };
        let jdoc = javadoc("    ", &clean_doc(&op.doc_comments), &[]);
        if !first && !jdoc.is_empty() {
            code.push('\n');
        }
        first = false;
        code.push_str(&jdoc);
        code.push_str(&method);
    }
    code.push_str("}\n");
    GeneratedFile {
        path: config.path_for(&iface),
        content: code,
    }
}

fn generate_router(
    config: &JavaConfig,
    name: &str,
    service: &CsilServiceDefinition,
) -> GeneratedFile {
    let iface = name.to_case(Case::Pascal);
    let class = format!("{iface}Router");
    let mut code = config.header();
    code.push_str(&format!(
        "/**\n\
         \x20* Decodes inbound channel frames and dispatches them to a {iface} handler, and\n\
         \x20* encodes server-pushed messages. The host owns the wire; this owns dispatch.\n\
         \x20*/\n"
    ));
    code.push_str(&format!("public final class {class} {{\n"));
    code.push_str(&format!("    private {class}() {{}}\n\n"));

    // Wire-id ordinals, emitted only for a service that carries @wire-id, so a
    // wire-id-free service stays byte-identical.
    if let Some(service_id) = service.wire_id {
        code.push_str(&format!(
            "    public static final long {iface}ServiceWireId = {service_id}L;\n"
        ));
        for op in &service.operations {
            if let Some(op_id) = op.wire_id {
                let m = pascal_op_name(&op.name);
                code.push_str(&format!(
                    "    public static final long {iface}Op{m}WireId = {op_id}L;\n"
                ));
            }
        }
        code.push('\n');
    }

    let has_channel = service_has_channel_ops(service);

    if has_channel {
        // Verbose router: dispatch on the wire method name.
        code.push_str(&format!(
            "    public static void route{iface}Channel({iface} handlers, Codec codec, String method, byte[] data) {{\n"
        ));
        code.push_str("        switch (method) {\n");
        for op in &service.operations {
            if !matches!(op.direction, CsilServiceDirection::Bidirectional) {
                continue;
            }
            let m = &op.name;
            let camel = op.name.to_case(Case::Camel);
            let input = map_type(&op.input_type);
            code.push_str(&format!("            case \"{m}\" -> {{\n"));
            code.push_str(&format!(
                "                {input} msg = codec.decode(data, {input}.class);\n"
            ));
            code.push_str(&format!("                handlers.{camel}(msg);\n"));
            code.push_str("            }\n");
        }
        code.push_str(
            "            default -> throw new IllegalArgumentException(\"unknown channel method \" + method);\n",
        );
        code.push_str("        }\n    }\n\n");

        // Compact router twin: dispatch on the @wire-id ordinal. The profile is
        // negotiated on the wire (never declared in CSIL), so a host keeps both
        // routers and calls whichever the peer selected. Java `switch` rejects a
        // `long` selector, so the ordinals are matched with an if/else chain.
        if service.wire_id.is_some() {
            code.push_str(&format!(
                "    public static void route{iface}ChannelCompact({iface} handlers, Codec codec, long op, byte[] data) {{\n"
            ));
            let mut first = true;
            for op in &service.operations {
                if !matches!(op.direction, CsilServiceDirection::Bidirectional) {
                    continue;
                }
                let Some(op_id) = op.wire_id else { continue };
                let camel = op.name.to_case(Case::Camel);
                let input = map_type(&op.input_type);
                let kw = if first { "if" } else { "} else if" };
                first = false;
                code.push_str(&format!("        {kw} (op == {op_id}L) {{\n"));
                code.push_str(&format!(
                    "            {input} msg = codec.decode(data, {input}.class);\n"
                ));
                code.push_str(&format!("            handlers.{camel}(msg);\n"));
            }
            if first {
                code.push_str(
                    "        throw new IllegalArgumentException(\"unknown channel ordinal \" + op);\n",
                );
            } else {
                code.push_str("        } else {\n");
                code.push_str(
                    "            throw new IllegalArgumentException(\"unknown channel ordinal \" + op);\n",
                );
                code.push_str("        }\n");
            }
            code.push_str("    }\n\n");
        }
    }

    // Outbound encoders for server-pushed (bidirectional + reverse) operations.
    for op in &service.operations {
        if !matches!(
            op.direction,
            CsilServiceDirection::Bidirectional | CsilServiceDirection::Reverse
        ) {
            continue;
        }
        let m = pascal_op_name(&op.name);
        let output = map_type(&op.output_type);
        code.push_str(&format!(
            "    public static EncodedMessage encode{iface}{m}(Codec codec, {output} msg) {{\n"
        ));
        code.push_str(&format!(
            "        return new EncodedMessage(\"{event}\", codec.encode(msg));\n",
            event = op.name
        ));
        code.push_str("    }\n\n");
    }

    code.push_str("}\n");
    GeneratedFile {
        path: config.path_for(&class),
        content: code,
    }
}

// ---------------------------------------------------------------------------
// Type mapping & helpers
// ---------------------------------------------------------------------------

/// Map a CSIL type to its Java form, using primitive scalars where possible.
fn map_type(type_expr: &CsilTypeExpression) -> String {
    match type_expr {
        CsilTypeExpression::Builtin(name) => match name.as_str() {
            // `nint` is a CBOR negative integer; it still lives in a signed 64-bit slot.
            "int" | "uint" | "nint" => "long".to_string(),
            // `float64` is the explicit-width spelling of `float`; both are IEEE-754 doubles.
            "float" | "float64" | "double" => "double".to_string(),
            "text" | "tstr" => "String".to_string(),
            "bytes" | "bstr" => "byte[]".to_string(),
            "bool" => "boolean".to_string(),
            // CBOR tag 0, RFC3339, always UTC — Instant is the UTC instant type.
            "timestamp" => "java.time.Instant".to_string(),
            // CBOR tag 4 exact decimal fraction — BigDecimal is Java's exact decimal.
            "decimal" => "java.math.BigDecimal".to_string(),
            // `any` is an arbitrary CBOR value passed through verbatim; the codec's own
            // value tree is exactly that, so the field holds a CborValue and the codec is
            // an identity at the seam.
            "any" => "CsilCbor.CborValue".to_string(),
            "nil" | "null" => "Object".to_string(),
            other => other.to_case(Case::Pascal),
        },
        CsilTypeExpression::Reference(name) => name.to_case(Case::Pascal),
        CsilTypeExpression::Array { element_type, .. } => {
            format!("java.util.List<{}>", map_type_boxed(element_type))
        }
        CsilTypeExpression::Map { key, value, .. } => {
            format!(
                "java.util.Map<{}, {}>",
                map_type_boxed(key),
                map_type_boxed(value)
            )
        }
        // Java has no tuple type; a fixed-shape array becomes a List<Object>.
        CsilTypeExpression::Tuple(_) => "java.util.List<Object>".to_string(),
        CsilTypeExpression::Literal(lit) => match lit {
            CsilLiteralValue::Integer(_) => "long".to_string(),
            CsilLiteralValue::Float(_) => "double".to_string(),
            CsilLiteralValue::Text(_) => "String".to_string(),
            CsilLiteralValue::Bool(_) => "boolean".to_string(),
            CsilLiteralValue::Bytes(_) => "byte[]".to_string(),
            CsilLiteralValue::Null => "Object".to_string(),
            CsilLiteralValue::Array(_) => "Object".to_string(),
        },
        CsilTypeExpression::Constrained { base_type, .. } => map_type(base_type),
        // A `text / "a" / "b"` style choice (a base scalar narrowed by string literals)
        // collapses to that one underlying scalar — the literals constrain values, not the
        // Java type. A genuine multi-type union (a lone non-literal arm whose Java type the
        // literal arms do NOT share, or more than one non-literal arm) has no single Java
        // type, so it stays Object. A `.default`-suffixed literal arm parses as
        // `Constrained { Literal, .. }`, so membership is tested through `choice_arm_literal`
        // rather than a bare `Literal` match.
        CsilTypeExpression::Choice(choices) => match codec_collapse_choice(choices) {
            Some(only) => map_type(only),
            None => "Object".to_string(),
        },
        _ => "Object".to_string(),
    }
}

/// Map a CSIL type to its Java form with primitive scalars boxed, for use as a
/// generic argument or a nullable (optional) component.
fn map_type_boxed(type_expr: &CsilTypeExpression) -> String {
    let mapped = map_type(type_expr);
    match mapped.as_str() {
        "long" => "Long".to_string(),
        "double" => "Double".to_string(),
        "boolean" => "Boolean".to_string(),
        other => other.to_string(),
    }
}

/// Reduce an operation output to its success type by dropping a top-level
/// `ServiceError` arm of a `Res / ServiceError` union — the error half is surfaced
/// by the transport, not part of the returned value.
fn success_type(type_expr: &CsilTypeExpression) -> CsilTypeExpression {
    if let CsilTypeExpression::Choice(choices) = type_expr {
        let kept: Vec<CsilTypeExpression> = choices
            .iter()
            .filter(|c| !matches!(c, CsilTypeExpression::Reference(name) if name == "ServiceError"))
            .cloned()
            .collect();
        match kept.len() {
            1 => kept.into_iter().next().unwrap(),
            0 => type_expr.clone(),
            _ => CsilTypeExpression::Choice(kept),
        }
    } else {
        type_expr.clone()
    }
}

/// A push op (`-> Event`) carries a `null` input type: no request to send.
fn is_null_input(type_expr: &CsilTypeExpression) -> bool {
    matches!(type_expr, CsilTypeExpression::Builtin(name) if name == "null" || name == "nil")
}

fn service_has_channel_ops(def: &CsilServiceDefinition) -> bool {
    def.operations
        .iter()
        .any(|op| matches!(op.direction, CsilServiceDirection::Bidirectional))
}

fn service_has_pushable_ops(def: &CsilServiceDefinition) -> bool {
    def.operations.iter().any(|op| {
        matches!(
            op.direction,
            CsilServiceDirection::Bidirectional | CsilServiceDirection::Reverse
        )
    })
}

/// Strip a trailing `Service` suffix and PascalCase the remainder, used only for
/// Java identifiers (the client class name and per-op codec stems). Wire strings
/// carry the verbatim CSIL service name instead (csil-rpc-transport.md §1.1).
fn service_base(name: &str) -> String {
    let pascal = name.to_case(Case::Pascal);
    pascal
        .strip_suffix("Service")
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or(pascal)
}

/// PascalCase an operation name for Java identifiers (codec stems, wire-id constant
/// names). Wire strings carry the verbatim kebab-case CSIL operation name instead.
fn pascal_op_name(name: &str) -> String {
    name.to_case(Case::Pascal)
}

/// The camelCase Java component name for a group entry, or `None` when no stable
/// name can be derived (e.g. a typed key).
fn entry_field_name(entry: &CsilGroupEntry) -> Option<String> {
    match &entry.key {
        Some(CsilGroupKey::Bare(name)) => Some(name.to_case(Case::Camel)),
        Some(CsilGroupKey::Literal(CsilLiteralValue::Text(name))) => {
            Some(name.to_case(Case::Camel))
        }
        Some(_) => None,
        None => match &entry.value_type {
            CsilTypeExpression::Reference(name) | CsilTypeExpression::Builtin(name) => {
                Some(name.to_case(Case::Camel))
            }
            _ => None,
        },
    }
}

/// The verbatim CSIL field name used as the CBOR map key on the wire.
fn entry_wire_name(entry: &CsilGroupEntry) -> Option<String> {
    match &entry.key {
        Some(CsilGroupKey::Bare(name)) => Some(name.clone()),
        Some(CsilGroupKey::Literal(CsilLiteralValue::Text(name))) => Some(name.clone()),
        Some(_) => None,
        None => match &entry.value_type {
            CsilTypeExpression::Reference(name) | CsilTypeExpression::Builtin(name) => {
                Some(name.clone())
            }
            _ => None,
        },
    }
}

/// The nested-record arm name for a type-choice alternative. The `Case` suffix
/// keeps the arm name distinct from the referenced type so the arm's component
/// type still resolves to the external type, not the arm record itself.
fn choice_arm_name(type_expr: &CsilTypeExpression) -> String {
    let base = match type_expr {
        CsilTypeExpression::Reference(name) => name.to_case(Case::Pascal),
        CsilTypeExpression::Builtin(name) => name.to_case(Case::Pascal),
        CsilTypeExpression::Array { .. } => "List".to_string(),
        CsilTypeExpression::Map { .. } => "Map".to_string(),
        _ => "Value".to_string(),
    };
    format!("{base}Case")
}

fn literal_as_text(value: &CsilLiteralValue) -> Option<String> {
    match value {
        CsilLiteralValue::Text(s) => Some(s.clone()),
        CsilLiteralValue::Integer(i) => Some(i.to_string()),
        CsilLiteralValue::Float(f) => Some(f.to_string()),
        _ => None,
    }
}

fn literal_as_number(value: &CsilLiteralValue) -> String {
    match value {
        CsilLiteralValue::Integer(i) => i.to_string(),
        CsilLiteralValue::Float(f) => f.to_string(),
        _ => "0".to_string(),
    }
}

/// Normalize CSIL doc-comment lines into plain prose lines: trim surrounding space and
/// any leading `;`/`/` comment punctuation the source used, dropping blanks.
fn clean_doc(lines: &[String]) -> Vec<String> {
    lines
        .iter()
        .map(|l| {
            l.trim()
                .trim_start_matches([';', '/', ' '])
                .trim()
                .to_string()
        })
        .filter(|l| !l.is_empty())
        .collect()
}

/// The human description for a field, drawn from its doc comments and any
/// `@description(...)` metadata, for use as a Javadoc `@param`.
fn entry_description(entry: &CsilGroupEntry) -> Option<String> {
    let mut parts = clean_doc(&entry.doc_comments);
    for m in &entry.metadata {
        if let csilgen_common::CsilFieldMetadata::Description(d) = m {
            let d = d.trim();
            if !d.is_empty() {
                parts.push(d.to_string());
            }
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" "))
    }
}

/// A Javadoc block (no trailing newline beyond the close) from prose lines and optional
/// `@param` entries, indented by `indent`. Empty when there is nothing to say.
fn javadoc(indent: &str, prose: &[String], params: &[(String, String)]) -> String {
    if prose.is_empty() && params.is_empty() {
        return String::new();
    }
    let mut out = format!("{indent}/**\n");
    for line in prose {
        out.push_str(&format!("{indent} * {line}\n"));
    }
    if !prose.is_empty() && !params.is_empty() {
        out.push_str(&format!("{indent} *\n"));
    }
    for (name, desc) in params {
        out.push_str(&format!("{indent} * @param {name} {desc}\n"));
    }
    out.push_str(&format!("{indent} */\n"));
    out
}

/// A type-level Javadoc from a rule's doc comments plus a `@param` per documented field.
fn type_javadoc(doc: &[String], named: &[(&CsilGroupEntry, String)]) -> String {
    let prose = clean_doc(doc);
    let params: Vec<(String, String)> = named
        .iter()
        .filter_map(|(e, f)| entry_description(e).map(|d| (f.clone(), d)))
        .collect();
    javadoc("", &prose, &params)
}

/// A safely-escaped Java double-quoted string literal for arbitrary text.
fn java_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn java_literal_cbor_expr(lit: &CsilLiteralValue) -> String {
    match lit {
        CsilLiteralValue::Integer(i) if *i >= 0 => format!("new CborUint({i}L)"),
        CsilLiteralValue::Integer(i) => format!("new CborInt({i}L)"),
        CsilLiteralValue::Float(f) => format!("new CborFloat({f})"),
        CsilLiteralValue::Text(s) => format!("new CborText({})", java_string(s)),
        CsilLiteralValue::Bool(b) => format!("new CborBool({b})"),
        CsilLiteralValue::Null => "new CborNull()".to_string(),
        CsilLiteralValue::Bytes(bytes) => {
            let values = bytes
                .iter()
                .map(|b| b.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            format!("new CborBytes(new byte[] {{ {values} }})")
        }
        CsilLiteralValue::Array(items) => {
            let values = items
                .iter()
                .map(java_literal_cbor_expr)
                .collect::<Vec<_>>()
                .join(", ");
            format!("new CborArray(java.util.List.of({values}))")
        }
    }
}

fn java_literal_value_expr(lit: &CsilLiteralValue) -> String {
    match lit {
        CsilLiteralValue::Integer(i) => format!("{i}L"),
        CsilLiteralValue::Float(f) => f.to_string(),
        CsilLiteralValue::Text(s) => java_string(s),
        CsilLiteralValue::Bool(b) => b.to_string(),
        CsilLiteralValue::Null => "null".to_string(),
        CsilLiteralValue::Bytes(bytes) => {
            let values = bytes
                .iter()
                .map(|b| b.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            format!("new byte[] {{ {values} }}")
        }
        CsilLiteralValue::Array(items) => {
            let values = items
                .iter()
                .map(java_literal_value_expr)
                .collect::<Vec<_>>()
                .join(", ");
            format!("java.util.Arrays.<Object>asList({values})")
        }
    }
}

/// The self-contained canonical-CBOR (RFC 8949 subset) value model, encoder, decoder,
/// generic composite helpers, and accessors every generated codec builds on. `bytes`
/// is a Java `byte[]` carried as a CBOR byte string (major type 2) by construction,
/// never an array of integers. Emitted as the body of `final class CsilCbor`, so JDK
/// types are written by FQN and hoisted to imports like the rest of this generator.
const CODEC_RUNTIME_JAVA: &str = r#"    /** A minimal canonical-CBOR value tree: a closed set of variants the codec builds and walks. */
    public sealed interface CborValue
        permits CborUint, CborInt, CborBool, CborFloat, CborNull,
                CborText, CborBytes, CborArray, CborMap, CborTag {}

    public record CborUint(long value) implements CborValue {}
    public record CborInt(long value) implements CborValue {}
    public record CborBool(boolean value) implements CborValue {}
    public record CborFloat(double value) implements CborValue {}
    public record CborNull() implements CborValue {}
    public record CborText(String value) implements CborValue {}
    public record CborBytes(byte[] value) implements CborValue {}
    public record CborArray(java.util.List<CborValue> items) implements CborValue {}
    public record CborEntry(CborValue key, CborValue val) {}
    public record CborMap(java.util.List<CborEntry> entries) implements CborValue {}
    public record CborTag(long num, CborValue inner) implements CborValue {}

    /**
     * Thrown when a CBOR payload is malformed or a required field is missing. Unchecked
     * so the generated codec methods read cleanly; a decoding fault is a protocol error,
     * not a recoverable application error (those ride inside the decoded payload).
     */
    public static final class CsilCborException extends RuntimeException {
        public CsilCborException(String message) {
            super(message);
        }
    }

    public static byte[] encode(CborValue v) {
        java.io.ByteArrayOutputStream out = new java.io.ByteArrayOutputStream();
        enc(v, out);
        return out.toByteArray();
    }

    private static void head(int major, long n, java.io.ByteArrayOutputStream out) {
        int mt = major << 5;
        if (Long.compareUnsigned(n, 24L) < 0) {
            out.write(mt | (int) n);
        } else if (Long.compareUnsigned(n, 0x100L) < 0) {
            out.write(mt | 24);
            out.write((int) (n & 0xff));
        } else if (Long.compareUnsigned(n, 0x10000L) < 0) {
            out.write(mt | 25);
            out.write((int) ((n >> 8) & 0xff));
            out.write((int) (n & 0xff));
        } else if (Long.compareUnsigned(n, 0x100000000L) < 0) {
            out.write(mt | 26);
            for (int i = 24; i >= 0; i -= 8) {
                out.write((int) ((n >> i) & 0xff));
            }
        } else {
            out.write(mt | 27);
            for (int i = 56; i >= 0; i -= 8) {
                out.write((int) ((n >> i) & 0xff));
            }
        }
    }

    private static void enc(CborValue v, java.io.ByteArrayOutputStream out) {
        if (v instanceof CborUint x) {
            head(0, x.value(), out);
        } else if (v instanceof CborInt x) {
            if (x.value() >= 0) {
                head(0, x.value(), out);
            } else {
                head(1, -1 - x.value(), out);
            }
        } else if (v instanceof CborBool x) {
            out.write(x.value() ? 0xf5 : 0xf4);
        } else if (v instanceof CborNull) {
            out.write(0xf6);
        } else if (v instanceof CborFloat x) {
            long bits = Double.doubleToRawLongBits(x.value());
            out.write(0xfb);
            for (int i = 56; i >= 0; i -= 8) {
                out.write((int) ((bits >> i) & 0xff));
            }
        } else if (v instanceof CborText x) {
            byte[] u = x.value().getBytes(java.nio.charset.StandardCharsets.UTF_8);
            head(3, u.length, out);
            out.write(u, 0, u.length);
        } else if (v instanceof CborBytes x) {
            head(2, x.value().length, out);
            out.write(x.value(), 0, x.value().length);
        } else if (v instanceof CborArray x) {
            head(4, x.items().size(), out);
            for (CborValue e : x.items()) {
                enc(e, out);
            }
        } else if (v instanceof CborMap x) {
            head(5, x.entries().size(), out);
            for (CborEntry e : x.entries()) {
                enc(e.key(), out);
                enc(e.val(), out);
            }
        } else if (v instanceof CborTag x) {
            head(6, x.num(), out);
            enc(x.inner(), out);
        }
    }

    public static CborValue decode(byte[] b) {
        int[] pos = {0};
        CborValue v = dec(b, pos, 0);
        if (pos[0] != b.length) {
            throw new CsilCborException("csil cbor: trailing bytes");
        }
        return v;
    }

    private static void requireLen(byte[] b, int need) {
        if (need > b.length) {
            throw new CsilCborException("csil cbor: truncated input");
        }
    }

    private static void requireRemaining(byte[] b, int pos, int need) {
        if (pos < 0 || pos > b.length || need > b.length - pos) {
            throw new CsilCborException("csil cbor: truncated input");
        }
    }

    private static long readArg(byte[] b, int[] pos, int low) {
        if (low < 24) {
            pos[0] += 1;
            return low;
        }
        switch (low) {
            case 24:
                requireRemaining(b, pos[0], 2);
                long v24 = b[pos[0] + 1] & 0xffL;
                pos[0] += 2;
                return v24;
            case 25:
                requireRemaining(b, pos[0], 3);
                long v25 = ((b[pos[0] + 1] & 0xffL) << 8) | (b[pos[0] + 2] & 0xffL);
                pos[0] += 3;
                return v25;
            case 26: {
                requireRemaining(b, pos[0], 5);
                long v = 0;
                for (int i = 1; i <= 4; i++) {
                    v = (v << 8) | (b[pos[0] + i] & 0xffL);
                }
                pos[0] += 5;
                return v;
            }
            case 27: {
                requireRemaining(b, pos[0], 9);
                long v = 0;
                for (int i = 1; i <= 8; i++) {
                    v = (v << 8) | (b[pos[0] + i] & 0xffL);
                }
                pos[0] += 9;
                return v;
            }
            default:
                throw new CsilCborException("csil cbor: reserved additional info");
        }
    }

    private static String decodeUtf8(byte[] b, int off, int len) {
        try {
            return java.nio.charset.StandardCharsets.UTF_8.newDecoder()
                    .onMalformedInput(java.nio.charset.CodingErrorAction.REPORT)
                    .onUnmappableCharacter(java.nio.charset.CodingErrorAction.REPORT)
                    .decode(java.nio.ByteBuffer.wrap(b, off, len)).toString();
        } catch (java.nio.charset.CharacterCodingException e) {
            throw new CsilCborException("csil cbor: invalid utf-8");
        }
    }

    private static CborValue dec(byte[] b, int[] pos, int depth) {
        if (depth > 64) {
            throw new CsilCborException("csil cbor: nesting limit exceeded");
        }
        if (pos[0] >= b.length) {
            throw new CsilCborException("csil cbor: unexpected end of input");
        }
        int ib = b[pos[0]] & 0xff;
        int major = ib >> 5;
        int low = ib & 0x1f;
        if (major == 7) {
            switch (low) {
                case 20:
                    pos[0] += 1;
                    return new CborBool(false);
                case 21:
                    pos[0] += 1;
                    return new CborBool(true);
                case 22:
                case 23:
                    pos[0] += 1;
                    return new CborNull();
                case 26: {
                    long bits = readArg(b, pos, low);
                    return new CborFloat(Float.intBitsToFloat((int) bits));
                }
                case 27: {
                    long bits = readArg(b, pos, low);
                    return new CborFloat(Double.longBitsToDouble(bits));
                }
                default:
                    throw new CsilCborException("csil cbor: unsupported simple value");
            }
        }
        long arg = readArg(b, pos, low);
        switch (major) {
            case 0:
                return new CborUint(arg);
            case 1:
                if (arg < 0) {
                    throw new CsilCborException("csil cbor: negative integer out of range");
                }
                return new CborInt(-1 - arg);
            case 2: {
                if (Long.compareUnsigned(arg, b.length - pos[0]) > 0) {
                    throw new CsilCborException("csil cbor: truncated byte string");
                }
                int n = (int) arg;
                requireLen(b, pos[0] + n);
                byte[] slice = java.util.Arrays.copyOfRange(b, pos[0], pos[0] + n);
                pos[0] += n;
                return new CborBytes(slice);
            }
            case 3: {
                if (Long.compareUnsigned(arg, b.length - pos[0]) > 0) {
                    throw new CsilCborException("csil cbor: truncated text string");
                }
                int n = (int) arg;
                requireLen(b, pos[0] + n);
                String s = decodeUtf8(b, pos[0], n);
                pos[0] += n;
                return new CborText(s);
            }
            case 4: {
                if (Long.compareUnsigned(arg, b.length - pos[0]) > 0) {
                    throw new CsilCborException("csil cbor: array length exceeds remaining input");
                }
                int n = (int) arg;
                java.util.List<CborValue> items = new java.util.ArrayList<>(n);
                for (int i = 0; i < n; i++) {
                    items.add(dec(b, pos, depth + 1));
                }
                return new CborArray(items);
            }
            case 5: {
                if (Long.compareUnsigned(arg, b.length - pos[0]) > 0) {
                    throw new CsilCborException("csil cbor: map length exceeds remaining input");
                }
                int n = (int) arg;
                java.util.List<CborEntry> entries = new java.util.ArrayList<>(n);
                for (int i = 0; i < n; i++) {
                    CborValue k = dec(b, pos, depth + 1);
                    CborValue val = dec(b, pos, depth + 1);
                    entries.add(new CborEntry(k, val));
                }
                return new CborMap(entries);
            }
            case 6:
                return new CborTag(arg, dec(b, pos, depth + 1));
            default:
                throw new CsilCborException("csil cbor: unexpected major type");
        }
    }

    public static long asI64(CborValue v) {
        if (v instanceof CborUint x) {
            if (x.value() < 0) {
                throw new CsilCborException("csil cbor: integer overflows int64");
            }
            return x.value();
        }
        if (v instanceof CborInt x) {
            return x.value();
        }
        throw new CsilCborException("csil cbor: expected integer");
    }

    public static long asU64(CborValue v) {
        if (v instanceof CborUint x) {
            return x.value();
        }
        if (v instanceof CborInt x) {
            if (x.value() < 0) {
                throw new CsilCborException("csil cbor: negative integer where unsigned expected");
            }
            return x.value();
        }
        throw new CsilCborException("csil cbor: expected unsigned integer");
    }

    public static double asF64(CborValue v) {
        if (v instanceof CborFloat x) {
            return x.value();
        }
        if (v instanceof CborUint x) {
            return (double) x.value();
        }
        if (v instanceof CborInt x) {
            return (double) x.value();
        }
        throw new CsilCborException("csil cbor: expected float");
    }

    public static boolean asBool(CborValue v) {
        if (v instanceof CborBool x) {
            return x.value();
        }
        throw new CsilCborException("csil cbor: expected bool");
    }

    public static String asText(CborValue v) {
        if (v instanceof CborText x) {
            return x.value();
        }
        throw new CsilCborException("csil cbor: expected text");
    }

    public static byte[] asBytes(CborValue v) {
        if (v instanceof CborBytes x) {
            return x.value();
        }
        throw new CsilCborException("csil cbor: expected bytes");
    }

    public static java.util.List<CborValue> asArray(CborValue v) {
        if (v instanceof CborArray x) {
            return x.items();
        }
        throw new CsilCborException("csil cbor: expected array");
    }

    public static java.util.List<CborEntry> asMap(CborValue v) {
        if (v instanceof CborMap x) {
            return x.entries();
        }
        throw new CsilCborException("csil cbor: expected map");
    }

    public static CborValue mapGet(CborValue v, String key) {
        if (v instanceof CborMap x) {
            for (CborEntry e : x.entries()) {
                if (e.key() instanceof CborText k && k.value().equals(key)) {
                    return e.val();
                }
            }
        }
        return null;
    }

    public static CborValue require(CborValue v, String key) {
        CborValue x = mapGet(v, key);
        if (x == null) {
            throw new CsilCborException("csil cbor: missing field " + key);
        }
        return x;
    }

    public static <T> T expectLiteral(CborValue actual, CborValue expected, T value) {
        if (!cborEqual(actual, expected)) {
            throw new CsilCborException("csil cbor: literal mismatch");
        }
        return value;
    }

    private static boolean cborEqual(CborValue actual, CborValue expected) {
        if (actual instanceof CborBytes a && expected instanceof CborBytes b) {
            return java.util.Arrays.equals(a.value(), b.value());
        }
        if (actual instanceof CborArray a && expected instanceof CborArray b) {
            if (a.items().size() != b.items().size()) return false;
            for (int i = 0; i < a.items().size(); i++) {
                if (!cborEqual(a.items().get(i), b.items().get(i))) return false;
            }
            return true;
        }
        return actual.equals(expected);
    }

    public static <E> CborValue encArray(java.util.List<E> xs, java.util.function.Function<E, CborValue> f) {
        java.util.List<CborValue> items = new java.util.ArrayList<>(xs.size());
        for (E x : xs) {
            items.add(f.apply(x));
        }
        return new CborArray(items);
    }

    public static <K, V> CborValue encMap(java.util.Map<K, V> m, java.util.function.Function<K, CborValue> kf, java.util.function.Function<V, CborValue> vf) {
        java.util.List<CborEntry> entries = new java.util.ArrayList<>(m.size());
        for (java.util.Map.Entry<K, V> e : m.entrySet()) {
            entries.add(new CborEntry(kf.apply(e.getKey()), vf.apply(e.getValue())));
        }
        return new CborMap(entries);
    }

    public static <E> java.util.List<E> decArray(CborValue v, java.util.function.Function<CborValue, E> f) {
        java.util.List<CborValue> xs = asArray(v);
        java.util.List<E> out = new java.util.ArrayList<>(xs.size());
        for (CborValue x : xs) {
            out.add(f.apply(x));
        }
        return out;
    }

    public static <K, V> java.util.Map<K, V> decMap(CborValue v, java.util.function.Function<CborValue, K> kf, java.util.function.Function<CborValue, V> vf) {
        java.util.Map<K, V> out = new java.util.LinkedHashMap<>();
        for (CborEntry e : asMap(v)) {
            out.put(kf.apply(e.key()), vf.apply(e.val()));
        }
        return out;
    }
"#;

/// Timestamp (CBOR tag 0, RFC3339, always UTC) codec, appended only when the spec uses
/// `timestamp` so `java.time.Instant` is never an unused import. `Instant` is the UTC
/// instant type and its `toString` is RFC3339 with a `Z` offset, sub-second preserved.
const CODEC_TIMESTAMP_JAVA: &str = r#"    public static CborValue encTimestamp(java.time.Instant t) {
        return new CborTag(0, new CborText(t.toString()));
    }

    public static java.time.Instant asTimestamp(CborValue v) {
        if (v instanceof CborTag t && t.num() == 0 && t.inner() instanceof CborText s) {
            return java.time.Instant.parse(s.value());
        }
        throw new CsilCborException("csil cbor: expected CBOR tag 0 timestamp");
    }
"#;

/// Generic enum-scalar helpers shared by a mixed-kind NAMED enum's codec
/// (`emit_mixed_enum_codec`) and an un-hoisted inline all-literal choice field
/// (`java_enc_value`/`java_dec_value`'s `Choice` arm, via `inline_enum_literals`): no
/// single `CborXxx`/`asXxx` pair fits every member's CBOR kind, so `asEnumScalar` reads
/// an item by its own CBOR major type and `encEnumScalar` writes a boxed Java value back
/// by its own runtime type — both recursing element-by-element through a `CborArray`/
/// `List` so an `Array` literal member is representable too. `requireEnumMember`
/// validates a decoded scalar against ONE choice's own declared vocabulary (the caller
/// supplies the membership predicate, since the vocabulary differs per choice).
/// Appended only when a generated enum decoder actually calls one of these.
const CODEC_ENUM_SCALAR_JAVA: &str = r#"    public static Object asEnumScalar(CborValue v) {
        if (v instanceof CborUint x) return x.value();
        if (v instanceof CborInt x) return x.value();
        if (v instanceof CborFloat x) return x.value();
        if (v instanceof CborBool x) return x.value();
        if (v instanceof CborText x) return x.value();
        if (v instanceof CborBytes x) return x.value();
        if (v instanceof CborNull) return null;
        if (v instanceof CborArray x) {
            java.util.List<Object> csilItems = new java.util.ArrayList<>();
            for (CborValue csilItem : x.items()) {
                csilItems.add(asEnumScalar(csilItem));
            }
            return csilItems;
        }
        throw new CsilCborException("csil cbor: expected an enum scalar");
    }

    public static CborValue encEnumScalar(Object v) {
        if (v instanceof Long x) return new CborInt(x);
        if (v instanceof Double x) return new CborFloat(x);
        if (v instanceof Boolean x) return new CborBool(x);
        if (v instanceof String x) return new CborText(x);
        if (v instanceof byte[] x) return new CborBytes(x);
        if (v == null) return new CborNull();
        if (v instanceof java.util.List<?> x) {
            java.util.List<CborValue> csilItems = new java.util.ArrayList<>();
            for (Object csilItem : x) {
                csilItems.add(encEnumScalar(csilItem));
            }
            return new CborArray(csilItems);
        }
        throw new CsilCborException("csil cbor: enum value has an unsupported runtime type");
    }

    public static <T> T requireEnumMember(T value, java.util.function.Predicate<T> member) {
        if (!member.test(value)) {
            throw new CsilCborException("csil cbor: value " + value + " is not a member of the declared enum");
        }
        return value;
    }
"#;

/// Decimal (CBOR tag 4 `[exponent, mantissa]`, exact) codec, appended only when the
/// spec uses `decimal`. `BigDecimal` is Java's exact decimal; its unscaled value and
/// scale map straight onto the tag-4 wire form, with a bignum fallback (tag 2/3) when
/// the mantissa exceeds 64 bits so no precision is lost.
const CODEC_DECIMAL_JAVA: &str = r#"    public static CborValue encDecimal(java.math.BigDecimal d) {
        long exp = -(long) d.scale();
        return new CborTag(4, new CborArray(java.util.List.of(new CborInt(exp), encBigInt(d.unscaledValue()))));
    }

    public static java.math.BigDecimal asDecimal(CborValue v) {
        if (v instanceof CborTag t && t.num() == 4 && t.inner() instanceof CborArray a && a.items().size() == 2) {
            long exp = asI64(a.items().get(0));
            java.math.BigInteger mant = decBigInt(a.items().get(1));
            return new java.math.BigDecimal(mant, (int) -exp);
        }
        throw new CsilCborException("csil cbor: expected CBOR tag 4 decimal");
    }

    private static CborValue encBigInt(java.math.BigInteger m) {
        if (m.bitLength() <= 63) {
            return new CborInt(m.longValue());
        }
        if (m.signum() >= 0 && m.bitLength() <= 64) {
            return new CborUint(m.longValue());
        }
        if (m.signum() >= 0) {
            return new CborTag(2, new CborBytes(magnitudeBytes(m)));
        }
        java.math.BigInteger n = m.negate().subtract(java.math.BigInteger.ONE);
        return new CborTag(3, new CborBytes(magnitudeBytes(n)));
    }

    private static java.math.BigInteger decBigInt(CborValue v) {
        if (v instanceof CborUint x) {
            return unsignedToBigInteger(x.value());
        }
        if (v instanceof CborInt x) {
            return java.math.BigInteger.valueOf(x.value());
        }
        if (v instanceof CborTag t && t.inner() instanceof CborBytes bs) {
            java.math.BigInteger mag = new java.math.BigInteger(1, bs.value());
            if (t.num() == 2) {
                return mag;
            }
            if (t.num() == 3) {
                return mag.negate().subtract(java.math.BigInteger.ONE);
            }
        }
        throw new CsilCborException("csil cbor: expected integer mantissa");
    }

    private static java.math.BigInteger unsignedToBigInteger(long v) {
        if (v >= 0) {
            return java.math.BigInteger.valueOf(v);
        }
        return java.math.BigInteger.valueOf(v).add(java.math.BigInteger.ONE.shiftLeft(64));
    }

    private static byte[] magnitudeBytes(java.math.BigInteger m) {
        byte[] full = m.toByteArray();
        int start = 0;
        while (start < full.length - 1 && full[start] == 0) {
            start++;
        }
        return java.util.Arrays.copyOfRange(full, start, full.length);
    }
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decoder_checks_declared_lengths_before_conversion_or_allocation() {
        assert_eq!(
            CODEC_RUNTIME_JAVA
                .matches("if (Long.compareUnsigned(arg, b.length - pos[0]) > 0)")
                .count(),
            4
        );
        assert!(CODEC_RUNTIME_JAVA.contains("array length exceeds remaining input"));
        assert!(CODEC_RUNTIME_JAVA.contains("map length exceeds remaining input"));
        assert!(CODEC_RUNTIME_JAVA.contains("if (depth > 64)"));
        assert!(CODEC_RUNTIME_JAVA.contains("CodingErrorAction.REPORT"));
    }
    use csilgen_common::{
        CsilFieldMetadata, CsilGroupExpression, CsilPosition, CsilRule, CsilServiceOperation,
        CsilSpecSerialized, GeneratorConfig,
    };
    use std::collections::HashMap;

    fn meta() -> GeneratorMetadata {
        GeneratorMetadata {
            name: "java".to_string(),
            version: "1.0.0".to_string(),
            description: String::new(),
            target: "java".to_string(),
            capabilities: vec![],
            author: None,
            homepage: None,
        }
    }

    fn input_for(rules: Vec<CsilRule>, target: &str) -> WasmGeneratorInput {
        let service_count = rules
            .iter()
            .filter(|r| matches!(r.rule_type, CsilRuleType::ServiceDef(_)))
            .count();
        WasmGeneratorInput {
            csil_spec: CsilSpecSerialized {
                rules,
                source_content: None,
                service_count,
                fields_with_metadata_count: 0,
            },
            config: GeneratorConfig {
                target: target.to_string(),
                output_dir: "/tmp".to_string(),
                options: HashMap::new(),
            },
            generator_metadata: meta(),
        }
    }

    fn rule(name: &str, rule_type: CsilRuleType) -> CsilRule {
        CsilRule {
            name: name.to_string(),
            rule_type,
            position: CsilPosition {
                line: 1,
                column: 1,
                offset: 0,
            },
            doc_comments: Vec::new(),
        }
    }

    fn bare(name: &str, ty: CsilTypeExpression, occ: Option<CsilOccurrence>) -> CsilGroupEntry {
        CsilGroupEntry {
            key: Some(CsilGroupKey::Bare(name.to_string())),
            value_type: ty,
            occurrence: occ,
            metadata: vec![],
            doc_comments: Vec::new(),
        }
    }

    fn builtin(name: &str) -> CsilTypeExpression {
        CsilTypeExpression::Builtin(name.to_string())
    }

    fn op(
        name: &str,
        input: CsilTypeExpression,
        output: CsilTypeExpression,
        dir: CsilServiceDirection,
        wire_id: Option<u64>,
    ) -> CsilServiceOperation {
        CsilServiceOperation {
            name: name.to_string(),
            input_type: input,
            output_type: output,
            direction: dir,
            position: CsilPosition {
                line: 1,
                column: 1,
                offset: 0,
            },
            doc_comments: Vec::new(),
            wire_id,
        }
    }

    fn file<'a>(files: &'a [GeneratedFile], suffix: &str) -> &'a GeneratedFile {
        files
            .iter()
            .find(|f| f.path.ends_with(suffix))
            .unwrap_or_else(|| panic!("no file ending in {suffix}; got {:?}", paths(files)))
    }

    fn paths(files: &[GeneratedFile]) -> Vec<String> {
        files.iter().map(|f| f.path.clone()).collect()
    }

    /// An optional `bytes` field carries three distinct states — absent,
    /// present-and-empty, present-and-non-empty — and the codec must decide presence by
    /// whether the value is set, never by whether it is non-empty (cbor-wire-contract.md
    /// "Optional fields"). A `length > 0` guard would collapse present-empty into absent
    /// and silently lose a caller's "replace this with nothing".
    #[test]
    fn optional_bytes_encodes_on_presence_not_emptiness() {
        let group = CsilGroupExpression {
            entries: vec![
                bare("id", builtin("text"), None),
                bare("payload", builtin("bytes"), Some(CsilOccurrence::Optional)),
            ],
        };
        let files = generate_java(&input_for(
            vec![rule("UpdateRequest", CsilRuleType::GroupDef(group))],
            "java",
        ))
        .unwrap();
        let codec = &file(&files, "csilgen/generated/CsilCbor.java").content;

        // Encode gates on `!= null` (presence), not on the array length.
        assert!(
            codec.contains("if (v.payload() != null) {"),
            "encode must gate on presence, not emptiness:\n{codec}"
        );
        assert!(
            !codec.contains("v.payload().length > 0"),
            "encode must not gate on emptiness:\n{codec}"
        );
        // Decode maps a missing key to null but keeps a present zero-length byte string
        // as a zero-length array, so the three states stay distinct.
        assert!(
            codec.contains("payload = csilField != null ? asBytes(csilField) : null;"),
            "decode must gate on key presence:\n{codec}"
        );
    }

    #[test]
    fn record_maps_snake_fields_to_camel_keeps_wire_name() {
        let group = CsilGroupExpression {
            entries: vec![
                bare("current_state", builtin("text"), None),
                bare(
                    "retry_count",
                    builtin("int"),
                    Some(CsilOccurrence::Optional),
                ),
                bare("blob", builtin("bytes"), None),
            ],
        };
        let files = generate_java(&input_for(
            vec![rule("TaskState", CsilRuleType::GroupDef(group))],
            "java",
        ))
        .unwrap();
        let f = file(&files, "csilgen/generated/TaskState.java");
        assert!(f.content.contains("public record TaskState("));
        // snake_case -> camelCase identifier, wire key kept verbatim in a comment.
        assert!(
            f.content
                .contains("String currentState /* wire: \"current_state\" */")
        );
        // optional int becomes a nullable boxed Long.
        assert!(
            f.content
                .contains("Long retryCount /* wire: \"retry_count\" */")
        );
        // a byte[] component forces a content-aware equals override, with the JDK type
        // hoisted to an import and referenced by simple name.
        assert!(f.content.contains("byte[] blob"));
        assert!(f.content.contains("import java.util.Arrays;"));
        assert!(f.content.contains("Arrays.equals(blob, o.blob)"));
        assert!(!f.content.contains("java.util.Arrays.equals"));
        assert!(f.content.contains("@Override\n    public int hashCode()"));
    }

    #[test]
    fn timestamp_and_decimal_map_to_jdk_types() {
        let group = CsilGroupExpression {
            entries: vec![
                bare("created_at", builtin("timestamp"), None),
                bare("amount", builtin("decimal"), None),
            ],
        };
        let files = generate_java(&input_for(
            vec![rule("Money", CsilRuleType::GroupDef(group))],
            "java-typesonly",
        ))
        .unwrap();
        let f = file(&files, "Money.java");
        // JDK types are imported and used by simple name, not inline-qualified.
        assert!(f.content.contains("import java.time.Instant;"));
        assert!(f.content.contains("import java.math.BigDecimal;"));
        assert!(f.content.contains("Instant createdAt"));
        assert!(f.content.contains("BigDecimal amount"));
        assert!(!f.content.contains("java.time.Instant createdAt"));
    }

    #[test]
    fn validation_runs_in_canonical_constructor() {
        let constrained = CsilTypeExpression::Constrained {
            base_type: Box::new(builtin("text")),
            constraints: vec![CsilControlOperator::Size(CsilSizeConstraint::Min(3))],
        };
        let mut entry = bare("name", constrained, None);
        entry.metadata.push(CsilFieldMetadata::Constraint(
            CsilValidationConstraint::MaxLength(10),
        ));
        let group = CsilGroupExpression {
            entries: vec![entry],
        };
        let files = generate_java(&input_for(
            vec![rule("User", CsilRuleType::GroupDef(group))],
            "java",
        ))
        .unwrap();
        let f = file(&files, "User.java");
        assert!(f.content.contains("public User {"));
        assert!(f.content.contains("name.length() < 3"));
        assert!(f.content.contains("name.length() > 10"));
        assert!(f.content.contains("throw new IllegalArgumentException("));
    }

    #[test]
    fn type_choice_becomes_sealed_interface() {
        let files = generate_java(&input_for(
            vec![rule(
                "Result",
                CsilRuleType::TypeChoice(vec![
                    CsilTypeExpression::Reference("Ok".to_string()),
                    CsilTypeExpression::Reference("Err".to_string()),
                ]),
            )],
            "java-typesonly",
        ))
        .unwrap();
        let f = file(&files, "Result.java");
        assert!(
            f.content
                .contains("public sealed interface Result permits Result.OkCase, Result.ErrCase")
        );
        assert!(
            f.content
                .contains("record OkCase(Ok value) implements Result {}")
        );
        assert!(
            f.content
                .contains("record ErrCase(Err value) implements Result {}")
        );
    }

    #[test]
    fn client_target_emits_typed_blocking_client() {
        let svc = CsilServiceDefinition {
            operations: vec![op(
                "submit-task",
                CsilTypeExpression::Reference("SubmitTaskRequest".to_string()),
                CsilTypeExpression::Choice(vec![
                    CsilTypeExpression::Reference("SubmitTaskResponse".to_string()),
                    CsilTypeExpression::Reference("ServiceError".to_string()),
                ]),
                CsilServiceDirection::Unidirectional,
                None,
            )],
            wire_id: None,
        };
        // The request/response must be records for the typed-codec path to engage.
        let request = CsilGroupExpression {
            entries: vec![bare("queue", builtin("text"), None)],
        };
        let response = CsilGroupExpression {
            entries: vec![bare("uuid", builtin("text"), None)],
        };
        let files = generate_java(&input_for(
            vec![
                rule("SubmitTaskRequest", CsilRuleType::GroupDef(request)),
                rule("SubmitTaskResponse", CsilRuleType::GroupDef(response)),
                rule("CorndogsService", CsilRuleType::ServiceDef(svc)),
            ],
            "java-client",
        ))
        .unwrap();
        assert!(files.iter().any(|f| f.path.ends_with("Transport.java")));
        assert!(
            files
                .iter()
                .any(|f| f.path.ends_with("ClientException.java"))
        );
        // The self-contained per-record codec rides along with the client.
        assert!(files.iter().any(|f| f.path.ends_with("CsilCbor.java")));
        let f = file(&files, "CorndogsClient.java");
        assert!(f.content.contains("public final class CorndogsClient"));
        // ServiceError stripped from the typed return; method is camelCase.
        assert!(f.content.contains(
            "public SubmitTaskResponse submitTask(SubmitTaskRequest req) throws ClientException"
        ));
        // The client encodes the request and decodes the response through the codec over
        // a dumb byte seam: verbatim CSIL service and op names, matching peers.
        assert!(f.content.contains(
            "return CsilCbor.decodeSubmitTaskResponse(transport.call(\"CorndogsService\", \"submit-task\", CsilCbor.encodeSubmitTaskRequest(req)));"
        ));
        // no server interface for the client target.
        assert!(!files.iter().any(|f| f.path.ends_with("Corndogs.java")));
    }

    #[test]
    fn non_record_op_boundaries_get_client_methods_and_per_op_codecs() {
        let arr = |elem: CsilTypeExpression| CsilTypeExpression::Array {
            element_type: Box::new(elem),
            occurrence: None,
        };
        let svc = CsilServiceDefinition {
            operations: vec![
                // record -> record (the only shape the old filter kept)
                op(
                    "create-member",
                    CsilTypeExpression::Reference("Member".to_string()),
                    CsilTypeExpression::Reference("Member".to_string()),
                    CsilServiceDirection::Unidirectional,
                    None,
                ),
                // scalar-id (alias) request -> record response
                op(
                    "get-member",
                    CsilTypeExpression::Reference("MemberID".to_string()),
                    CsilTypeExpression::Reference("Member".to_string()),
                    CsilServiceDirection::Unidirectional,
                    None,
                ),
                // record request -> bare-array response
                op(
                    "list-members",
                    CsilTypeExpression::Reference("ListMembersRequest".to_string()),
                    arr(CsilTypeExpression::Reference("Member".to_string())),
                    CsilServiceDirection::Unidirectional,
                    None,
                ),
                // scalar-id request -> scalar response
                op(
                    "delete-task",
                    CsilTypeExpression::Reference("TaskID".to_string()),
                    builtin("bool"),
                    CsilServiceDirection::Unidirectional,
                    None,
                ),
            ],
            wire_id: None,
        };
        let member = CsilGroupExpression {
            entries: vec![
                bare(
                    "id",
                    CsilTypeExpression::Reference("MemberID".to_string()),
                    None,
                ),
                bare("name", builtin("text"), None),
            ],
        };
        let list_req = CsilGroupExpression {
            entries: vec![bare(
                "limit",
                builtin("uint"),
                Some(CsilOccurrence::Optional),
            )],
        };
        let files = generate_java(&input_for(
            vec![
                rule("MemberID", CsilRuleType::TypeDef(builtin("text"))),
                rule("TaskID", CsilRuleType::TypeDef(builtin("text"))),
                rule("Member", CsilRuleType::GroupDef(member)),
                rule("ListMembersRequest", CsilRuleType::GroupDef(list_req)),
                rule("MemberService", CsilRuleType::ServiceDef(svc)),
            ],
            "java-client",
        ))
        .unwrap();

        let client = file(&files, "MemberClient.java");
        // Every op gets a method now — scalar-id request, bare-array and scalar responses
        // included — and none is dropped with a note.
        assert!(
            client
                .content
                .contains("public Member getMember(MemberId req) throws ClientException")
        );
        assert!(client.content.contains(
            "public List<Member> listMembers(ListMembersRequest req) throws ClientException"
        ));
        assert!(
            client
                .content
                .contains("public Boolean deleteTask(TaskId req) throws ClientException")
        );
        assert!(!client.content.contains("handle it manually"));
        assert!(!client.content.contains("non-record payload"));
        // A record boundary keeps its `encode<T>`/`decode<T>` wrapper; a non-record
        // boundary rides the op-keyed per-op helpers.
        assert!(client.content.contains("CsilCbor.encodeMember(req)"));
        assert!(
            client
                .content
                .contains("CsilCbor.encodeMemberGetMemberRequest(req)")
        );
        assert!(
            client
                .content
                .contains("CsilCbor.decodeMemberListMembersResponse(transport.call(")
        );
        assert!(
            client
                .content
                .contains("CsilCbor.decodeMemberDeleteTaskResponse(transport.call(")
        );

        let codec = file(&files, "CsilCbor.java");
        // The non-record op boundaries are exposed as public per-op helpers a server in
        // another package can compose decode(request)/encode(response) from.
        assert!(
            codec
                .content
                .contains("public static MemberId decodeMemberGetMemberRequest(byte[] csilData)")
        );
        assert!(
            codec.content.contains(
                "public static byte[] encodeMemberListMembersResponse(List<Member> csilV)"
            )
        );
        assert!(
            codec
                .content
                .contains("public static byte[] encodeMemberDeleteTaskResponse(Boolean csilV)")
        );
    }

    #[test]
    fn transport_seam_is_a_dumb_byte_carrier() {
        let svc = CsilServiceDefinition {
            operations: vec![op(
                "ping",
                builtin("null"),
                CsilTypeExpression::Reference("Pong".to_string()),
                CsilServiceDirection::Unidirectional,
                None,
            )],
            wire_id: None,
        };
        let files = generate_java(&input_for(
            vec![
                rule(
                    "Pong",
                    CsilRuleType::GroupDef(CsilGroupExpression {
                        entries: vec![bare("ok", builtin("bool"), None)],
                    }),
                ),
                rule("HealthService", CsilRuleType::ServiceDef(svc)),
            ],
            "java-client",
        ))
        .unwrap();
        let t = file(&files, "Transport.java");
        // The seam moves bytes only — no reflection Class<Resp>, no Object payload.
        assert!(t.content.contains(
            "byte[] call(String service, String op, byte[] req) throws ClientException;"
        ));
        assert!(!t.content.contains("Class<Resp>"));
        assert!(!t.content.contains("Object req"));
    }

    #[test]
    fn server_target_emits_interface_and_router_twins() {
        let svc = CsilServiceDefinition {
            operations: vec![
                op(
                    "list-events",
                    CsilTypeExpression::Reference("Q".to_string()),
                    CsilTypeExpression::Reference("R".to_string()),
                    CsilServiceDirection::Unidirectional,
                    Some(1),
                ),
                op(
                    "play",
                    CsilTypeExpression::Reference("Move".to_string()),
                    CsilTypeExpression::Reference("Ack".to_string()),
                    CsilServiceDirection::Bidirectional,
                    Some(2),
                ),
            ],
            wire_id: Some(7),
        };
        let files = generate_java(&input_for(
            vec![rule("Match", CsilRuleType::ServiceDef(svc))],
            "java",
        ))
        .unwrap();
        assert!(files.iter().any(|f| f.path.ends_with("Codec.java")));
        assert!(
            files
                .iter()
                .any(|f| f.path.ends_with("EncodedMessage.java"))
        );

        let iface = file(&files, "Match.java");
        assert!(iface.content.contains("public interface Match {"));
        assert!(iface.content.contains("R listEvents(Q req);"));
        assert!(iface.content.contains("void play(Move msg);"));

        let router = file(&files, "MatchRouter.java");
        // wire-id ordinals
        assert!(
            router
                .content
                .contains("public static final long MatchServiceWireId = 7L;")
        );
        assert!(
            router
                .content
                .contains("public static final long MatchOpPlayWireId = 2L;")
        );
        // verbose router dispatches on method name
        assert!(router
            .content
            .contains("public static void routeMatchChannel(Match handlers, Codec codec, String method, byte[] data)"));
        assert!(router.content.contains("case \"play\" -> {"));
        assert!(router.content.contains("handlers.play(msg);"));
        // compact router twin dispatches on the ordinal
        assert!(router
            .content
            .contains("public static void routeMatchChannelCompact(Match handlers, Codec codec, long op, byte[] data)"));
        assert!(router.content.contains("if (op == 2L) {"));
        // outbound encoder for the bidi op
        assert!(
            router
                .content
                .contains("public static EncodedMessage encodeMatchPlay(Codec codec, Ack msg)")
        );
        assert!(
            router
                .content
                .contains("return new EncodedMessage(\"play\", codec.encode(msg));")
        );
    }

    #[test]
    fn push_only_op_drops_request_parameter() {
        let svc = CsilServiceDefinition {
            operations: vec![op(
                "ping",
                builtin("null"),
                CsilTypeExpression::Reference("Pong".to_string()),
                CsilServiceDirection::Unidirectional,
                None,
            )],
            wire_id: None,
        };
        let files = generate_java(&input_for(
            vec![
                rule(
                    "Pong",
                    CsilRuleType::GroupDef(CsilGroupExpression {
                        entries: vec![bare("ok", builtin("bool"), None)],
                    }),
                ),
                rule("Health", CsilRuleType::ServiceDef(svc)),
            ],
            "java-client",
        ))
        .unwrap();
        let f = file(&files, "HealthClient.java");
        assert!(
            f.content
                .contains("public Pong ping() throws ClientException")
        );
        // A null-input op sends a null payload; the response decodes through the codec.
        assert!(
            f.content.contains(
                "return CsilCbor.decodePong(transport.call(\"Health\", \"ping\", null));"
            )
        );
    }

    #[test]
    fn unknown_subtarget_errors() {
        let err = generate_java(&input_for(
            vec![rule(
                "M",
                CsilRuleType::GroupDef(CsilGroupExpression { entries: vec![] }),
            )],
            "java-bogus",
        ));
        assert!(err.is_err());
    }

    #[test]
    fn field_description_becomes_param_javadoc_and_zero_floor_is_skipped() {
        let mut described = bare("display_name", builtin("text"), None);
        described
            .metadata
            .push(CsilFieldMetadata::Description("The shown name".to_string()));
        // A `.size (0..40)` lower bound of zero is vacuous and must not emit a `< 0` guard.
        let bounded_ty = CsilTypeExpression::Constrained {
            base_type: Box::new(builtin("text")),
            constraints: vec![CsilControlOperator::Size(CsilSizeConstraint::Range {
                min: 0,
                max: 40,
            })],
        };
        let bio = bare("bio", bounded_ty, None);
        let group = CsilGroupExpression {
            entries: vec![described, bio],
        };
        let mut r = rule("Profile", CsilRuleType::GroupDef(group));
        r.doc_comments = vec!["A user profile.".to_string()];
        let files = generate_java(&input_for(vec![r], "java-typesonly")).unwrap();
        let f = file(&files, "Profile.java");
        assert!(f.content.contains("/**\n * A user profile.\n"));
        assert!(f.content.contains(" * @param displayName The shown name"));
        // vacuous zero floor skipped; real upper bound kept.
        assert!(!f.content.contains("bio.length() < 0"));
        assert!(f.content.contains("bio.length() > 40"));
    }

    #[test]
    fn literal_choice_collapses_to_its_scalar() {
        // `text / "a" / "b"` is a string-constrained scalar, not a multi-type union.
        let files = generate_java(&input_for(
            vec![rule(
                "Status",
                CsilRuleType::TypeDef(CsilTypeExpression::Choice(vec![
                    builtin("text"),
                    CsilTypeExpression::Literal(CsilLiteralValue::Text("a".to_string())),
                    CsilTypeExpression::Literal(CsilLiteralValue::Text("b".to_string())),
                ])),
            )],
            "java-typesonly",
        ))
        .unwrap();
        let f = file(&files, "Status.java");
        assert!(f.content.contains("public record Status(String value) {}"));
    }

    #[test]
    fn emitted_files_use_spaces_not_tabs() {
        let svc = CsilServiceDefinition {
            operations: vec![op(
                "do-thing",
                CsilTypeExpression::Reference("In".to_string()),
                CsilTypeExpression::Reference("Out".to_string()),
                CsilServiceDirection::Unidirectional,
                None,
            )],
            wire_id: None,
        };
        let files = generate_java(&input_for(
            vec![rule("ThingService", CsilRuleType::ServiceDef(svc))],
            "java-client",
        ))
        .unwrap();
        // The whole surface is space-indented; a stray tab would break the house style.
        for f in &files {
            assert!(!f.content.contains('\t'), "tab found in {}", f.path);
        }
    }

    #[test]
    fn map_and_array_box_primitive_generic_args() {
        assert_eq!(
            map_type(&CsilTypeExpression::Array {
                element_type: Box::new(builtin("int")),
                occurrence: None,
            }),
            "java.util.List<Long>"
        );
        assert_eq!(
            map_type(&CsilTypeExpression::Map {
                key: Box::new(builtin("text")),
                value: Box::new(builtin("bool")),
                occurrence: None,
            }),
            "java.util.Map<String, Boolean>"
        );
    }

    /// The corndogs `java-client` spec used by the codec/round-trip tests: a `Task`
    /// record exercising every scalar/optional/map/list shape, a wrapping request, and
    /// the `submit-task` operation.
    fn corndogs_rules() -> Vec<CsilRule> {
        let task = CsilGroupExpression {
            entries: vec![
                bare("uuid", builtin("text"), None),
                bare("current_state", builtin("text"), None),
                bare("payload", builtin("bytes"), None),
                bare("priority", builtin("int"), Some(CsilOccurrence::Optional)),
                bare(
                    "labels",
                    CsilTypeExpression::Map {
                        key: Box::new(builtin("text")),
                        value: Box::new(builtin("int")),
                        occurrence: None,
                    },
                    None,
                ),
                bare(
                    "tags",
                    CsilTypeExpression::Array {
                        element_type: Box::new(builtin("text")),
                        occurrence: None,
                    },
                    None,
                ),
            ],
        };
        let request = CsilGroupExpression {
            entries: vec![
                bare(
                    "task",
                    CsilTypeExpression::Reference("Task".to_string()),
                    None,
                ),
                bare("queue", builtin("text"), None),
            ],
        };
        let svc = CsilServiceDefinition {
            operations: vec![op(
                "submit-task",
                CsilTypeExpression::Reference("SubmitTaskRequest".to_string()),
                CsilTypeExpression::Choice(vec![
                    CsilTypeExpression::Reference("Task".to_string()),
                    CsilTypeExpression::Reference("ServiceError".to_string()),
                ]),
                CsilServiceDirection::Unidirectional,
                None,
            )],
            wire_id: None,
        };
        // A record reachable only through a map-of-record alias, to prove the alias arm
        // recurses into the per-record codec (`M = {* text => SomeRecord}`).
        let item = CsilGroupExpression {
            entries: vec![bare("name", builtin("text"), None)],
        };
        // A record whose fields are transparent map aliases — the regression case: a named
        // map alias must round-trip, not stub to an empty map.
        let bag = CsilGroupExpression {
            entries: vec![
                bare(
                    "counts",
                    CsilTypeExpression::Reference("StringInt64Map".to_string()),
                    None,
                ),
                bare(
                    "items",
                    CsilTypeExpression::Reference("ItemMap".to_string()),
                    None,
                ),
            ],
        };
        vec![
            rule("Task", CsilRuleType::GroupDef(task)),
            rule("SubmitTaskRequest", CsilRuleType::GroupDef(request)),
            rule("Item", CsilRuleType::GroupDef(item)),
            rule(
                "StringInt64Map",
                CsilRuleType::TypeDef(CsilTypeExpression::Map {
                    key: Box::new(builtin("text")),
                    value: Box::new(builtin("int")),
                    occurrence: None,
                }),
            ),
            rule(
                "ItemMap",
                CsilRuleType::TypeDef(CsilTypeExpression::Map {
                    key: Box::new(builtin("text")),
                    value: Box::new(CsilTypeExpression::Reference("Item".to_string())),
                    occurrence: None,
                }),
            ),
            rule("Bag", CsilRuleType::GroupDef(bag)),
            rule("CorndogsService", CsilRuleType::ServiceDef(svc)),
        ]
    }

    #[test]
    fn codec_emits_self_contained_value_model_and_per_record_pairs() {
        let files = generate_java(&input_for(corndogs_rules(), "java-client")).unwrap();
        let f = file(&files, "CsilCbor.java");
        // A self-contained canonical-CBOR value model with every variant.
        assert!(f.content.contains("public sealed interface CborValue"));
        for variant in [
            "CborUint",
            "CborInt",
            "CborBool",
            "CborFloat",
            "CborNull",
            "CborText",
            "CborBytes",
            "CborArray",
            "CborMap",
            "CborTag",
        ] {
            assert!(
                f.content.contains(&format!("record {variant}(")),
                "missing variant {variant}"
            );
        }
        // Public per-record byte wrappers.
        assert!(
            f.content
                .contains("public static byte[] encodeTask(Task v)")
        );
        assert!(
            f.content
                .contains("public static Task decodeTask(byte[] data)")
        );
        assert!(
            f.content
                .contains("public static byte[] encodeSubmitTaskRequest(SubmitTaskRequest v)")
        );
        // text -> CborText (major 3); bytes -> CborBytes (major 2).
        assert!(
            f.content
                .contains("new CborEntry(new CborText(\"uuid\"), new CborText(v.uuid()))")
        );
        assert!(
            f.content
                .contains("new CborEntry(new CborText(\"payload\"), new CborBytes(v.payload()))")
        );
        // Optional absent -> omitted on encode, null on decode.
        assert!(f.content.contains("if (v.priority() != null) {"));
        assert!(
            f.content
                .contains("priority = csilField != null ? asI64(csilField) : null;")
        );
        // Composite map/list go through the generic helpers.
        assert!(f.content.contains(
            "encMap(v.labels(), csilK0 -> new CborText(csilK0), csilV0 -> new CborInt(csilV0))"
        ));
        assert!(
            f.content
                .contains("encArray(v.tags(), csilElem0 -> new CborText(csilElem0))")
        );
        // The nested record reference recurses into its own codec.
        assert!(
            f.content
                .contains("new CborEntry(new CborText(\"task\"), encTask(v.task()))")
        );
        // A named map alias field reaches through its wrapper record's `.value()` into the
        // underlying map codec instead of stubbing to `CborNull`/`null` (the regression).
        assert!(f.content.contains(
            "encMap((v.counts()).value(), csilK0 -> new CborText(csilK0), csilV0 -> new CborInt(csilV0))"
        ));
        assert!(
            f.content
                .contains("new StringInt64Map(decMap(require(csilRoot, \"counts\")")
        );
        // A map-of-record alias recurses to the referenced record's own codec.
        assert!(f.content.contains(
            "encMap((v.items()).value(), csilK0 -> new CborText(csilK0), csilV0 -> encItem(csilV0))"
        ));
        // Canonical RFC 8949 key order: among the length-4 keys, "tags" precedes "uuid".
        let tags_at = f.content.find("new CborText(\"tags\")").unwrap();
        let uuid_at = f.content.find("new CborText(\"uuid\")").unwrap();
        assert!(tags_at < uuid_at, "map keys not in canonical order");
        // The JDK types the codec writes are hoisted to imports.
        assert!(f.content.contains("import java.util.ArrayList;"));
        assert!(f.content.contains("import java.util.function.Function;"));
        // No tabs anywhere in the generated codec.
        assert!(!f.content.contains('\t'));
    }

    /// The generic decoder must be symmetric with the encoder for major type 6: the
    /// encoder writes `CborTag` (e.g. the CSIL-RPC envelope's `#6.24(bstr)`), so the
    /// decoder needs a `case 6` that reconstructs it, or any tagged payload throws
    /// "unexpected major type" on decode.
    #[test]
    fn codec_decoder_handles_tag_major_type() {
        let files = generate_java(&input_for(corndogs_rules(), "java-client")).unwrap();
        let f = file(&files, "CsilCbor.java");
        assert!(
            f.content.contains(
                "case 6:\n                return new CborTag(arg, dec(b, pos, depth + 1));"
            ),
            "decoder missing `case 6` that reconstructs a CborTag"
        );
    }

    /// Generate the corndogs `java-client` spec, write a Driver with a loopback byte
    /// transport, compile every generated + driver source with `javac`, and run it,
    /// asserting a full typed round-trip. Skips cleanly when `javac` is absent.
    #[test]
    fn codec_round_trips_through_javac() {
        let have = std::process::Command::new("javac")
            .arg("-version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if !have {
            eprintln!("skipping: no javac on PATH");
            return;
        }

        let files = generate_java(&input_for(corndogs_rules(), "java-client")).unwrap();

        let dir = std::env::temp_dir().join(format!("csilgen-java-codec-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut sources: Vec<std::path::PathBuf> = Vec::new();
        for f in &files {
            let path = dir.join(&f.path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, &f.content).unwrap();
            sources.push(path);
        }
        let driver = dir.join("csilgen/generated/Driver.java");
        std::fs::write(&driver, JAVA_CODEC_DRIVER).unwrap();
        sources.push(driver);

        let classes = dir.join("classes");
        std::fs::create_dir_all(&classes).unwrap();
        let compile = std::process::Command::new("javac")
            .arg("-d")
            .arg(&classes)
            .args(&sources)
            .output()
            .unwrap();
        assert!(
            compile.status.success(),
            "javac failed:\n{}",
            String::from_utf8_lossy(&compile.stderr)
        );

        let run = std::process::Command::new("java")
            .arg("-cp")
            .arg(&classes)
            .arg("csilgen.generated.Driver")
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&run.stdout);
        let stderr = String::from_utf8_lossy(&run.stderr);
        assert!(
            run.status.success(),
            "java run failed:\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );
        assert_eq!(
            stdout.trim(),
            "ok",
            "unexpected output:\n{stdout}\n{stderr}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    const JAVA_CODEC_DRIVER: &str = r#"package csilgen.generated;

import java.util.List;
import java.util.Map;

public final class Driver {
    // A loopback "server" on the far side of the dumb byte seam: it decodes the typed
    // request, then encodes its task as the typed response, exercising both directions.
    static final class Loopback implements Transport {
        public byte[] call(String service, String op, byte[] req) throws ClientException {
            if (!service.equals("CorndogsService") || !op.equals("submit-task")) {
                throw new ClientException("unexpected route " + service + "/" + op);
            }
            SubmitTaskRequest in = CsilCbor.decodeSubmitTaskRequest(req);
            return CsilCbor.encodeTask(in.task());
        }
    }

    static void check(boolean cond, String msg) {
        if (!cond) {
            System.out.println("FAIL: " + msg);
            System.exit(1);
        }
    }

    public static void main(String[] args) throws Exception {
        byte[] payload = new byte[] {(byte) 0xde, (byte) 0xad, (byte) 0xbe};
        Task task = new Task("u-123", "PENDING", payload, 7L, Map.of("a", 1L, "b", 2L), List.of("x", "y"));
        SubmitTaskRequest req = new SubmitTaskRequest(task, "default");

        // Direct codec round-trip through the nested record.
        SubmitTaskRequest back = CsilCbor.decodeSubmitTaskRequest(CsilCbor.encodeSubmitTaskRequest(req));
        check(back.task().uuid().equals("u-123"), "uuid");
        check(back.task().currentState().equals("PENDING"), "current_state");
        check(java.util.Arrays.equals(back.task().payload(), payload), "payload");
        check(back.task().priority() != null && back.task().priority() == 7L, "priority");
        check(back.task().labels().size() == 2 && back.task().labels().get("a") == 1L && back.task().labels().get("b") == 2L, "labels");
        check(back.task().tags().size() == 2 && back.task().tags().get(0).equals("x") && back.task().tags().get(1).equals("y"), "tags");
        check(back.queue().equals("default"), "queue");

        // An absent optional must round-trip to null, not a zero value.
        Task noPrio = new Task("u-2", "S", new byte[] {1}, null, Map.of(), List.of());
        SubmitTaskRequest back2 = CsilCbor.decodeSubmitTaskRequest(CsilCbor.encodeSubmitTaskRequest(new SubmitTaskRequest(noPrio, "q")));
        check(back2.task().priority() == null, "absent optional null");

        // Typed client round-trip over the loopback carrier.
        CorndogsClient client = new CorndogsClient(new Loopback());
        Task resp = client.submitTask(req);
        check(resp.uuid().equals("u-123"), "client uuid");
        check(java.util.Arrays.equals(resp.payload(), payload), "client payload");
        check(resp.priority() != null && resp.priority() == 7L, "client priority");
        check(resp.tags().size() == 2 && resp.tags().get(1).equals("y"), "client tags");

        // Named map aliases must round-trip, not stub to empty: a scalar-valued map alias
        // and a map-of-record alias both reach through the generated wrapper records.
        Bag bag = new Bag(
            new StringInt64Map(Map.of("a", 1L, "b", 2L)),
            new ItemMap(Map.of("k", new Item("hello"))));
        Bag bagBack = CsilCbor.decodeBag(CsilCbor.encodeBag(bag));
        check(bagBack.counts().value().size() == 2
            && bagBack.counts().value().get("a") == 1L
            && bagBack.counts().value().get("b") == 2L, "named map alias entries");
        check(bagBack.items().value().size() == 1
            && bagBack.items().value().get("k").name().equals("hello"), "map-of-record alias entries");

        // A tagged value (e.g. the CSIL-RPC envelope's tag 24 #6.24(bstr)) must survive a
        // generic round-trip: the decoder grew a `case 6` mirroring the encoder's tag branch,
        // so encode->decode reconstructs the tag number and its inner payload.
        byte[] tagPayload = new byte[] {(byte) 0xca, (byte) 0xfe, (byte) 0x01};
        CsilCbor.CborValue tagBack = CsilCbor.decode(
            CsilCbor.encode(new CsilCbor.CborTag(24, new CsilCbor.CborBytes(tagPayload))));
        check(tagBack instanceof CsilCbor.CborTag, "tag major decoded");
        CsilCbor.CborTag rt = (CsilCbor.CborTag) tagBack;
        check(rt.num() == 24, "tag number");
        check(rt.inner() instanceof CsilCbor.CborBytes
            && java.util.Arrays.equals(((CsilCbor.CborBytes) rt.inner()).value(), tagPayload),
            "tag inner bytes");

        System.out.println("ok");
    }
}
"#;

    /// Compatibility guard: an all-literal choice (`Color = "red" / "green" /
    /// "blue"`, matching `Color` in tests/interop/interop.csil) must keep encoding as
    /// the bare literal, never the tagged sum — the union fix must not touch this
    /// shape at all.
    #[test]
    fn all_literal_choice_codec_stays_bare_literal() {
        let choices = vec![
            CsilTypeExpression::Literal(CsilLiteralValue::Text("red".to_string())),
            CsilTypeExpression::Literal(CsilLiteralValue::Text("green".to_string())),
            CsilTypeExpression::Literal(CsilLiteralValue::Text("blue".to_string())),
        ];
        let out = emit_choice_codec(
            "Color",
            &choices,
            &std::collections::HashSet::new(),
            &std::collections::HashMap::new(),
            &std::collections::HashMap::new(),
        );
        assert!(out.contains(
            "static CborValue encColor(Color v) {\n        return new CborText((String) v.value());\n    }"
        ));
        // Decode reads the bare literal AND validates it against the declared set —
        // an arbitrary string must not silently become a `Color` (see
        // `enum_decode_rejects_out_of_set_value` for the compiled, run proof).
        assert!(
            out.contains("var csilVal = asText(csilRoot);"),
            "got:\n{out}"
        );
        assert!(
            out.contains("java.util.Objects.equals(csilVal, \"red\")")
                && out.contains("java.util.Objects.equals(csilVal, \"green\")")
                && out.contains("java.util.Objects.equals(csilVal, \"blue\")"),
            "got:\n{out}"
        );
        assert!(
            out.contains("throw new CsilCborException(\"csil cbor: Color value \" + csilVal + \" is not a member of the declared enum\");"),
            "got:\n{out}"
        );
        assert!(out.contains("return new Color(csilVal);"), "got:\n{out}");
        assert!(!out.contains("CborArray"));
    }

    /// Compatibility guard: a genuine (non-mixed) union with no literal arms must
    /// still dispatch one `instanceof`-guarded case per arm, in declaration order —
    /// the type-grouping added to support literal arms in a mixed union must not
    /// perturb a union that has none.
    #[test]
    fn non_literal_union_dispatches_each_arm_by_its_own_type() {
        let choices = vec![
            CsilTypeExpression::Reference("Task".to_string()),
            CsilTypeExpression::Reference("ServiceError".to_string()),
        ];
        let mut records = std::collections::HashSet::new();
        records.insert("Task".to_string());
        records.insert("ServiceError".to_string());
        let out = emit_choice_codec(
            "Result",
            &choices,
            &records,
            &std::collections::HashMap::new(),
            &std::collections::HashMap::new(),
        );
        assert!(out.contains("if (csilInner instanceof Task csilCast0) {"));
        assert!(out.contains(
            "return new CborArray(java.util.Arrays.asList(new CborUint(0L), encTask(csilCast0)));"
        ));
        assert!(out.contains("if (csilInner instanceof ServiceError csilCast1) {"));
        assert!(out.contains(
            "return new CborArray(java.util.Arrays.asList(new CborUint(1L), encServiceError(csilCast1)));"
        ));
        assert!(out.contains(
            "if (csilIdx == 0L) {\n            return new Result(decTask(csilPayload));\n        }"
        ));
        assert!(out.contains(
            "if (csilIdx == 1L) {\n            return new Result(decServiceError(csilPayload));\n        }"
        ));
    }

    /// A spec with a mixed-union type-choice (`OrderStatus = text / "pending" /
    /// "confirmed" / "cancelled"`), matching `OrderStatus` in
    /// examples/real-world-api/e-commerce-api.csil, referenced by a minimal `Order`
    /// record so `generate_codec` emits the choice's own `enc`/`dec` pair.
    fn mixed_union_rules() -> Vec<CsilRule> {
        let status = rule(
            "OrderStatus",
            CsilRuleType::TypeDef(CsilTypeExpression::Choice(vec![
                builtin("text"),
                CsilTypeExpression::Literal(CsilLiteralValue::Text("pending".to_string())),
                CsilTypeExpression::Literal(CsilLiteralValue::Text("confirmed".to_string())),
                CsilTypeExpression::Literal(CsilLiteralValue::Text("cancelled".to_string())),
            ])),
        );
        let order = rule(
            "Order",
            CsilRuleType::GroupDef(CsilGroupExpression {
                entries: vec![bare(
                    "status",
                    CsilTypeExpression::Reference("OrderStatus".to_string()),
                    None,
                )],
            }),
        );
        vec![status, order]
    }

    /// Regression test for the general-arm-shadows-literals bug: before the fix,
    /// `OrderStatus` (a single-non-literal-arm choice) collapsed to a transparent
    /// `String` wrapper and encoded/decoded the bare literal, which is
    /// wire-incompatible with the locked union contract every other generator
    /// (go/python/rust/c/csharp/kotlin) already implements for this shape. Confirms
    /// literal-first indices, the general-arm fallback for a non-literal string, that
    /// every declared index decodes (literal arms included), and that a literal arm's
    /// payload is still validated on decode rather than trusted from the index alone.
    #[test]
    fn mixed_union_encode_prefers_literal_over_general_arm() {
        let have = std::process::Command::new("javac")
            .arg("-version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if !have {
            eprintln!("skipping: no javac on PATH");
            return;
        }

        let files = generate_java(&input_for(mixed_union_rules(), "java-client")).unwrap();

        let dir =
            std::env::temp_dir().join(format!("csilgen-java-mixedunion-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut sources: Vec<std::path::PathBuf> = Vec::new();
        for f in &files {
            let path = dir.join(&f.path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, &f.content).unwrap();
            sources.push(path);
        }
        let driver = dir.join("csilgen/generated/MixedUnionDriver.java");
        std::fs::write(&driver, JAVA_MIXED_UNION_DRIVER).unwrap();
        sources.push(driver);

        let classes = dir.join("classes");
        std::fs::create_dir_all(&classes).unwrap();
        let compile = std::process::Command::new("javac")
            .arg("-d")
            .arg(&classes)
            .args(&sources)
            .output()
            .unwrap();
        assert!(
            compile.status.success(),
            "javac failed:\n{}",
            String::from_utf8_lossy(&compile.stderr)
        );

        let run = std::process::Command::new("java")
            .arg("-cp")
            .arg(&classes)
            .arg("csilgen.generated.MixedUnionDriver")
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&run.stdout);
        let stderr = String::from_utf8_lossy(&run.stderr);
        assert!(
            run.status.success(),
            "java run failed:\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );
        assert_eq!(
            stdout.trim(),
            "ok",
            "unexpected output:\n{stdout}\n{stderr}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    const JAVA_MIXED_UNION_DRIVER: &str = r#"package csilgen.generated;

public final class MixedUnionDriver {
    static void check(boolean cond, String msg) {
        if (!cond) {
            System.out.println("FAIL: " + msg);
            System.exit(1);
        }
    }

    public static void main(String[] args) throws Exception {
        // A declared literal must win its own index over the general `text` arm,
        // even though both dispatch on the same Java `String`.
        CsilCbor.CborValue pendingCbor = CsilCbor.encOrderStatus(new OrderStatus("pending"));
        check(pendingCbor instanceof CsilCbor.CborArray, "pending is an array");
        java.util.List<CsilCbor.CborValue> pendingArr = ((CsilCbor.CborArray) pendingCbor).items();
        check(((CsilCbor.CborUint) pendingArr.get(0)).value() == 1L, "pending index 1");
        check(((CsilCbor.CborText) pendingArr.get(1)).value().equals("pending"), "pending payload");

        java.util.List<CsilCbor.CborValue> confirmedArr =
            ((CsilCbor.CborArray) CsilCbor.encOrderStatus(new OrderStatus("confirmed"))).items();
        check(((CsilCbor.CborUint) confirmedArr.get(0)).value() == 2L, "confirmed index 2");

        java.util.List<CsilCbor.CborValue> cancelledArr =
            ((CsilCbor.CborArray) CsilCbor.encOrderStatus(new OrderStatus("cancelled"))).items();
        check(((CsilCbor.CborUint) cancelledArr.get(0)).value() == 3L, "cancelled index 3");

        // A string matching no literal falls back to the general arm, index 0.
        java.util.List<CsilCbor.CborValue> onHoldArr =
            ((CsilCbor.CborArray) CsilCbor.encOrderStatus(new OrderStatus("on-hold"))).items();
        check(((CsilCbor.CborUint) onHoldArr.get(0)).value() == 0L, "on-hold index 0");
        check(((CsilCbor.CborText) onHoldArr.get(1)).value().equals("on-hold"), "on-hold payload");

        // Every declared index decodes back to its value, literal arms included.
        check(CsilCbor.decOrderStatus(CsilCbor.encOrderStatus(new OrderStatus("on-hold"))).value().equals("on-hold"), "decode index 0");
        check(CsilCbor.decOrderStatus(CsilCbor.encOrderStatus(new OrderStatus("pending"))).value().equals("pending"), "decode index 1");
        check(CsilCbor.decOrderStatus(CsilCbor.encOrderStatus(new OrderStatus("confirmed"))).value().equals("confirmed"), "decode index 2");
        check(CsilCbor.decOrderStatus(CsilCbor.encOrderStatus(new OrderStatus("cancelled"))).value().equals("cancelled"), "decode index 3");

        // A full round-trip through a record field.
        Order order = new Order(new OrderStatus("pending"));
        Order orderBack = CsilCbor.decodeOrder(CsilCbor.encodeOrder(order));
        check(orderBack.status().value().equals("pending"), "record field round-trip");

        // A literal arm still validates its payload rather than trusting the index: an
        // index that claims "pending" but carries a different string must be rejected.
        CsilCbor.CborValue bad = new CsilCbor.CborArray(java.util.Arrays.asList(
            new CsilCbor.CborUint(1L), new CsilCbor.CborText("confirmed")));
        boolean threw = false;
        try {
            CsilCbor.decOrderStatus(bad);
        } catch (CsilCbor.CsilCborException e) {
            threw = true;
        }
        check(threw, "literal mismatch rejected");

        // An out-of-range index is rejected too.
        CsilCbor.CborValue unknown = new CsilCbor.CborArray(java.util.Arrays.asList(
            new CsilCbor.CborUint(99L), new CsilCbor.CborText("pending")));
        boolean threw2 = false;
        try {
            CsilCbor.decOrderStatus(unknown);
        } catch (CsilCbor.CsilCborException e) {
            threw2 = true;
        }
        check(threw2, "unknown variant rejected");

        System.out.println("ok");
    }
}
"#;

    fn lit_text(s: &str) -> CsilTypeExpression {
        CsilTypeExpression::Literal(CsilLiteralValue::Text(s.to_string()))
    }

    /// A spec with a MIXED-KIND all-literal choice (`MixedKindStatus = "pending" / 42`,
    /// text and integer literals in the same closed enum), referenced by a minimal
    /// holder record so `generate_codec` emits the choice's own `enc`/`dec` pair through
    /// `emit_mixed_enum_codec`.
    fn mixed_kind_enum_rules() -> Vec<CsilRule> {
        let status = rule(
            "MixedKindStatus",
            CsilRuleType::TypeDef(CsilTypeExpression::Choice(vec![
                lit_text("pending"),
                CsilTypeExpression::Literal(CsilLiteralValue::Integer(42)),
            ])),
        );
        let holder = rule(
            "MixedKindHolder",
            CsilRuleType::GroupDef(CsilGroupExpression {
                entries: vec![bare(
                    "status",
                    CsilTypeExpression::Reference("MixedKindStatus".to_string()),
                    None,
                )],
            }),
        );
        vec![status, holder]
    }

    /// Regression test for defect (a): before the fix, `emit_choice_codec` derived the
    /// SOLE codec kind from the choice's FIRST literal arm (`enum_literal_kind`) and
    /// applied it to every member — encoding the `42` member of `"pending" / 42` cast
    /// `v.value()` to `String` (a `ClassCastException`), and decoding rejected a
    /// legitimately-encoded `42` as "not a member of the declared enum" because the
    /// membership check was filtered down to text-only literals. Confirms both kinds
    /// round-trip byte-identically through the named choice's own codec AND through a
    /// record field, and that an out-of-set value of EITHER kind is rejected on decode.
    #[test]
    fn mixed_kind_enum_round_trips_through_javac() {
        let have = std::process::Command::new("javac")
            .arg("-version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if !have {
            eprintln!("skipping: no javac on PATH");
            return;
        }

        let files = generate_java(&input_for(mixed_kind_enum_rules(), "java-client")).unwrap();

        let dir =
            std::env::temp_dir().join(format!("csilgen-java-mixedenum-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut sources: Vec<std::path::PathBuf> = Vec::new();
        for f in &files {
            let path = dir.join(&f.path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, &f.content).unwrap();
            sources.push(path);
        }
        let driver = dir.join("csilgen/generated/MixedKindEnumDriver.java");
        std::fs::write(&driver, JAVA_MIXED_KIND_ENUM_DRIVER).unwrap();
        sources.push(driver);

        let classes = dir.join("classes");
        std::fs::create_dir_all(&classes).unwrap();
        let compile = std::process::Command::new("javac")
            .arg("-d")
            .arg(&classes)
            .args(&sources)
            .output()
            .unwrap();
        assert!(
            compile.status.success(),
            "javac failed:\n{}",
            String::from_utf8_lossy(&compile.stderr)
        );

        let run = std::process::Command::new("java")
            .arg("-cp")
            .arg(&classes)
            .arg("csilgen.generated.MixedKindEnumDriver")
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&run.stdout);
        let stderr = String::from_utf8_lossy(&run.stderr);
        assert!(
            run.status.success(),
            "java run failed:\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );
        assert_eq!(
            stdout.trim(),
            "ok",
            "unexpected output:\n{stdout}\n{stderr}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    const JAVA_MIXED_KIND_ENUM_DRIVER: &str = r#"package csilgen.generated;

public final class MixedKindEnumDriver {
    static void check(boolean cond, String msg) {
        if (!cond) {
            System.out.println("FAIL: " + msg);
            System.exit(1);
        }
    }

    public static void main(String[] args) throws Exception {
        // The text member round-trips through the named choice's own codec.
        MixedKindStatus pending = new MixedKindStatus("pending");
        byte[] pendingBytes = CsilCbor.encode(CsilCbor.encMixedKindStatus(pending));
        MixedKindStatus pendingBack = CsilCbor.decMixedKindStatus(CsilCbor.decode(pendingBytes));
        check(pendingBack.value().equals("pending"), "text member round-trip");

        // The integer member round-trips too — before the fix this cast `v.value()` to
        // String on encode (ClassCastException) since "text" was the winning kind from
        // the first literal arm.
        MixedKindStatus fortyTwo = new MixedKindStatus(42L);
        byte[] intBytes = CsilCbor.encode(CsilCbor.encMixedKindStatus(fortyTwo));
        MixedKindStatus intBack = CsilCbor.decMixedKindStatus(CsilCbor.decode(intBytes));
        check(((Long) intBack.value()) == 42L, "int member round-trip");

        // Both kinds round-trip through a record field too.
        MixedKindHolder h1Back = CsilCbor.decodeMixedKindHolder(
            CsilCbor.encodeMixedKindHolder(new MixedKindHolder(pending)));
        check(h1Back.status().value().equals("pending"), "record field text round-trip");

        MixedKindHolder h2Back = CsilCbor.decodeMixedKindHolder(
            CsilCbor.encodeMixedKindHolder(new MixedKindHolder(fortyTwo)));
        check(((Long) h2Back.status().value()) == 42L, "record field int round-trip");

        // An out-of-set value of EITHER kind is rejected on decode — before the fix, a
        // legitimately out-of-vocabulary int like `7` (or, worse, an in-vocabulary-kind
        // int at all) was checked against a text-only-filtered membership list.
        boolean threwText = false;
        try {
            CsilCbor.decMixedKindStatus(new CsilCbor.CborText("nope"));
        } catch (CsilCbor.CsilCborException e) {
            threwText = true;
        }
        check(threwText, "out-of-set text value rejected");

        boolean threwInt = false;
        try {
            CsilCbor.decMixedKindStatus(new CsilCbor.CborInt(7L));
        } catch (CsilCbor.CsilCborException e) {
            threwInt = true;
        }
        check(threwInt, "out-of-set int value rejected");

        System.out.println("ok");
    }
}
"#;

    /// A choice arm carrying a trailing `.default` control operator, the shape CSIL's
    /// parser produces for the last arm of `a / b .default "b"`.
    fn default_arm(inner: CsilTypeExpression, def: &str) -> CsilTypeExpression {
        CsilTypeExpression::Constrained {
            base_type: Box::new(inner),
            constraints: vec![CsilControlOperator::Default(CsilLiteralValue::Text(
                def.to_string(),
            ))],
        }
    }

    fn tuple_entry(ty: CsilTypeExpression) -> CsilGroupEntry {
        CsilGroupEntry {
            key: None,
            value_type: ty,
            occurrence: None,
            metadata: vec![],
            doc_comments: vec![],
        }
    }

    /// The inline-choice torture record, matching the shared `inline-choice-torture.csil`:
    /// every position an inline group/choice can appear (direct field, optional field with
    /// a trailing `.default` arm, closed all-literal enum with a `.default` arm, mixed
    /// union, array element, map value, tuple element, inline group field).
    fn inline_choice_rules() -> Vec<CsilRule> {
        let payload = rule(
            "InlineChoicePayload",
            CsilRuleType::GroupDef(CsilGroupExpression {
                entries: vec![bare("detail", builtin("text"), None)],
            }),
        );
        let status = CsilTypeExpression::Choice(vec![
            builtin("text"),
            lit_text("pending"),
            lit_text("active"),
            lit_text("closed"),
        ]);
        let priority = CsilTypeExpression::Choice(vec![
            builtin("text"),
            lit_text("low"),
            lit_text("normal"),
            default_arm(lit_text("high"), "normal"),
        ]);
        let size = CsilTypeExpression::Choice(vec![
            lit_text("small"),
            lit_text("medium"),
            default_arm(lit_text("large"), "medium"),
        ]);
        let payload_choice = CsilTypeExpression::Choice(vec![
            lit_text("none"),
            CsilTypeExpression::Literal(CsilLiteralValue::Integer(42)),
            CsilTypeExpression::Reference("InlineChoicePayload".to_string()),
        ]);
        let tags = CsilTypeExpression::Array {
            element_type: Box::new(CsilTypeExpression::Choice(vec![
                builtin("text"),
                lit_text("red"),
                lit_text("green"),
                lit_text("blue"),
                builtin("int"),
            ])),
            occurrence: Some(CsilOccurrence::ZeroOrMore),
        };
        let labels = CsilTypeExpression::Map {
            key: Box::new(builtin("text")),
            value: Box::new(CsilTypeExpression::Choice(vec![
                builtin("text"),
                lit_text("yes"),
                lit_text("no"),
                builtin("bool"),
            ])),
            occurrence: Some(CsilOccurrence::ZeroOrMore),
        };
        let coord = CsilTypeExpression::Tuple(CsilGroupExpression {
            entries: vec![
                tuple_entry(builtin("int")),
                tuple_entry(CsilTypeExpression::Choice(vec![
                    builtin("text"),
                    lit_text("x"),
                    lit_text("y"),
                    lit_text("z"),
                ])),
            ],
        });
        let nested = CsilTypeExpression::Group(CsilGroupExpression {
            entries: vec![bare(
                "kind",
                CsilTypeExpression::Choice(vec![
                    builtin("text"),
                    lit_text("a"),
                    lit_text("b"),
                    builtin("int"),
                ]),
                None,
            )],
        });
        let record = rule(
            "InlineChoiceRecord",
            CsilRuleType::GroupDef(CsilGroupExpression {
                entries: vec![
                    bare("status", status, None),
                    bare("priority", priority, Some(CsilOccurrence::Optional)),
                    bare("size", size, Some(CsilOccurrence::Optional)),
                    bare("payload", payload_choice, None),
                    bare("tags", tags, None),
                    bare("labels", labels, None),
                    bare("coord", coord, None),
                    bare("nested", nested, None),
                ],
            }),
        );
        vec![payload, record]
    }

    /// Every inline group/choice position with at least one non-literal arm gets its own
    /// synthesized named type (and codec), and the owning record field routes through it
    /// — those anonymous-composite fields no longer collapse to `Object`/`CborNull`. A
    /// PURE all-literal choice (`size`, matching neither `codec_collapse_choice`'s
    /// narrowed-scalar shape nor a genuine union) is deliberately left un-hoisted
    /// (`HoistOptions::hoist_all_literal_choices: false` — Java already renders it inline
    /// via the generic `encEnumScalar`/`asEnumScalar`/`requireEnumMember` helpers, no
    /// synthesized wrapper class needed).
    #[test]
    fn inline_composites_are_hoisted_to_named_types() {
        let files = generate_java(&input_for(inline_choice_rules(), "java")).unwrap();
        let names: std::collections::HashSet<&str> = files
            .iter()
            .filter_map(|f| f.path.rsplit('/').next())
            .collect();
        for expected in [
            "InlineChoiceRecordStatus.java",
            "InlineChoiceRecordPriority.java",
            "InlineChoiceRecordPayload.java",
            "InlineChoiceRecordTagsItem.java",
            "InlineChoiceRecordLabelsValue.java",
            "InlineChoiceRecordCoord1.java",
            "InlineChoiceRecordNested.java",
            "InlineChoiceRecordNestedKind.java",
        ] {
            assert!(
                names.contains(expected),
                "missing synthesized file {expected}"
            );
        }
        assert!(
            !names.contains("InlineChoiceRecordSize.java"),
            "a pure all-literal choice must stay un-hoisted"
        );

        let record = files
            .iter()
            .find(|f| f.path.ends_with("InlineChoiceRecord.java"))
            .unwrap();
        assert!(record.content.contains("InlineChoiceRecordStatus status"));
        assert!(
            record
                .content
                .contains("List<InlineChoiceRecordTagsItem> tags")
        );
        assert!(
            record
                .content
                .contains("Map<String, InlineChoiceRecordLabelsValue> labels")
        );
        assert!(record.content.contains("InlineChoiceRecordNested nested"));
        // The un-hoisted pure-literal choice keeps the field position's `Object` type.
        assert!(record.content.contains("Object size"));

        let codec = files
            .iter()
            .find(|f| f.path.ends_with("CsilCbor.java"))
            .unwrap();
        // The record field routes through the synthesized codec, not a `CborNull` stub.
        assert!(
            codec
                .content
                .contains("encInlineChoiceRecordStatus(v.status())")
        );
        assert!(
            codec.content.contains(
                "encArray(v.tags(), csilElem0 -> encInlineChoiceRecordTagsItem(csilElem0))"
            )
        );
        assert!(codec.content.contains("encInlineChoiceRecordNestedKind"));
        // The un-hoisted `size` field routes through the generic enum-scalar helpers
        // instead of a synthesized `encInlineChoiceRecordSize`/`decInlineChoiceRecordSize`
        // pair — and still validates membership, not a `CborNull`/`null` stub.
        assert!(codec.content.contains("encEnumScalar(v.size())"));
        assert!(
            codec
                .content
                .contains("requireEnumMember(asEnumScalar(csilField)")
        );
        assert!(
            codec
                .content
                .contains("Objects.equals(csilEnumScalar0, \"small\")")
                && codec
                    .content
                    .contains("Objects.equals(csilEnumScalar0, \"medium\")")
                && codec
                    .content
                    .contains("Objects.equals(csilEnumScalar0, \"large\")")
        );
        assert!(!codec.content.contains("encInlineChoiceRecord(InlineChoiceRecord v) {\n        List<CborEntry> csilEntries = new ArrayList<>(8);\n        if (v.size() != null) {\n            csilEntries.add(new CborEntry(new CborText(\"size\"), new CborText(\"large\")));"));
    }

    /// The Constrained-arm classification fix: a closed all-literal choice whose last arm
    /// carries a trailing `.default` (parsed as `Constrained { Literal, .. }`) stays a
    /// bare-literal enum — it must NOT flip into the tagged-sum union shape.
    #[test]
    fn default_suffixed_literal_arm_stays_closed_enum() {
        let choices = vec![
            lit_text("small"),
            lit_text("medium"),
            default_arm(lit_text("large"), "medium"),
        ];
        let out = emit_choice_codec(
            "Size",
            &choices,
            &std::collections::HashSet::new(),
            &std::collections::HashMap::new(),
            &std::collections::HashMap::new(),
        );
        assert!(out.contains(
            "static CborValue encSize(Size v) {\n        return new CborText((String) v.value());\n    }"
        ));
        assert!(
            !out.contains("CborArray"),
            "closed enum must not become a tagged sum"
        );
    }

    /// A mixed union whose lone non-literal arm's Java type the literal arms do NOT share
    /// (`"none" / 42 / Record`) keeps an `Object` wrapper and dispatches each arm by its
    /// own runtime type — the single-non-literal scalar collapse must not swallow the
    /// literal arms into the record's type.
    #[test]
    fn mixed_java_type_union_uses_object_dispatch() {
        let choices = vec![
            lit_text("none"),
            CsilTypeExpression::Literal(CsilLiteralValue::Integer(42)),
            CsilTypeExpression::Reference("InlineChoicePayload".to_string()),
        ];
        let mut records = std::collections::HashSet::new();
        records.insert("InlineChoicePayload".to_string());
        let out = emit_choice_codec(
            "Payload",
            &choices,
            &records,
            &std::collections::HashMap::new(),
            &std::collections::HashMap::new(),
        );
        assert!(out.contains("Object csilInner = v.value();"));
        assert!(out.contains("csilInner instanceof String csilCast0 && "));
        assert!(out.contains("csilInner instanceof Long csilCast1 && "));
        assert!(out.contains("csilInner instanceof InlineChoicePayload csilCast2"));
        assert!(out.contains(
            "if (csilIdx == 2L) {\n            return new Payload(decInlineChoicePayload(csilPayload));\n        }"
        ));
    }

    /// Regression test for defect (b): in a dispatch group of arms sharing one Java
    /// runtime type, `general_idx` used to be overwritten on every iteration of the
    /// group's arms (last-wins) instead of only when still unset (first-wins). Two
    /// general (non-literal) `text`-typed arms sharing the dispatch type `String` force
    /// the multi-arm branch of `emit_object_union_encode`; declaration order requires the
    /// FIRST (index 0) to be the one whose payload/index the encoder actually returns —
    /// the second is unreachable dead code once the first already matches every `String`
    /// value, so keeping it instead would non-deterministically change which arm's index
    /// callers observe on the wire.
    #[test]
    fn union_with_duplicate_dispatch_type_arms_prefers_first_declared_index() {
        let choices = vec![
            builtin("text"),
            builtin("text"),
            CsilTypeExpression::Reference("Marker".to_string()),
        ];
        let mut records = std::collections::HashSet::new();
        records.insert("Marker".to_string());
        let out = emit_choice_codec(
            "DupArms",
            &choices,
            &records,
            &std::collections::HashMap::new(),
            &std::collections::HashMap::new(),
        );
        assert!(
            out.contains(
                "if (csilInner instanceof String csilCast0) {\n            return new CborArray(java.util.Arrays.asList(new CborUint(0L), new CborText(csilCast0)));\n        }"
            ),
            "the first general arm of the shared-type group must win, got:\n{out}"
        );
        assert!(
            !out.contains("new CborUint(1L), new CborText"),
            "the second same-type arm must not be reachable, got:\n{out}"
        );
    }

    /// Mirrors `crates/csilgen-common/src/hoist.rs`'s
    /// `case_insensitive_collision_between_existing_and_synthesized_rule_is_disambiguated`:
    /// an existing `UserData` rule and a `User` rule whose `data` field is an inline
    /// MIXED choice (so it hoists regardless of `hoist_all_literal_choices`) referencing
    /// `UserData` — the synthesized name (`User_data`, owner `User` + field `data`)
    /// pascal-collides with the existing `UserData` rule.
    fn case_insensitive_collision_rules() -> Vec<CsilRule> {
        let user_data = rule(
            "UserData",
            CsilRuleType::GroupDef(CsilGroupExpression {
                entries: vec![bare("value", builtin("text"), None)],
            }),
        );
        let user = rule(
            "User",
            CsilRuleType::GroupDef(CsilGroupExpression {
                entries: vec![bare(
                    "data",
                    CsilTypeExpression::Choice(vec![
                        lit_text("x"),
                        CsilTypeExpression::Reference("UserData".to_string()),
                    ]),
                    None,
                )],
            }),
        );
        vec![user_data, user]
    }

    /// Migration-proof: this asserts the shared hoist pass's case-insensitive name
    /// reservation is actually wired through `generate_java` (via
    /// `csilgen_common::hoist_inline_composites`/`HoistOptions`), not merely that the
    /// crate compiles against the new signature. Before the shared-module migration, a
    /// per-generator hoist that only checked collisions against the RAW (not
    /// case-normalized) name could emit two Java files that both compile to the class
    /// name `UserData` — a duplicate, non-compiling declaration.
    #[test]
    fn case_insensitive_collision_is_disambiguated_in_generated_java() {
        let files = generate_java(&input_for(case_insensitive_collision_rules(), "java")).unwrap();
        let class_names: Vec<String> = files
            .iter()
            .filter_map(|f| f.path.rsplit('/').next())
            .filter(|n| n.ends_with(".java"))
            .map(|n| n.trim_end_matches(".java").to_string())
            .collect();
        // The original `UserData` rule survives unchanged.
        assert!(
            class_names.iter().any(|n| n == "UserData"),
            "got: {class_names:?}"
        );
        // The synthesized rule must NOT be the raw "User_data" spelling verbatim (its
        // PascalCase form collides with `UserData`) — it must be disambiguated instead.
        assert!(
            !class_names.iter().any(|n| n == "User_data"),
            "synthesized name collided with UserData but was not disambiguated: {class_names:?}"
        );
        // No two emitted Java source files declare the same PascalCase class name.
        let mut canonical: Vec<String> = class_names
            .iter()
            .map(|n| n.to_case(Case::Pascal))
            .collect();
        let before = canonical.len();
        canonical.sort();
        canonical.dedup();
        assert_eq!(
            canonical.len(),
            before,
            "two emitted classes share a PascalCase name: {class_names:?}"
        );
    }

    /// End-to-end: generate the torture record, compile it, and confirm the encoded bytes
    /// for every inline-choice position match the canonical wire (byte cross-checked
    /// against the OCaml generator for the record-field positions, and against the
    /// `[variant_index, value]` / bare-literal contract for the array/map/tuple positions),
    /// plus a full-record round-trip. Skips gracefully when javac is unavailable.
    #[test]
    fn inline_choice_torture_roundtrips_and_matches_wire() {
        let have = std::process::Command::new("javac")
            .arg("-version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if !have {
            eprintln!("skipping: no javac on PATH");
            return;
        }

        let files = generate_java(&input_for(inline_choice_rules(), "java")).unwrap();
        let dir =
            std::env::temp_dir().join(format!("csilgen-java-inlinechoice-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut sources: Vec<std::path::PathBuf> = Vec::new();
        for f in &files {
            let path = dir.join(&f.path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, &f.content).unwrap();
            sources.push(path);
        }
        let driver = dir.join("csilgen/generated/InlineChoiceDriver.java");
        std::fs::write(&driver, JAVA_INLINE_CHOICE_DRIVER).unwrap();
        sources.push(driver);

        let classes = dir.join("classes");
        std::fs::create_dir_all(&classes).unwrap();
        let compile = std::process::Command::new("javac")
            .arg("-d")
            .arg(&classes)
            .args(&sources)
            .output()
            .unwrap();
        assert!(
            compile.status.success(),
            "javac failed:\n{}",
            String::from_utf8_lossy(&compile.stderr)
        );
        let run = std::process::Command::new("java")
            .arg("-cp")
            .arg(&classes)
            .arg("csilgen.generated.InlineChoiceDriver")
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&run.stdout);
        let stderr = String::from_utf8_lossy(&run.stderr);
        assert!(
            run.status.success() && stdout.trim() == "ok",
            "java run failed:\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    const JAVA_INLINE_CHOICE_DRIVER: &str = r#"package csilgen.generated;

public final class InlineChoiceDriver {
    static String hex(byte[] b) {
        StringBuilder sb = new StringBuilder();
        for (byte x : b) sb.append(String.format("%02x", x & 0xff));
        return sb.toString();
    }

    static void eq(String label, byte[] bytes, String expect) {
        String got = hex(bytes);
        if (!got.equals(expect)) {
            System.out.println("FAIL " + label + " = " + got + " expected " + expect);
            System.exit(1);
        }
    }

    public static void main(String[] args) {
        // Record-field positions: byte-identical to the OCaml oracle.
        eq("status.pending", CsilCbor.encode(CsilCbor.encInlineChoiceRecordStatus(new InlineChoiceRecordStatus("pending"))), "82016770656e64696e67");
        eq("status.free", CsilCbor.encode(CsilCbor.encInlineChoiceRecordStatus(new InlineChoiceRecordStatus("free"))), "82006466726565");
        eq("priority.high", CsilCbor.encode(CsilCbor.encInlineChoiceRecordPriority(new InlineChoiceRecordPriority("high"))), "82036468696768");
        // `size` is a PURE all-literal choice (no non-literal arm), left un-hoisted
        // (`HoistOptions::hoist_all_literal_choices: false`): no synthesized
        // `InlineChoiceRecordSize` wrapper/codec exists, so its wire shape is exercised
        // directly through the generic `encEnumScalar` helper instead.
        eq("size.medium", CsilCbor.encode(CsilCbor.encEnumScalar("medium")), "666d656469756d");
        eq("payload.none", CsilCbor.encode(CsilCbor.encInlineChoiceRecordPayload(new InlineChoiceRecordPayload("none"))), "8200646e6f6e65");
        eq("payload.inline", CsilCbor.encode(CsilCbor.encInlineChoiceRecordPayload(new InlineChoiceRecordPayload(new InlineChoicePayload("hi")))), "8202a16664657461696c626869");
        eq("nested.a", CsilCbor.encode(CsilCbor.encInlineChoiceRecordNestedKind(new InlineChoiceRecordNestedKind("a"))), "82016161");
        eq("nested.free", CsilCbor.encode(CsilCbor.encInlineChoiceRecordNestedKind(new InlineChoiceRecordNestedKind("free"))), "82006466726565");
        eq("nested.int7", CsilCbor.encode(CsilCbor.encInlineChoiceRecordNestedKind(new InlineChoiceRecordNestedKind(7L))), "820307");

        // Array/map/tuple positions: the [variant_index, value] / bare-literal contract.
        eq("tags.red", CsilCbor.encode(CsilCbor.encInlineChoiceRecordTagsItem(new InlineChoiceRecordTagsItem("red"))), "820163726564");
        eq("tags.int7", CsilCbor.encode(CsilCbor.encInlineChoiceRecordTagsItem(new InlineChoiceRecordTagsItem(7L))), "820407");
        eq("labels.yes", CsilCbor.encode(CsilCbor.encInlineChoiceRecordLabelsValue(new InlineChoiceRecordLabelsValue("yes"))), "820163796573");
        eq("labels.true", CsilCbor.encode(CsilCbor.encInlineChoiceRecordLabelsValue(new InlineChoiceRecordLabelsValue(Boolean.TRUE))), "8203f5");
        eq("coord.x", CsilCbor.encode(CsilCbor.encInlineChoiceRecordCoord1(new InlineChoiceRecordCoord1("x"))), "82016178");

        // Full-record round-trip through the composed record codec.
        InlineChoiceRecord rec = new InlineChoiceRecord(
            new InlineChoiceRecordStatus("pending"),
            new InlineChoiceRecordPriority("high"),
            "medium",
            new InlineChoiceRecordPayload(new InlineChoicePayload("hi")),
            java.util.List.of(new InlineChoiceRecordTagsItem("red"), new InlineChoiceRecordTagsItem(7L)),
            java.util.Map.of("k", new InlineChoiceRecordLabelsValue("yes")),
            java.util.List.of(42L, new InlineChoiceRecordCoord1("x")),
            new InlineChoiceRecordNested(new InlineChoiceRecordNestedKind(7L)));
        InlineChoiceRecord back = CsilCbor.decodeInlineChoiceRecord(CsilCbor.encodeInlineChoiceRecord(rec));
        boolean rt = back.status().value().equals("pending")
            && back.priority().value().equals("high")
            && back.size().equals("medium")
            && ((InlineChoicePayload) back.payload().value()).detail().equals("hi")
            && back.tags().get(0).value().equals("red")
            && ((Long) back.tags().get(1).value()) == 7L
            && back.labels().get("k").value().equals("yes")
            && back.coord().get(0).equals(42L)
            && ((InlineChoiceRecordCoord1) back.coord().get(1)).value().equals("x")
            && ((Long) back.nested().kind().value()) == 7L;
        if (!rt) { System.out.println("FAIL round-trip"); System.exit(1); }

        // A union arm's payload is validated against its declared literal on decode: an
        // index claiming "pending" that carries a different string must be rejected.
        boolean threw2 = false;
        try {
            CsilCbor.decInlineChoiceRecordStatus(new CsilCbor.CborArray(java.util.Arrays.asList(
                new CsilCbor.CborUint(1L), new CsilCbor.CborText("nope"))));
        } catch (CsilCbor.CsilCborException e) { threw2 = true; }
        if (!threw2) { System.out.println("FAIL literal validation"); System.exit(1); }

        // A closed all-literal enum (`size: "small" / "medium" / "large"`) left un-hoisted
        // is a plain `Object`-typed field position, not a native Java enum or a synthesized
        // wrapper class — decode must still reject a well-typed but undeclared value rather
        // than let it through, so tamper with an otherwise-valid record's encoded `size`
        // entry and confirm the full-record decode rejects it.
        java.util.List<CsilCbor.CborEntry> csilTampered = new java.util.ArrayList<>(
            ((CsilCbor.CborMap) CsilCbor.decode(CsilCbor.encodeInlineChoiceRecord(rec))).entries());
        for (int i = 0; i < csilTampered.size(); i++) {
            CsilCbor.CborEntry e = csilTampered.get(i);
            if (e.key() instanceof CsilCbor.CborText k && k.value().equals("size")) {
                csilTampered.set(i, new CsilCbor.CborEntry(e.key(), new CsilCbor.CborText("huge")));
            }
        }
        boolean threw3 = false;
        try {
            CsilCbor.decodeInlineChoiceRecord(CsilCbor.encode(new CsilCbor.CborMap(csilTampered)));
        } catch (CsilCbor.CsilCborException e) { threw3 = true; }
        if (!threw3) { System.out.println("FAIL enum membership validation (size)"); System.exit(1); }
        // Every declared member still decodes through the full record.
        InlineChoiceRecord small = CsilCbor.decodeInlineChoiceRecord(CsilCbor.encodeInlineChoiceRecord(
            new InlineChoiceRecord(rec.status(), rec.priority(), "small", rec.payload(), rec.tags(), rec.labels(), rec.coord(), rec.nested())));
        if (!small.size().equals("small")) {
            System.out.println("FAIL valid enum member decode (size)"); System.exit(1);
        }

        System.out.println("ok");
    }
}
"#;

    /// Build a package-mode-capable input from the corndogs spec, overlaying the given
    /// option key/values onto the config so each test sets only the coordinates it cares
    /// about.
    fn package_input(target: &str, opts: &[(&str, serde_json::Value)]) -> WasmGeneratorInput {
        let mut input = input_for(corndogs_rules(), target);
        for (k, v) in opts {
            input.config.options.insert((*k).to_string(), v.clone());
        }
        input
    }

    #[test]
    fn pom_emitted_only_when_emit_packages_includes_java() {
        // No trigger: the default flat layout, no pom, sources directly under the package.
        let plain = generate_java(&input_for(corndogs_rules(), "java-client")).unwrap();
        assert!(!plain.iter().any(|f| f.path == "pom.xml"));
        assert!(
            plain
                .iter()
                .any(|f| f.path == "csilgen/generated/Task.java")
        );

        // emit_packages that does not name java is inert (another language was requested).
        let other = generate_java(&package_input(
            "java-client",
            &[("emit_packages", serde_json::json!(["go", "rust"]))],
        ))
        .unwrap();
        assert!(!other.iter().any(|f| f.path == "pom.xml"));

        // With "java": a pom with the resolved coordinates, and sources relaid under
        // Maven's standard src/main/java/<package path> root with no flat-layout twin.
        let pkg = generate_java(&package_input(
            "java-client",
            &[
                ("emit_packages", serde_json::json!(["java"])),
                ("java_package", serde_json::json!("community.catalyst.demo")),
                ("package_name", serde_json::json!("corndogs-client")),
                ("package_version", serde_json::json!("1.2.3")),
            ],
        ))
        .unwrap();
        let pom = pkg
            .iter()
            .find(|f| f.path == "pom.xml")
            .expect("pom.xml emitted");
        assert!(
            pom.content
                .contains("<groupId>community.catalyst.demo</groupId>")
        );
        assert!(
            pom.content
                .contains("<artifactId>corndogs-client</artifactId>")
        );
        assert!(pom.content.contains("<version>1.2.3</version>"));
        assert!(
            pom.content
                .contains("<maven.compiler.release>17</maven.compiler.release>")
        );
        assert!(
            pkg.iter()
                .any(|f| f.path == "src/main/java/community/catalyst/demo/Task.java")
        );
        assert!(
            !pkg.iter()
                .any(|f| f.path == "community/catalyst/demo/Task.java")
        );
    }

    #[test]
    fn package_coordinates_default_and_parse_defensively() {
        // Absent package_name/version: artifactId is the kebab of the package's last
        // segment and version falls back to the conventional first release.
        let pkg = generate_java(&package_input(
            "java-typesonly",
            &[
                ("emit_packages", serde_json::json!(["java"])),
                ("java_package", serde_json::json!("com.example.widgetApi")),
            ],
        ))
        .unwrap();
        let pom = pkg.iter().find(|f| f.path == "pom.xml").unwrap();
        assert!(
            pom.content
                .contains("<groupId>com.example.widgetApi</groupId>")
        );
        assert!(pom.content.contains("<artifactId>widget-api</artifactId>"));
        assert!(pom.content.contains("<version>0.1.0</version>"));

        // emit_packages handed in as a JSON-encoded string still triggers.
        let as_string = generate_java(&package_input(
            "java-typesonly",
            &[("emit_packages", serde_json::json!("[\"java\"]"))],
        ))
        .unwrap();
        assert!(as_string.iter().any(|f| f.path == "pom.xml"));

        // A bare comma-separated string is tolerated as well.
        let csv = generate_java(&package_input(
            "java-typesonly",
            &[("emit_packages", serde_json::json!("go, java"))],
        ))
        .unwrap();
        assert!(csv.iter().any(|f| f.path == "pom.xml"));
    }

    /// Generate a `java-client` package into a temp dir, assert the pom.xml is well-formed
    /// XML (parsed by the JDK's own parser, no third-party dep), and compile ALL laid-out
    /// `src/main/java/**.java` with `javac` to prove the sources are a coherent, compilable
    /// package. Maven itself is never invoked (offline it would need a populated local
    /// repo); javac + XML-validity is the proof. Skips cleanly when `javac` is absent.
    #[test]
    fn package_pom_is_well_formed_xml_and_sources_compile() {
        let have = std::process::Command::new("javac")
            .arg("-version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if !have {
            eprintln!("skipping: no javac on PATH");
            return;
        }

        let files = generate_java(&package_input(
            "java-client",
            &[
                ("emit_packages", serde_json::json!(["java"])),
                ("java_package", serde_json::json!("community.catalyst.demo")),
                ("package_name", serde_json::json!("corndogs-client")),
                ("package_version", serde_json::json!("0.1.0")),
            ],
        ))
        .unwrap();

        let dir = std::env::temp_dir().join(format!("csilgen-java-pkg-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut sources: Vec<std::path::PathBuf> = Vec::new();
        let mut pom_path = None;
        for f in &files {
            let path = dir.join(&f.path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, &f.content).unwrap();
            if f.path == "pom.xml" {
                pom_path = Some(path);
            } else if f.path.starts_with("src/main/java/") && f.path.ends_with(".java") {
                sources.push(path);
            }
        }
        let pom_path = pom_path.expect("pom.xml present in package mode");

        let classes = dir.join("classes");
        std::fs::create_dir_all(&classes).unwrap();

        // Prove the pom is well-formed XML through the JDK's own DOM parser: a malformed
        // document throws on parse and the validator exits non-zero.
        let validator = dir.join("PomCheck.java");
        std::fs::write(&validator, JAVA_POM_VALIDATOR).unwrap();
        let vcompile = std::process::Command::new("javac")
            .arg("-d")
            .arg(&classes)
            .arg(&validator)
            .output()
            .unwrap();
        assert!(
            vcompile.status.success(),
            "javac PomCheck failed:\n{}",
            String::from_utf8_lossy(&vcompile.stderr)
        );
        let vrun = std::process::Command::new("java")
            .arg("-cp")
            .arg(&classes)
            .arg("PomCheck")
            .arg(&pom_path)
            .output()
            .unwrap();
        assert!(
            vrun.status.success(),
            "pom.xml is not well-formed XML:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&vrun.stdout),
            String::from_utf8_lossy(&vrun.stderr)
        );

        // Compile every laid-out source: a clean compile proves the package's sources form
        // a coherent, publishable unit under the standard Maven layout.
        assert!(
            !sources.is_empty(),
            "no src/main/java sources were laid out"
        );
        let compile = std::process::Command::new("javac")
            .arg("-d")
            .arg(&classes)
            .args(&sources)
            .output()
            .unwrap();
        assert!(
            compile.status.success(),
            "javac failed on laid-out package sources:\n{}",
            String::from_utf8_lossy(&compile.stderr)
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    const JAVA_POM_VALIDATOR: &str = r#"import javax.xml.parsers.DocumentBuilderFactory;
import java.io.File;

public final class PomCheck {
    public static void main(String[] args) throws Exception {
        DocumentBuilderFactory.newInstance().newDocumentBuilder().parse(new File(args[0]));
        System.out.println("ok");
    }
}
"#;

    /// The package README only rides along in package mode (`emit_packages` names java).
    #[test]
    fn readme_emitted_only_in_package_mode() {
        let plain = generate_java(&input_for(corndogs_rules(), "java-client")).unwrap();
        assert!(!plain.iter().any(|f| f.path == "genquickstart.md"));

        let pkg = generate_java(&package_input(
            "java-client",
            &[("emit_packages", serde_json::json!(["java"]))],
        ))
        .unwrap();
        assert!(pkg.iter().any(|f| f.path == "genquickstart.md"));
    }

    /// `emit_readme: false` suppresses only the README; the rest of the package (notably the
    /// pom and the laid-out sources) is unchanged.
    #[test]
    fn emit_readme_false_suppresses_only_readme() {
        let on = generate_java(&package_input(
            "java-client",
            &[("emit_packages", serde_json::json!(["java"]))],
        ))
        .unwrap();
        assert!(on.iter().any(|f| f.path == "genquickstart.md"));

        let off = generate_java(&package_input(
            "java-client",
            &[
                ("emit_packages", serde_json::json!(["java"])),
                ("emit_readme", serde_json::json!(false)),
            ],
        ))
        .unwrap();
        assert!(!off.iter().any(|f| f.path == "genquickstart.md"));
        // Everything other than the README is still emitted.
        assert!(off.iter().any(|f| f.path == "pom.xml"));
        let on_without_readme: Vec<_> = on
            .iter()
            .filter(|f| f.path != "genquickstart.md")
            .map(|f| &f.path)
            .collect();
        let off_paths: Vec<_> = off.iter().map(|f| &f.path).collect();
        assert_eq!(on_without_readme, off_paths);
    }
    // --- 3-transport genquickstart (lib-based) ----------------------------

    /// A reference type expression for the verification spec helpers.
    fn reference(name: &str) -> CsilTypeExpression {
        CsilTypeExpression::Reference(name.to_string())
    }

    /// The verification spec: a `->` op (`ping: Ping -> Ping`, record request and response
    /// so the datagram codec round-trips) and a record-typed `<->` op (`chat: ChatMsg <->
    /// ChatReply`, both records so the channel router exists).
    fn demo_rules() -> Vec<CsilRule> {
        let ping = rule(
            "Ping",
            CsilRuleType::GroupDef(CsilGroupExpression {
                entries: vec![
                    bare("message", builtin("text"), None),
                    bare("nonce", builtin("int"), None),
                ],
            }),
        );
        let chat_msg = rule(
            "ChatMsg",
            CsilRuleType::GroupDef(CsilGroupExpression {
                entries: vec![bare("body", builtin("text"), None)],
            }),
        );
        let chat_reply = rule(
            "ChatReply",
            CsilRuleType::GroupDef(CsilGroupExpression {
                entries: vec![bare("ok", builtin("bool"), None)],
            }),
        );
        let svc = CsilServiceDefinition {
            operations: vec![
                op(
                    "ping",
                    reference("Ping"),
                    reference("Ping"),
                    CsilServiceDirection::Unidirectional,
                    None,
                ),
                op(
                    "chat",
                    reference("ChatMsg"),
                    reference("ChatReply"),
                    CsilServiceDirection::Bidirectional,
                    Some(2),
                ),
            ],
            wire_id: Some(1),
        };
        vec![
            ping,
            chat_msg,
            chat_reply,
            rule("DemoService", CsilRuleType::ServiceDef(svc)),
        ]
    }

    fn demo_input_pkg(target: &str) -> WasmGeneratorInput {
        package_input_with(
            target,
            demo_rules(),
            &[
                ("emit_packages", serde_json::json!(["java"])),
                ("java_package", serde_json::json!("community.catalyst.demo")),
                ("package_name", serde_json::json!("demo-client")),
            ],
        )
    }

    /// Like `package_input`, but over a caller-supplied rule set rather than the corndogs
    /// fixture, so a test can shape a minimal spec and still set package options.
    fn package_input_with(
        target: &str,
        rules: Vec<CsilRule>,
        opts: &[(&str, serde_json::Value)],
    ) -> WasmGeneratorInput {
        let mut input = input_for(rules, target);
        for (k, v) in opts {
            input.config.options.insert((*k).to_string(), v.clone());
        }
        input
    }

    #[test]
    fn client_readme_has_rpc_and_datagram_sections() {
        let files = generate_java(&demo_input_pkg("java-client")).unwrap();
        let c = &file(&files, "genquickstart.md").content;

        // All three headings render in a fixed order.
        assert!(c.contains("## CSIL-RPC (HTTP)"), "rpc heading");
        assert!(c.contains("## CSIL-Events (TLS)"), "events heading");
        assert!(c.contains("## CSIL-Datagrams (UDP)"), "datagrams heading");

        // Install names the transport library.
        assert!(
            c.contains("<artifactId>csilgen-transport</artifactId>"),
            "install adds the transport lib"
        );

        // RPC: the carrier implements the generated seam and builds the envelope with the
        // LIBRARY's Rpc.Request/Rpc.Response (never hand-rolled), POSTing to the path.
        assert!(
            c.contains("final class HttpRpcCarrier implements Transport"),
            "carrier implements the seam"
        );
        assert!(
            c.contains("Rpc.Request.of(service, op, req == null ? new byte[0] : req).encode()"),
            "request envelope via the lib"
        );
        assert!(
            c.contains("Rpc.Response.decode(resp.body())"),
            "response decode via the lib"
        );
        assert!(c.contains("/csil/v1/rpc"), "posts to the path");
        assert!(
            c.contains("\"ServiceError\".equals(decoded.variant())"),
            "typed ServiceError arm handled"
        );
        assert!(
            c.contains("import community.catalyst.csilgen.transport.Rpc;"),
            "imports the transport lib"
        );
        // Example constructs the typed client and calls the first `->` op with a sample.
        assert!(
            c.contains("DemoClient client = new DemoClient(new HttpRpcCarrier("),
            "example constructs the typed client"
        );
        assert!(
            c.contains("Ping resp = client.ping(new Ping(\"example\", 0L));"),
            "example calls the first unary op with a sample"
        );

        // Datagrams: encode via the generated codec, wrap in the lib Datagram, note the
        // no-synchronous-response semantics.
        assert!(
            c.contains("Datagrams.Datagram.of(OP_ORD, 0, CsilCbor.encodePing(request)).encode()"),
            "datagram send via lib + generated codec"
        );
        assert!(
            c.contains("CsilCbor.decodePing(dg.payload())"),
            "inbound payload decoded into the response type"
        );
        assert!(
            c.contains("MAY arrive later — or never"),
            "datagram loss/late note"
        );

        // A package carries both surfaces, so even when generated from `java-client` the
        // Events section dispatches into the generated router (the package ships it).
        assert!(
            c.contains(
                "DemoServiceRouter.routeDemoServiceChannel(handlers, codec, ev.event(), ev.payload())"
            ),
            "events dispatches to the generated router in package mode"
        );
        assert!(
            c.contains(
                "new Events.Hello(List.of(1L), List.of(\"verbose\"), \"DemoService\", null)"
            ),
            "events handshake names the service"
        );
    }

    #[test]
    fn server_readme_events_dispatches_to_the_generated_router() {
        let files = generate_java(&demo_input_pkg("java")).unwrap();
        let c = &file(&files, "genquickstart.md").content;

        // A server surface emits the channel router, so Events dispatches into it.
        assert!(
            c.contains("static final class Handlers implements DemoService"),
            "events handler implements the generated interface"
        );
        assert!(
            c.contains(
                "DemoServiceRouter.routeDemoServiceChannel(handlers, codec, ev.event(), ev.payload())"
            ),
            "events dispatches to the generated router"
        );
        assert!(
            c.contains(
                "new Events.Hello(List.of(1L), List.of(\"verbose\"), \"DemoService\", null)"
            ),
            "events handshake names the service"
        );
        assert!(
            c.contains("Events.PING_NAME") && c.contains("Events.PONG_NAME"),
            "events answers the $ping/$pong heartbeat"
        );
        assert!(
            c.contains(
                "Events.Event.verbose(\"DemoService\", \"chat\", CsilCbor.encodeChatMsg(outbound))"
            ),
            "events sends one outbound event via the generated codec"
        );

        // A package carries both surfaces, so even when generated from the base `java`
        // (server) target the RPC section constructs the typed client (the package ships it).
        assert!(
            c.contains("DemoClient client = new DemoClient(new HttpRpcCarrier("),
            "rpc constructs the typed client in package mode"
        );
        assert!(
            c.contains("Ping resp = client.ping(new Ping(\"example\", 0L));"),
            "rpc calls the first unary op with a sample"
        );
    }

    #[test]
    fn genquickstart_transports_selects_a_subset() {
        // Only "datagrams" requested: the other two sections are suppressed.
        let files = generate_java(&package_input_with(
            "java-client",
            demo_rules(),
            &[
                ("emit_packages", serde_json::json!(["java"])),
                ("java_package", serde_json::json!("community.catalyst.demo")),
                ("genquickstart_transports", serde_json::json!(["datagrams"])),
            ],
        ))
        .unwrap();
        let c = &file(&files, "genquickstart.md").content;
        assert!(c.contains("## CSIL-Datagrams (UDP)"), "datagrams kept");
        assert!(!c.contains("## CSIL-RPC (HTTP)"), "rpc suppressed");
        assert!(!c.contains("## CSIL-Events (TLS)"), "events suppressed");

        // An array naming no known transport falls back to all three.
        let all = generate_java(&package_input_with(
            "java-client",
            demo_rules(),
            &[
                ("emit_packages", serde_json::json!(["java"])),
                ("java_package", serde_json::json!("community.catalyst.demo")),
                ("genquickstart_transports", serde_json::json!(["bogus"])),
            ],
        ))
        .unwrap();
        let c = &file(&all, "genquickstart.md").content;
        assert!(
            c.contains("## CSIL-RPC (HTTP)")
                && c.contains("## CSIL-Events (TLS)")
                && c.contains("## CSIL-Datagrams (UDP)"),
            "unknown-only subset falls back to all three"
        );
    }

    #[test]
    fn serviceless_package_falls_back_to_notes() {
        let typed = generate_java(&package_input_with(
            "java-typesonly",
            vec![rule(
                "Money",
                CsilRuleType::GroupDef(CsilGroupExpression {
                    entries: vec![bare("amount", builtin("int"), None)],
                }),
            )],
            &[("emit_packages", serde_json::json!(["java"]))],
        ))
        .unwrap();
        let c = &file(&typed, "genquickstart.md").content;
        // No services: every section degrades to a note, none emits a live carrier.
        assert!(c.contains("no `->` operations"), "rpc/datagram notes");
        assert!(
            !c.contains("implements Transport"),
            "no carrier without a service"
        );
    }

    /// Collect every `*.java` under `dir`, recursively.
    fn collect_java(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                collect_java(&path, out);
            } else if path.extension().and_then(|e| e.to_str()) == Some("java") {
                out.push(path);
            }
        }
    }

    /// The reference Java transport library's `src/main/java` sources, compiled alongside
    /// the generated package so the Quickstart sections resolve against the real library.
    fn transport_java_sources() -> Vec<std::path::PathBuf> {
        let root = format!(
            "{}/../../transports/java/src/main/java",
            env!("CARGO_MANIFEST_DIR")
        );
        let mut out = Vec::new();
        collect_java(std::path::Path::new(&root), &mut out);
        out
    }

    /// The body of the first ```java block under `heading`, bounded to that section so a
    /// note-only section (no code block) returns `None` rather than the next section's.
    fn extract_java_block_after(readme: &str, heading: &str) -> Option<String> {
        let h = readme.find(heading)?;
        let rest = &readme[h + heading.len()..];
        let section = &rest[..rest.find("\n## ").unwrap_or(rest.len())];
        const FENCE: &str = "```java\n";
        let start = section.find(FENCE)? + FENCE.len();
        let after = &section[start..];
        let end = after.find("\n```")?;
        Some(after[..end].to_string())
    }

    /// Compile-check the shipped 3-transport Quickstart against the SINGLE package a user
    /// publishes (the default `java` target) + the reference transport library with `javac`
    /// (JDK17). All three sections — RPC + Datagrams (typed client) and Events (generated
    /// router) — must resolve against that one package, proving package mode is
    /// self-contained. No socket is opened; javac proves the carriers and the lib/codec API
    /// names compose. Skips cleanly when `javac` is absent.
    #[test]
    fn readme_three_transports_compile() {
        let have = std::process::Command::new("javac")
            .arg("-version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if !have {
            eprintln!("skipping: no javac on PATH");
            return;
        }
        let lib_sources = transport_java_sources();
        assert!(!lib_sources.is_empty(), "transport lib sources found");

        // The `package community.catalyst.demo;` source root the example files share.
        let src_root = "src/main/java/community/catalyst/demo";

        // Compile a generated package plus the named example blocks (extracted by heading)
        // against the transport library. Returns the javac output on failure.
        let compile_surface =
            |tag: &str, target: &str, blocks: &[(&str, &str)]| -> Result<(), String> {
                let files = generate_java(&demo_input_pkg(target)).unwrap();
                let dir = std::env::temp_dir()
                    .join(format!("csilgen-java-3t-{tag}-{}", std::process::id()));
                let _ = std::fs::remove_dir_all(&dir);
                let mut sources: Vec<std::path::PathBuf> = lib_sources.clone();
                let mut readme = String::new();
                for f in &files {
                    if f.path == "genquickstart.md" {
                        readme = f.content.clone();
                        continue;
                    }
                    let path = dir.join(&f.path);
                    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                    std::fs::write(&path, &f.content).unwrap();
                    if f.path.starts_with("src/main/java/") && f.path.ends_with(".java") {
                        sources.push(path);
                    }
                }
                for (heading, class) in blocks {
                    let block = extract_java_block_after(&readme, heading)
                        .unwrap_or_else(|| panic!("{tag}: missing {heading} java block"));
                    let path = dir.join(format!("{src_root}/{class}.java"));
                    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                    std::fs::write(&path, block).unwrap();
                    sources.push(path);
                }
                let classes = dir.join("classes");
                std::fs::create_dir_all(&classes).unwrap();
                let compile = std::process::Command::new("javac")
                    .arg("-d")
                    .arg(&classes)
                    .args(&sources)
                    .output()
                    .unwrap();
                let result = if compile.status.success() {
                    Ok(())
                } else {
                    Err(String::from_utf8_lossy(&compile.stderr).to_string())
                };
                let _ = std::fs::remove_dir_all(&dir);
                result
            };

        // The single default `java` package: RPC + Events + Datagrams are ALL live and ALL
        // resolve against the one package (typed client + generated router + codec together).
        compile_surface(
            "single",
            "java",
            &[
                ("## CSIL-RPC (HTTP)", "RpcExample"),
                ("## CSIL-Events (TLS)", "EventsExample"),
                ("## CSIL-Datagrams (UDP)", "DatagramsExample"),
            ],
        )
        .unwrap_or_else(|e| panic!("single-package javac failed:\n{e}"));
    }
}
