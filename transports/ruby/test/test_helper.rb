# frozen_string_literal: true

require "minitest/autorun"
require "json"

$LOAD_PATH.unshift File.expand_path("../lib", __dir__)
require "csilgen/transport"

# The byte-exact conformance vectors are shared across every language's reference
# library; locate them relative to this test directory so the suite runs from anywhere.
CONFORMANCE_DIR = File.expand_path("../../conformance", __dir__)

def load_vectors(name)
  JSON.parse(File.read(File.join(CONFORMANCE_DIR, name)))["vectors"]
end

def unhex(text)
  [text].pack("H*")
end

def to_hex(bytes)
  bytes.unpack1("H*")
end
