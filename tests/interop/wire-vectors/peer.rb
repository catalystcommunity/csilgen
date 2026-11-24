# Decode each vector as the shape its kind names; re-encode the rows that must decode.
require_relative "codec"

def union_round_trip(bytes, decode, encode)
  CsilCbor.encode(CsilCbor.public_send(encode, CsilCbor.public_send(decode, CsilCbor.decode(bytes))))
end

ROUND_TRIP = {
  "holder" => ->(bytes) { Holder.from_cbor(bytes).to_cbor },
  "contact" => ->(bytes) { union_round_trip(bytes, :dec_union_Contact, :enc_union_Contact) },
  "output" => lambda { |bytes|
    union_round_trip(bytes, :dec_union_LookupReadCountsOutput, :enc_union_LookupReadCountsOutput)
  }
}.freeze

failures = 0
File.foreach(ARGV.fetch(0), encoding: "UTF-8") do |line|
  line = line.chomp
  next if line.empty? || line.start_with?("#")

  name, kind, expect, input_hex, canonical = line.split("\t")
  begin
    encoded = ROUND_TRIP.fetch(kind).call([input_hex].pack("H*")).unpack1("H*")
  rescue StandardError => e
    if expect == "ok"
      failures += 1
      puts "FAIL #{name}: decode failed: #{e.message}"
    end
    next
  end
  if expect != "ok"
    failures += 1
    puts "FAIL #{name}: decoded, but must fail (re-encoded #{encoded})"
  elsif encoded != canonical
    failures += 1
    puts "FAIL #{name}: re-encoded #{encoded}, want #{canonical}"
  end
end

if failures.positive?
  puts "#{failures} vector(s) failed"
  exit 1
end
puts "all wire vectors pass"
