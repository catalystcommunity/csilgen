# frozen_string_literal: true

require_relative "lib/csilgen/transport/version"

Gem::Specification.new do |spec|
  # The gem name MUST equal what consumers put in `gem "..."`; the Bundler git/glob
  # source resolves the gem by this name (see README.md).
  spec.name = "csilgen-transport"
  spec.version = Csilgen::Transport::VERSION_STRING
  spec.authors = ["Catalyst Community"]
  spec.summary = "Reference CSIL transport family (RPC, Events, Datagrams) for Ruby."
  spec.description = "Hand-written, dependency-free Ruby implementation of the CSIL " \
                     "transport family: canonical CBOR codec plus matched client/server " \
                     "for CSIL-RPC, CSIL-Events, and CSIL-Datagrams over a " \
                     "bring-your-own-carrier seam."
  spec.homepage = "https://github.com/catalystcommunity/csilgen"
  spec.license = "Apache-2.0"

  # Data.define and frozen-by-default value objects require Ruby 3.2+.
  spec.required_ruby_version = ">= 3.2"

  spec.files = Dir["lib/**/*.rb"] + ["README.md"]
  spec.require_paths = ["lib"]

  # No runtime gem dependencies by design: the codec is hand-rolled and the tests use
  # only the `minitest` and `json` default gems that ship with every CRuby.

  # TODO: Publish to RubyGems.org for the convenience path. Until then, consumers pull
  # straight from this git repo via Bundler's git+glob source (see README.md).
  # Publishing would be: bump VERSION_STRING + a semver git tag, `gem build
  # csilgen-transport.gemspec`, `gem push csilgen-transport-<version>.gem`, and add the
  # gem owners. After that consumers could use a plain `gem "csilgen-transport", "~> x.y"`.
end
