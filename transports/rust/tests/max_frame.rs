//! The configurable max-frame guard (conventions doc §5): a host sets the limit up
//! or down through the carrier's public API, the limit applies to reads and writes
//! alike, an oversized inbound length is rejected before allocation, and an invalid
//! limit fails at construction rather than on the first frame.

use csilgen_transport::carrier::{FrameCarrier, StreamCarrier};
use csilgen_transport::conventions::{MAX_FRAME_DEFAULT, MAX_FRAME_LIMIT, TransportError};
use std::io::{Cursor, Read, Write};

/// A stream whose reads are counted, so a test can prove the guard fires before the
/// frame body is ever pulled off the wire.
struct CountingStream {
    inner: Cursor<Vec<u8>>,
    read: usize,
}

impl CountingStream {
    fn new(bytes: Vec<u8>) -> Self {
        Self {
            inner: Cursor::new(bytes),
            read: 0,
        }
    }
}

impl Read for CountingStream {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.read += n;
        Ok(n)
    }
}

impl Write for CountingStream {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// An in-memory duplex: writes land in a buffer that subsequent reads drain.
#[derive(Default)]
struct Duplex {
    buf: Vec<u8>,
    pos: usize,
}

impl Read for Duplex {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        let n = (&self.buf[self.pos..]).read(out)?;
        self.pos += n;
        Ok(n)
    }
}

impl Write for Duplex {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        self.buf.extend_from_slice(data);
        Ok(data.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn default_limit_accepts_frame_below_it() {
    let mut carrier = StreamCarrier::new(Duplex::default());
    let frame = vec![0xAB; 1024];
    carrier.send_frame(&frame).expect("frame under the limit");
    let got = carrier.recv_frame().expect("read back").expect("a frame");
    assert_eq!(got, frame);
}

#[test]
fn default_limit_rejects_frame_above_it() {
    let mut carrier = StreamCarrier::new(Duplex::default());
    let frame = vec![0u8; MAX_FRAME_DEFAULT + 1];
    match carrier.send_frame(&frame) {
        Err(TransportError::FrameTooLarge { got, max }) => {
            assert_eq!(got, MAX_FRAME_DEFAULT + 1);
            assert_eq!(max, MAX_FRAME_DEFAULT);
        }
        other => panic!("expected FrameTooLarge, got {other:?}"),
    }
}

#[test]
fn larger_custom_limit_accepts_what_default_rejects() {
    let mut carrier = StreamCarrier::with_max_frame(Duplex::default(), MAX_FRAME_DEFAULT + 4096)
        .expect("a limit above the default is valid");
    let frame = vec![0u8; MAX_FRAME_DEFAULT + 1];
    carrier.send_frame(&frame).expect("raised limit accepts");
    let got = carrier.recv_frame().expect("read back").expect("a frame");
    assert_eq!(got.len(), frame.len());
}

#[test]
fn smaller_custom_limit_rejects_what_default_accepts() {
    let mut carrier =
        StreamCarrier::with_max_frame(Duplex::default(), 64).expect("64 is a valid limit");
    let frame = vec![0xCD; 1024];
    assert!(matches!(
        carrier.send_frame(&frame),
        Err(TransportError::FrameTooLarge { got: 1024, max: 64 })
    ));
}

#[test]
fn oversized_incoming_length_rejected_before_allocation() {
    // A prefix claiming ~4 GiB followed by no body: if the guard ran after the read
    // this would allocate; it must fail on the prefix alone.
    let stream = CountingStream::new(vec![0xFF, 0xFF, 0xFF, 0xFF]);
    let mut carrier = StreamCarrier::with_max_frame(stream, 4096).expect("4096 is valid");
    assert!(matches!(
        carrier.recv_frame(),
        Err(TransportError::FrameTooLarge { .. })
    ));
    assert_eq!(
        carrier.into_inner().read,
        4,
        "guard must fire on the 4-byte prefix alone"
    );
}

#[test]
fn invalid_limits_rejected_at_construction() {
    for limit in [0usize, MAX_FRAME_LIMIT + 1, usize::MAX] {
        match StreamCarrier::with_max_frame(Duplex::default(), limit) {
            Err(TransportError::InvalidMaxFrame { got, limit: lim }) => {
                assert_eq!(got, limit);
                assert_eq!(lim, MAX_FRAME_LIMIT);
            }
            _ => panic!("limit {limit} must be rejected"),
        }
    }
    for limit in [1usize, MAX_FRAME_DEFAULT, MAX_FRAME_LIMIT] {
        assert!(
            StreamCarrier::with_max_frame(Duplex::default(), limit).is_ok(),
            "limit {limit} must be accepted"
        );
    }
}
