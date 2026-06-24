// CSIL-Events transport — typed bidirectional event streams — see
// csil-events-transport.md. Verbose (text-keyed) and compact (positional) profiles,
// plus the control plane (service ordinal 0) lifecycle events.

/// Which wire profile a connection uses for its lifetime, fixed by hello.
public enum Profile: Sendable {
    case verbose
    case compact

    public var wireName: String {
        switch self {
        case .verbose: return "verbose"
        case .compact: return "compact"
        }
    }

    public static func parse(_ s: String) -> Profile? {
        switch s {
        case "verbose": return .verbose
        case "compact": return .compact
        default: return nil
        }
    }
}

/// One typed event flowing in either direction. It is identified by service+operation
/// (verbose) or by their ordinals (compact), and carries an optional correlation id
/// when it is a request expecting a reply, or that reply.
public struct Event: Equatable, Sendable {
    /// The CSIL service name (verbose). nil on a single-service verbose connection.
    public var service: String?
    /// The service ordinal (compact). Always present in compact frames.
    public var serviceOrd: UInt64?
    /// The CSIL operation name (verbose).
    public var event: String?
    /// The operation ordinal (compact).
    public var opOrd: UInt64?
    public var id: UInt64?
    public var payload: [UInt8]

    public init(
        service: String? = nil, serviceOrd: UInt64? = nil, event: String? = nil,
        opOrd: UInt64? = nil, id: UInt64? = nil, payload: [UInt8]
    ) {
        self.service = service
        self.serviceOrd = serviceOrd
        self.event = event
        self.opOrd = opOrd
        self.id = id
        self.payload = payload
    }

    /// A verbose event by name.
    public static func verbose(service: String?, event: String, payload: [UInt8]) -> Event {
        Event(service: service, event: event, payload: payload)
    }

    /// A compact event by ordinals.
    public static func compact(serviceOrd: UInt64, opOrd: UInt64, payload: [UInt8]) -> Event {
        Event(serviceOrd: serviceOrd, opOrd: opOrd, payload: payload)
    }

    public func withID(_ id: UInt64) -> Event {
        var copy = self
        copy.id = id
        return copy
    }

    public func encode(_ profile: Profile) throws -> [UInt8] {
        switch profile {
        case .verbose: return try encodeVerbose()
        case .compact: return try encodeCompact()
        }
    }

    private func encodeVerbose() throws -> [UInt8] {
        guard let event else { throw malformed("verbose event missing 'event' name") }
        var entries: [CBOREntry] = [
            CBOREntry(key: .text("event"), value: .text(event)),
            CBOREntry(key: .text("payload"), value: tag24(payload)),
        ]
        if let service { entries.append(CBOREntry(key: .text("service"), value: .text(service))) }
        if let id { entries.append(CBOREntry(key: .text("id"), value: .uint(id))) }
        return encodeValue(canonMap(entries))
    }

    private func encodeCompact() throws -> [UInt8] {
        guard let serviceOrd else { throw malformed("compact event missing service ordinal") }
        guard let opOrd else { throw malformed("compact event missing op ordinal") }
        var arr: [CBORValue] = [.uint(serviceOrd), .uint(opOrd)]
        if let id { arr.append(.uint(id)) }
        arr.append(tag24(payload))
        return encodeValue(.array(arr))
    }

    public static func decode(_ b: [UInt8], _ profile: Profile) throws -> Event {
        switch profile {
        case .verbose: return try decodeVerbose(b)
        case .compact: return try decodeCompact(b)
        }
    }

    private static func decodeVerbose(_ b: [UInt8]) throws -> Event {
        let v = try decodeEnvelope(b)
        guard let p = mapGet(v, "payload") else { throw malformed("missing 'payload'") }
        return Event(
            service: getTextOpt(v, "service"),
            event: try getText(v, "event"),
            id: getUintOpt(v, "id"),
            payload: try untag24(p)
        )
    }

    private static func decodeCompact(_ b: [UInt8]) throws -> Event {
        let v = try decodeEnvelope(b)
        guard case .array(let arr) = v else { throw malformed("compact event is not an array") }
        // 3 elements => [service_ord, op_ord, payload]; 4 => with correlation id.
        let serviceOrdV: CBORValue
        let opOrdV: CBORValue
        let payloadV: CBORValue
        var idV: CBORValue? = nil
        switch arr.count {
        case 3:
            serviceOrdV = arr[0]
            opOrdV = arr[1]
            payloadV = arr[2]
        case 4:
            serviceOrdV = arr[0]
            opOrdV = arr[1]
            idV = arr[2]
            payloadV = arr[3]
        default:
            throw malformed("compact event array has \(arr.count) elements, expected 3 or 4")
        }
        guard let serviceOrd = asU64(serviceOrdV), let opOrd = asU64(opOrdV) else {
            throw malformed("ordinal is not an integer")
        }
        var event = Event(serviceOrd: serviceOrd, opOrd: opOrd, payload: try untag24(payloadV))
        if let idV {
            guard let id = asU64(idV) else { throw malformed("ordinal is not an integer") }
            event.id = id
        }
        return event
    }
}

/// Control-plane operation ordinals (under service ordinal 0).
public enum Control {
    public static let hello: UInt64 = 0
    public static let helloAck: UInt64 = 1
    public static let ping: UInt64 = 2
    public static let pong: UInt64 = 3
    public static let close: UInt64 = 4
    public static let error: UInt64 = 5

    // Verbose control-event names (the `$`-sigil names).
    public static let helloName = "$hello"
    public static let helloAckName = "$hello-ack"
    public static let pingName = "$ping"
    public static let pongName = "$pong"
    public static let closeName = "$close"
    public static let errorName = "$error"
}

/// The `$hello` payload offered by the connection initiator.
public struct Hello: Equatable, Sendable {
    public var versions: [UInt64]
    public var profiles: [String]
    public var service: String?
    public var auth: String?

    public init(versions: [UInt64], profiles: [String], service: String? = nil, auth: String? = nil)
    {
        self.versions = versions
        self.profiles = profiles
        self.service = service
        self.auth = auth
    }

    public func encode() -> [UInt8] {
        var entries: [CBOREntry] = [
            CBOREntry(key: .text("versions"), value: .array(versions.map { .uint($0) })),
            CBOREntry(key: .text("profiles"), value: .array(profiles.map { .text($0) })),
        ]
        if let service { entries.append(CBOREntry(key: .text("service"), value: .text(service))) }
        if let auth { entries.append(CBOREntry(key: .text("auth"), value: .text(auth))) }
        return encodeValue(canonMap(entries))
    }

    public static func decode(_ b: [UInt8]) throws -> Hello {
        let v = try decodeEnvelope(b)
        guard case .array(let versArr)? = mapGet(v, "versions") else {
            throw malformed("hello missing 'versions'")
        }
        guard case .array(let profArr)? = mapGet(v, "profiles") else {
            throw malformed("hello missing 'profiles'")
        }
        return Hello(
            versions: versArr.compactMap { asU64($0) },
            profiles: profArr.compactMap { if case .text(let t) = $0 { return t } else { return nil } },
            service: getTextOpt(v, "service"),
            auth: getTextOpt(v, "auth")
        )
    }

    /// Select a profile from this hello's offers, honoring the peer's preference order
    /// and what the receiver supports. Returns the chosen (version, profile), or nil if
    /// nothing is mutually supported.
    public func negotiate(supported: [Profile]) -> (version: UInt64, profile: Profile)? {
        guard versions.contains(csilVersion) else { return nil }
        for offered in profiles {
            guard let p = Profile.parse(offered) else { continue }
            if supported.contains(p) {
                return (csilVersion, p)
            }
        }
        return nil
    }
}

/// The `$hello-ack` payload returned by the peer.
public struct HelloAck: Equatable, Sendable {
    public var v: UInt64
    public var profile: String
    public var session: String?

    public init(v: UInt64, profile: String, session: String? = nil) {
        self.v = v
        self.profile = profile
        self.session = session
    }

    public func encode() -> [UInt8] {
        var entries: [CBOREntry] = [
            CBOREntry(key: .text("v"), value: .uint(v)),
            CBOREntry(key: .text("profile"), value: .text(profile)),
        ]
        if let session { entries.append(CBOREntry(key: .text("session"), value: .text(session))) }
        return encodeValue(canonMap(entries))
    }

    public static func decode(_ b: [UInt8]) throws -> HelloAck {
        let v = try decodeEnvelope(b)
        return HelloAck(
            v: try getUint(v, "v"),
            profile: try getText(v, "profile"),
            session: getTextOpt(v, "session")
        )
    }
}

/// A `$ping`/`$pong` heartbeat payload.
public struct Heartbeat: Equatable, Sendable {
    public var nonce: UInt64
    public var at: UInt64?

    public init(nonce: UInt64, at: UInt64? = nil) {
        self.nonce = nonce
        self.at = at
    }

    public func encode() -> [UInt8] {
        var entries: [CBOREntry] = [CBOREntry(key: .text("nonce"), value: .uint(nonce))]
        if let at { entries.append(CBOREntry(key: .text("at"), value: .uint(at))) }
        return encodeValue(canonMap(entries))
    }

    public static func decode(_ b: [UInt8]) throws -> Heartbeat {
        let v = try decodeEnvelope(b)
        return Heartbeat(nonce: try getUint(v, "nonce"), at: getUintOpt(v, "at"))
    }
}

/// A `$close` payload.
public struct Close: Equatable, Sendable {
    public var status: Status
    public var reason: String?

    public init(status: Status, reason: String? = nil) {
        self.status = status
        self.reason = reason
    }

    public func encode() -> [UInt8] {
        var entries: [CBOREntry] = [
            CBOREntry(key: .text("status"), value: .int(Int64(status.code)))
        ]
        if let reason { entries.append(CBOREntry(key: .text("reason"), value: .text(reason))) }
        return encodeValue(canonMap(entries))
    }

    public static func decode(_ b: [UInt8]) throws -> Close {
        let v = try decodeEnvelope(b)
        return Close(status: Status(code: Int(try getInt(v, "status"))), reason: getTextOpt(v, "reason"))
    }
}
