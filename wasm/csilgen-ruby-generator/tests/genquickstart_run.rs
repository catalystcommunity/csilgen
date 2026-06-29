//! Hermetic verification that the `genquickstart.md` Ruby carriers actually run.
//!
//! The genquickstart was authored without a Ruby toolchain, so its carrier code was
//! previously only string-asserted. This test generates the three-transport package for a
//! spec with a `->` op and a record-typed `<->` op, writes it next to the in-repo
//! `transports/ruby` reference lib, extracts the emitted code blocks, and *runs* them under
//! `ruby` with the network carrier swapped for an in-process echo:
//!
//! - CSIL-RPC: the verbatim emitted `HttpRpcTransport` POSTs to a loopback HTTP echo server
//!   that round-trips the `RpcRequest`/`RpcResponse` envelope, and the typed value is
//!   asserted to survive the generated codec.
//! - CSIL-Datagrams: the emitted body runs verbatim with `open_udp_carrier` swapped for an
//!   in-process echo datagram carrier; the typed value is asserted through the codec.
//! - CSIL-Events: the emitted session is run with the TLS carrier swapped for an in-process
//!   one that replays a scripted handshake + one typed event; the generated `<Service>Router`
//!   (which lives on the server surface) must resolve from the package and dispatch the
//!   decoded value into the handler, proving the client package is self-contained.
//!
//! The test resolves `ruby` via PATH and skips cleanly when it is absent, so it runs where a
//! toolchain is installed and is a no-op in CI images without Ruby.

mod common;

use common::*;
use csilgen_common::*;
use serde_json::json;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The verification spec: a unary `->` op and a record-typed `<->` op, with distinct
/// request/response records so the codec round-trip is meaningful (not an identity on the
/// same class).
fn verification_spec() -> CsilSpecSerialized {
    spec(vec![
        group_rule("ping", vec![bare_entry("nonce", builtin("int"))]),
        group_rule("pong", vec![bare_entry("nonce", builtin("int"))]),
        group_rule("tick", vec![bare_entry("seq", builtin("int"))]),
        group_rule("tock", vec![bare_entry("seq", builtin("int"))]),
        service_rule(
            "echo_service",
            vec![
                op_wire(
                    "do-ping",
                    reference("ping"),
                    reference("pong"),
                    CsilServiceDirection::Unidirectional,
                    7,
                ),
                op(
                    "stream-tick",
                    reference("tick"),
                    reference("tock"),
                    CsilServiceDirection::Bidirectional,
                ),
            ],
            None,
        ),
    ])
}

/// `ruby` on PATH, or `None` so the test skips on a toolchain-free image.
fn ruby_bin() -> Option<String> {
    Command::new("ruby")
        .arg("--version")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|_| "ruby".to_string())
}

/// Pull the first fenced ```ruby block that follows `heading` out of the markdown.
fn ruby_block(md: &str, heading: &str) -> String {
    let start = md
        .find(heading)
        .unwrap_or_else(|| panic!("genquickstart missing heading {heading}"));
    let after = &md[start..];
    let fence = after
        .find("```ruby")
        .unwrap_or_else(|| panic!("no ```ruby block under {heading}"));
    let body_start = start + fence + "```ruby".len();
    let rest = &md[body_start..];
    let end = rest
        .find("```")
        .unwrap_or_else(|| panic!("unterminated ```ruby block under {heading}"));
    rest[..end].trim_start_matches('\n').to_string()
}

/// The in-repo Ruby reference transport lib (`require "csilgen/transport"` resolves here).
fn transport_lib_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../transports/ruby/lib")
}

/// `$LOAD_PATH` shims so the harness can `require "csilgen/transport"` and the generated
/// package entry point without installing anything.
fn load_path_preamble(transport_lib: &Path, pkg_lib: &Path) -> String {
    format!(
        "$LOAD_PATH.unshift({:?})\n$LOAD_PATH.unshift({:?})\n",
        transport_lib.to_str().unwrap(),
        pkg_lib.to_str().unwrap(),
    )
}

/// Run a Ruby script string, returning (success, stdout, stderr).
fn run_ruby(ruby: &str, script: &Path, args: &[&str]) -> (bool, String, String) {
    let out = Command::new(ruby)
        .args(args)
        .arg(script)
        .output()
        .expect("spawn ruby");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn genquickstart_carriers_run_under_ruby() {
    let Some(ruby) = ruby_bin() else {
        eprintln!("skipping genquickstart_carriers_run_under_ruby: `ruby` not on PATH");
        return;
    };

    // Generate the three-transport package.
    let mut opts = HashMap::new();
    opts.insert("emit_packages".to_string(), json!(["ruby"]));
    opts.insert("package_name".to_string(), json!("echo_client"));
    opts.insert("package_version".to_string(), json!("0.1.0"));
    let cfg = GeneratorConfig {
        target: "ruby-client".to_string(),
        output_dir: "/tmp/echo".to_string(),
        options: opts,
    };
    let files = generate_ruby_code_from_serialized(&verification_spec(), &cfg)
        .expect("generation succeeded");

    // Lay the package out on disk; the entry point lives at lib/echo_client.rb.
    let root = std::env::temp_dir().join(format!("csilgen-ruby-gqs-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    for f in &files {
        let p = root.join(&f.path);
        std::fs::create_dir_all(p.parent().unwrap()).expect("mkdir");
        std::fs::write(&p, &f.content).expect("write package file");
    }
    let pkg_lib = root.join("lib");
    let transport_lib = transport_lib_dir();
    assert!(
        transport_lib.join("csilgen/transport.rb").exists(),
        "transport lib not found at {transport_lib:?}"
    );

    let md = files
        .iter()
        .find(|f| f.path == "genquickstart.md")
        .expect("genquickstart.md emitted")
        .content
        .clone();
    let preamble = load_path_preamble(&transport_lib, &pkg_lib);

    // --- CSIL-RPC: run the emitted HttpRpcTransport against a loopback HTTP echo server ----
    let rpc_block = ruby_block(&md, "## CSIL-RPC (HTTP)")
        // The example points at a fixed port; aim it at the in-process echo server instead.
        .replace("\"http://localhost:5080\"", "\"http://127.0.0.1:#{port}\"");
    let rpc_harness = format!(
        "{preamble}\
require \"socket\"\n\
require \"csilgen/transport\"\n\
# Loopback HTTP echo: decode the RpcRequest envelope and echo its payload as an ok\n\
# RpcResponse, so the emitted HttpRpcTransport carrier runs end to end with no network.\n\
RpcSrv = Csilgen::Transport::RPC\n\
server = TCPServer.new(\"127.0.0.1\", 0)\n\
port = server.addr[1]\n\
Thread.new do\n\
  loop do\n\
    conn = server.accept\n\
    len = 0\n\
    while (line = conn.gets)\n\
      break if line == \"\\r\\n\"\n\
      len = $1.to_i if line =~ /\\AContent-Length:\\s*(\\d+)/i\n\
    end\n\
    body = (len > 0) ? conn.read(len) : \"\".b\n\
    req = RpcSrv::RpcRequest.decode(body.b)\n\
    resp = RpcSrv::RpcResponse.ok(\"Pong\", req.payload).encode\n\
    conn.write(\"HTTP/1.1 200 OK\\r\\nContent-Length: #{{resp.bytesize}}\\r\\nConnection: close\\r\\n\\r\\n\")\n\
    conn.write(resp)\n\
    conn.close\n\
  end\n\
rescue IOError, Errno::EBADF\n\
  # Server socket closed when the script exited; nothing to do.\n\
end\n\n\
{rpc_block}\n\
raise \"RPC sample did not decode to Pong: #{{resp.inspect}}\" unless resp.is_a?(Pong)\n\
echoed = client.do_ping(Ping.new(nonce: 1234567))\n\
raise \"RPC round-trip mismatch: #{{echoed.inspect}}\" unless echoed == Pong.new(nonce: 1234567)\n\
puts \"RPC OK\"\n",
    );
    let rpc_path = root.join("rpc_harness.rb");
    std::fs::write(&rpc_path, &rpc_harness).expect("write rpc harness");
    let (ok, stdout, stderr) = run_ruby(&ruby, &rpc_path, &[]);
    assert!(
        ok && stdout.contains("RPC OK"),
        "CSIL-RPC carrier failed under ruby.\nstdout:\n{stdout}\nstderr:\n{stderr}\nscript:\n{rpc_harness}"
    );

    // --- CSIL-Datagrams: run the emitted body with an in-process echo datagram carrier -----
    let dg_block = ruby_block(&md, "## CSIL-Datagrams (UDP)")
        // Swap the UDP socket carrier for an in-process echo so the emitted Datagram envelope
        // body runs hermetically; carry a non-trivial nonce so a dropped field is caught.
        .replace(
            "def open_udp_carrier(host, port)\n  socket = UDPSocket.new\n  socket.connect(host, port)\n  Csilgen::Transport::UdpDatagramCarrier.new(socket)\nend",
            "def open_udp_carrier(host, port)\n  EchoDatagramCarrier.new\nend",
        )
        .replace("Ping.new(nonce: 0)", "Ping.new(nonce: 42)");
    assert!(
        dg_block.contains("EchoDatagramCarrier.new"),
        "datagram carrier swap did not match the emitted open_udp_carrier:\n{dg_block}"
    );
    let dg_harness = format!(
        "{preamble}\
# An in-process echo datagram carrier: a sent datagram loops straight back to recv.\n\
class EchoDatagramCarrier\n\
  def initialize = @q = []\n\
  def send_datagram(bytes) = @q << bytes.b\n\
  def recv_datagram = @q.shift\n\
end\n\n\
{dg_block}\n\
raise \"no datagram echoed\" if inbound.nil?\n\
got = Pong.from_cbor(Csilgen::Transport::Datagrams::Datagram.decode(inbound).payload)\n\
raise \"datagram round-trip mismatch: #{{got.inspect}}\" unless got == Pong.new(nonce: 42)\n\
puts \"DATAGRAMS OK\"\n",
    );
    let dg_path = root.join("datagrams_harness.rb");
    std::fs::write(&dg_path, &dg_harness).expect("write datagram harness");
    let (ok, stdout, stderr) = run_ruby(&ruby, &dg_path, &[]);
    assert!(
        ok && stdout.contains("DATAGRAMS OK"),
        "CSIL-Datagrams carrier failed under ruby.\nstdout:\n{stdout}\nstderr:\n{stderr}\nscript:\n{dg_harness}"
    );

    // --- CSIL-Events: drive a real session through the generated router -------------------
    // The Events section dispatches inbound typed events through the generated
    // `<Service>Router` (`encode_<op>` outbound + `route_channel` inbound), which lives on
    // the *server* surface. A `ruby-client` package must therefore still carry that surface
    // for its own quickstart to resolve. Rather than only load/syntax-check, swap the TLS
    // carrier for an in-process one and actually run `session`, so a missing router surface
    // (the NameError this whole pass exists to prevent) fails the test for real.
    let ev_block = ruby_block(&md, "## CSIL-Events (TLS)").replace(
        "def open_tls_carrier(host, port)\n  socket = TCPSocket.new(host, port)\n  ssl = OpenSSL::SSL::SSLSocket.new(socket)\n  ssl.connect\n  Csilgen::Transport::StreamCarrier.new(ssl)\nend",
        "def open_tls_carrier(host, port)\n  $carrier\nend",
    );
    assert!(
        ev_block.contains("def open_tls_carrier(host, port)\n  $carrier\nend"),
        "TLS carrier swap did not match the emitted open_tls_carrier:\n{ev_block}"
    );
    // The recv loop reads the `$hello-ack` first, then one typed `StreamTick` event, then
    // `nil` to end. `route_channel` must decode that event to a `Tick` and dispatch it into
    // the handler — which is what proves the generated router resolves from this package.
    let ev_harness = format!(
        "{preamble}{ev_block}\n\
# An in-process stream carrier: send_frame collects, recv_frame replays a scripted inbound\n\
# sequence so the emitted session drives the generated router with no socket.\n\
class InProcCarrier\n\
  def initialize(inbound) = (@inbound = inbound)\n\
  def send_frame(bytes) = nil\n\
  def recv_frame = @inbound.shift\n\
end\n\n\
inbound = [\n\
  Events::HelloAck.new(v: 1, profile: \"verbose\").encode,\n\
  Events::Event.verbose(\"echo\", \"StreamTick\", Tick.new(seq: 5).to_cbor).encode(Events::Profile::VERBOSE),\n\
]\n\
$carrier = InProcCarrier.new(inbound)\n\n\
# Override the one channel op so the dispatched, codec-decoded value is observable.\n\
class CaptureHandlers < EchoHandlers\n\
  def stream_tick(msg) = ($captured = msg)\n\
end\n\n\
session(CaptureHandlers.new, channel_codec)\n\
raise \"router did not dispatch StreamTick into the handler: #{{$captured.inspect}}\" unless $captured == Tick.new(seq: 5)\n\
puts \"EVENTS OK\"\n",
    );
    let ev_path = root.join("events_harness.rb");
    std::fs::write(&ev_path, &ev_harness).expect("write events harness");
    let (ok, stdout, stderr) = run_ruby(&ruby, &ev_path, &[]);
    assert!(
        ok && stdout.contains("EVENTS OK"),
        "CSIL-Events router dispatch failed under ruby.\nstdout:\n{stdout}\nstderr:\n{stderr}\nscript:\n{ev_harness}"
    );

    let _ = std::fs::remove_dir_all(&root);
}
