import Foundation

func unhex(_ s: String) -> [UInt8] {
    let chars = Array(s.utf8)
    return stride(from: 0, to: chars.count, by: 2).map {
        UInt8(String(decoding: chars[$0..<$0 + 2], as: UTF8.self), radix: 16)!
    }
}

func hex(_ b: [UInt8]) -> String {
    b.map { String(format: "%02x", $0) }.joined()
}

// Decodes the input as the shape the kind names and re-encodes it.
func roundTrip(_ kind: String, _ input: [UInt8]) throws -> [UInt8] {
    switch kind {
    case "holder": return try Holder.fromCbor(input).toCbor()
    case "contact": return try Contact.fromCbor(input).toCbor()
    case "output": return try LookupReadCountsOutput.fromCbor(input).toCbor()
    default: fatalError("unknown kind \(kind)")
    }
}

let text = try String(contentsOfFile: CommandLine.arguments[1], encoding: .utf8)
var failures = 0
for line in text.split(separator: "\n") where !line.hasPrefix("#") {
    let f = line.split(separator: "\t", omittingEmptySubsequences: false).map(String.init)
    let (name, kind, expect, input, canonical) = (f[0], f[1], f[2], f[3], f[4])
    do {
        let encoded = hex(try roundTrip(kind, unhex(input)))
        if expect != "ok" {
            print("FAIL \(name): decoded, but must fail (re-encoded \(encoded))")
            failures += 1
        } else if encoded != canonical {
            print("FAIL \(name): re-encoded \(encoded), want \(canonical)")
            failures += 1
        }
    } catch {
        if expect == "ok" {
            print("FAIL \(name): decode failed: \(error)")
            failures += 1
        }
    }
}
if failures > 0 { exit(1) }
print("all wire vectors pass")
