// Loopback TCP and UDP sockets over the C socket API. The Swift transport library
// has no socket carrier of its own (the host owns I/O), and Foundation is not used,
// so the harness binds the BSD socket calls directly.

#if canImport(Glibc)
import Glibc
private let streamType = Int32(SOCK_STREAM.rawValue)
private let datagramType = Int32(SOCK_DGRAM.rawValue)
#else
import Darwin
private let streamType = SOCK_STREAM
private let datagramType = SOCK_DGRAM
#endif

import CsilgenTransport

struct SocketError: Error, CustomStringConvertible {
    let description: String

    init(_ call: String) {
        description = "\(call) failed: errno \(errno)"
    }
}

/// Writes to a peer that has closed raise SIGPIPE, which would end a server that
/// outlives one client, so the harness turns the signal off at start.
func ignoreBrokenPipe() {
    signal(SIGPIPE, SIG_IGN)
}

private func loopback(_ port: UInt16) -> sockaddr_in {
    var address = sockaddr_in()
    address.sin_family = sa_family_t(AF_INET)
    address.sin_port = port.bigEndian
    address.sin_addr = in_addr(s_addr: UInt32(0x7f00_0001).bigEndian)
    #if !canImport(Glibc)
    // BSD socket addresses carry their own length; Linux has no such field.
    address.sin_len = UInt8(MemoryLayout<sockaddr_in>.size)
    #endif
    return address
}

private func withSockaddr<T>(_ address: sockaddr_in, _ body: (UnsafePointer<sockaddr>, socklen_t) -> T) -> T {
    var copy = address
    return withUnsafePointer(to: &copy) {
        $0.withMemoryRebound(to: sockaddr.self, capacity: 1) {
            body($0, socklen_t(MemoryLayout<sockaddr_in>.size))
        }
    }
}

private func reuseAddress(_ fd: Int32) {
    var on: Int32 = 1
    setsockopt(fd, SOL_SOCKET, SO_REUSEADDR, &on, socklen_t(MemoryLayout<Int32>.size))
}

private func pause(milliseconds: Int) {
    usleep(useconds_t(milliseconds * 1000))
}

/// A connected TCP socket as the transport's `ByteStream`.
final class SocketStream: ByteStream {
    let fd: Int32

    init(fd: Int32) {
        self.fd = fd
    }

    func write(_ bytes: [UInt8]) throws {
        var sent = 0
        while sent < bytes.count {
            let n = bytes.withUnsafeBytes { raw in
                send(fd, raw.baseAddress! + sent, bytes.count - sent, 0)
            }
            if n < 0 {
                if errno == EINTR { continue }
                throw TransportError.carrier("send failed: errno \(errno)")
            }
            sent += n
        }
    }

    func readExactly(_ count: Int) throws -> [UInt8] {
        var out: [UInt8] = []
        var buffer = [UInt8](repeating: 0, count: 65536)
        while out.count < count {
            let want = min(count - out.count, buffer.count)
            let n = buffer.withUnsafeMutableBytes { recv(fd, $0.baseAddress, want, 0) }
            if n == 0 { break }
            if n < 0 {
                if errno == EINTR { continue }
                throw TransportError.carrier("recv failed: errno \(errno)")
            }
            out.append(contentsOf: buffer[0..<n])
        }
        return out
    }

    func close() {
        _ = closeFd(fd)
    }
}

/// A listening TCP socket on 127.0.0.1. Bind retries briefly so the port that the
/// previous server in the matrix just freed is accepted.
final class TcpListener {
    let fd: Int32

    init(port: UInt16) throws {
        fd = socket(AF_INET, streamType, 0)
        guard fd >= 0 else { throw SocketError("socket") }
        reuseAddress(fd)
        var bound = false
        for _ in 0..<200 {
            if withSockaddr(loopback(port), { bind(fd, $0, $1) }) == 0 {
                bound = true
                break
            }
            pause(milliseconds: 15)
        }
        guard bound else { throw SocketError("bind") }
        guard listen(fd, 16) == 0 else { throw SocketError("listen") }
    }

    func accept() -> SocketStream? {
        while true {
            #if canImport(Glibc)
            let peer = Glibc.accept(fd, nil, nil)
            #else
            let peer = Darwin.accept(fd, nil, nil)
            #endif
            if peer >= 0 { return SocketStream(fd: peer) }
            if errno == EINTR { continue }
            return nil
        }
    }
}

/// Connects to 127.0.0.1:port, retrying while the server starts.
func connectTcp(port: UInt16) throws -> SocketStream {
    for _ in 0..<200 {
        let fd = socket(AF_INET, streamType, 0)
        guard fd >= 0 else { throw SocketError("socket") }
        if withSockaddr(loopback(port), { connect(fd, $0, $1) }) == 0 {
            return SocketStream(fd: fd)
        }
        _ = closeFd(fd)
        pause(milliseconds: 15)
    }
    throw SocketError("connect")
}

/// A UDP socket on 127.0.0.1. A server binds the matrix port; a client binds an
/// ephemeral port and connects, so the server replies to the `recvfrom` source.
final class UdpSocket {
    let fd: Int32

    init(bindPort: UInt16) throws {
        fd = socket(AF_INET, datagramType, 0)
        guard fd >= 0 else { throw SocketError("socket") }
        reuseAddress(fd)
        guard withSockaddr(loopback(bindPort), { bind(fd, $0, $1) }) == 0 else {
            throw SocketError("bind")
        }
    }

    func connect(port: UInt16) throws {
        #if canImport(Glibc)
        let result = withSockaddr(loopback(port)) { Glibc.connect(fd, $0, $1) }
        #else
        let result = withSockaddr(loopback(port)) { Darwin.connect(fd, $0, $1) }
        #endif
        guard result == 0 else { throw SocketError("connect") }
    }

    func setReceiveTimeout(seconds: Int) {
        var timeout = timeval(tv_sec: seconds, tv_usec: 0)
        setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &timeout, socklen_t(MemoryLayout<timeval>.size))
    }

    func send(_ bytes: [UInt8]) throws {
        #if canImport(Glibc)
        let n = bytes.withUnsafeBytes { Glibc.send(fd, $0.baseAddress, bytes.count, 0) }
        #else
        let n = bytes.withUnsafeBytes { Darwin.send(fd, $0.baseAddress, bytes.count, 0) }
        #endif
        guard n == bytes.count else { throw SocketError("send") }
    }

    func receive() throws -> [UInt8] {
        var buffer = [UInt8](repeating: 0, count: 65536)
        #if canImport(Glibc)
        let n = buffer.withUnsafeMutableBytes { Glibc.recv(fd, $0.baseAddress, $0.count, 0) }
        #else
        let n = buffer.withUnsafeMutableBytes { Darwin.recv(fd, $0.baseAddress, $0.count, 0) }
        #endif
        guard n >= 0 else { throw SocketError("recv") }
        return Array(buffer[0..<n])
    }

    /// The next datagram and its source address, or nil on a receive error.
    func receiveFrom() -> ([UInt8], sockaddr_storage, socklen_t)? {
        var buffer = [UInt8](repeating: 0, count: 65536)
        var source = sockaddr_storage()
        var length = socklen_t(MemoryLayout<sockaddr_storage>.size)
        let n = buffer.withUnsafeMutableBytes { raw in
            withUnsafeMutablePointer(to: &source) {
                $0.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                    recvfrom(fd, raw.baseAddress, raw.count, 0, $0, &length)
                }
            }
        }
        guard n >= 0 else { return nil }
        return (Array(buffer[0..<n]), source, length)
    }

    func send(_ bytes: [UInt8], to destination: sockaddr_storage, length: socklen_t) {
        var target = destination
        _ = bytes.withUnsafeBytes { raw in
            withUnsafePointer(to: &target) {
                $0.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                    sendto(fd, raw.baseAddress, bytes.count, 0, $0, length)
                }
            }
        }
    }
}

#if canImport(Glibc)
private func closeFd(_ fd: Int32) -> Int32 { Glibc.close(fd) }
#else
private func closeFd(_ fd: Int32) -> Int32 { Darwin.close(fd) }
#endif
