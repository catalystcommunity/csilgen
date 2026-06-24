# frozen_string_literal: true

module Csilgen
  # Reference implementation of the CSIL transport family for Ruby: CSIL-RPC,
  # CSIL-Events, and CSIL-Datagrams. It owns the envelope codecs, framing, and
  # connection lifecycle; the byte/datagram carrier is injected (bring-your-own-carrier)
  # so a host plugs HTTP, WebSocket, QUIC, WebRTC, or a platform media stack without
  # changing this library.
  #
  # See the spec documents at the repo root: csil-transport-conventions.md,
  # csil-rpc-transport.md, csil-events-transport.md, csil-datagrams-transport.md. The
  # byte layout is pinned by the conformance vectors in transports/conformance/.
  module Transport
    # Base for every error the transport layer (the wire) surfaces, distinct from a
    # host's application errors (which ride inside the payload). Declared before the
    # submodules so each specialized error class -- and CBOR::CBORError -- can descend
    # from it at load time.
    class Error < StandardError; end
  end
end

require_relative "transport/version"
require_relative "transport/cbor"
require_relative "transport/conventions"
require_relative "transport/carrier"
require_relative "transport/rpc"
require_relative "transport/events"
require_relative "transport/datagrams"
