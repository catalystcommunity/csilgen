import Foundation

// The generated cddl_time is canonical UTC text, which keeps all nine digits.
let digits = 9

func unhex(_ s: String) -> [UInt8] {
    let chars = Array(s.utf8)
    return stride(from: 0, to: chars.count, by: 2).map {
        UInt8(String(decoding: chars[$0..<$0 + 2], as: UTF8.self), radix: 16)!
    }
}

func hex(_ b: [UInt8]) -> String {
    b.map { String(format: "%02x", $0) }.joined()
}

let text = try String(contentsOfFile: CommandLine.arguments[1], encoding: .utf8)
var failures: [String] = []
for line in text.split(separator: "\n") where !line.hasPrefix("#") {
    let f = line.split(separator: "\t", omittingEmptySubsequences: false).map(String.init)
    let (name, expect, applies, input, canonical) = (f[0], f[1], f[2], f[3], f[4])
    if applies != "*" && Int(applies)! != digits { continue }
    let wantOk = expect == "ok"
    do {
        let moment = try Moment.fromCbor(unhex(input))
        if !wantOk {
            failures.append("\(name): decode accepted \(moment.at)")
        } else {
            let encoded = hex(try moment.toCbor())
            if encoded != canonical { failures.append("\(name): encoded \(encoded)") }
        }
    } catch {
        if wantOk { failures.append("\(name): round trip failed: \(error)") }
    }
}
// Text that decode refuses must make encode throw, not stop the process.
for bad in ["2000-02-30T00:00:00Z", "0001-01-01T00:30:00+01:00", "10000-01-01T00:00:00Z", "not a time"] {
    do {
        _ = try Moment(at: bad).toCbor()
        failures.append("encode accepted \(bad)")
    } catch {}
}
failures.forEach { print($0) }
if !failures.isEmpty { exit(1) }
print("swift: all epoch vectors pass")
