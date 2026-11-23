# Decode each vector as a `Stamp`; re-encode the rows that must decode.
# A Ruby `Time` keeps an exact Rational fraction, so this target holds 9 digits.
require_relative "codec"

DIGITS = 9

failures = 0
File.foreach(ARGV.fetch(0), encoding: "UTF-8") do |line|
  line = line.chomp
  next if line.empty? || line.start_with?("#")

  name, expect, precision, input_hex, canonical = line.split("\t")
  must_decode = expect == "ok" && Integer(precision, 10) <= DIGITS
  begin
    encoded = Stamp.from_cbor([input_hex].pack("H*")).to_cbor.unpack1("H*")
  rescue StandardError => e
    if must_decode
      failures += 1
      puts "FAIL #{name}: decode failed: #{e.message}"
    end
    next
  end
  if !must_decode
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
puts "all timestamp vectors pass"
