# Decode each vector as a `Moment`; re-encode the rows that must decode.
# A Ruby `Time` keeps an exact Rational fraction, so this target holds 9 digits.
require_relative "codec"

DIGITS = 9

checked = 0
failures = 0
File.foreach(ARGV.fetch(0), encoding: "UTF-8") do |line|
  line = line.chomp
  next if line.empty? || line.start_with?("#")

  name, expect, digits, input_hex, canonical = line.split("\t")
  next unless digits == "*" || Integer(digits, 10) == DIGITS

  checked += 1
  begin
    moment = Moment.from_cbor([input_hex].pack("H*"))
    raise TypeError, "decoded cddl_time is not a Time" unless moment.at.is_a?(Time)

    encoded = moment.to_cbor.unpack1("H*")
  rescue ArgumentError => e
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
  puts "#{failures} of #{checked} epoch vector(s) failed"
  exit 1
end
puts "all #{checked} epoch vectors pass"
