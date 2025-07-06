//! Encode/decode helpers for TCP framing modes.
//!
//! The write side is synchronous (produces a `Vec<u8>`).  The read side is
//! async and uses compio's buffer-passing I/O model.

use std::io;

use compio::io::{AsyncRead, AsyncReadExt};

use crate::spec::TcpFraming;

// ---------------------------------------------------------------------------
// Encode (synchronous)
// ---------------------------------------------------------------------------

/// Encode `payload` according to `framing`, returning the bytes that should
/// be written to the wire.
pub fn encode_frame(payload: &[u8], framing: &TcpFraming) -> Vec<u8> {
    match framing {
        TcpFraming::Raw => payload.to_vec(),

        TcpFraming::LengthPrefixed { width } => {
            let len = payload.len();
            let mut buf = Vec::with_capacity(*width as usize + len);
            match width {
                1 => buf.push(len as u8),
                2 => buf.extend_from_slice(&(len as u16).to_be_bytes()),
                4 => buf.extend_from_slice(&(len as u32).to_be_bytes()),
                _ => buf.extend_from_slice(&(len as u32).to_be_bytes()),
            }
            buf.extend_from_slice(payload);
            buf
        }

        TcpFraming::Delimiter(delim) => {
            let mut buf = Vec::with_capacity(payload.len() + delim.len());
            buf.extend_from_slice(payload);
            buf.extend_from_slice(delim);
            buf
        }

        TcpFraming::FixedSize(size) => {
            let mut buf = vec![0u8; *size];
            let copy_len = payload.len().min(*size);
            buf[..copy_len].copy_from_slice(&payload[..copy_len]);
            buf
        }
    }
}

// ---------------------------------------------------------------------------
// Decode (async, compio buffer-passing)
// ---------------------------------------------------------------------------

/// Read a framed response from `stream`.
///
/// Returns the response bytes.  The caller is responsible for timing; the
/// returned `Vec<u8>` can be empty if no data was available (e.g. Raw mode
/// with a quiet server).
pub async fn read_framed<S: AsyncRead>(
    stream: &mut S,
    framing: &TcpFraming,
    max_bytes: usize,
) -> io::Result<Vec<u8>> {
    match framing {
        TcpFraming::Raw => read_raw(stream, max_bytes).await,
        TcpFraming::Delimiter(delim) => read_until_delimiter(stream, delim, max_bytes).await,
        TcpFraming::LengthPrefixed { width } => {
            read_length_prefixed(stream, *width, max_bytes).await
        }
        TcpFraming::FixedSize(size) => read_fixed(stream, *size).await,
    }
}

/// Raw mode: perform a single read into a buffer up to `max_bytes`.
async fn read_raw<S: AsyncRead>(stream: &mut S, max_bytes: usize) -> io::Result<Vec<u8>> {
    let buf = Vec::with_capacity(max_bytes);
    let result = stream.read(buf).await;
    let bytes_read = result.0?;
    let mut buf = result.1;
    // The buffer was passed to compio which may have written `bytes_read` bytes.
    // `AsyncReadExt::read` on a Vec<u8> appends at the initialized end, so
    // `buf.len()` should reflect how many bytes were actually read.
    // Truncate to the number of bytes actually read if necessary.
    buf.truncate(bytes_read);
    Ok(buf)
}

/// Delimiter mode: read one byte at a time until the delimiter sequence is
/// found.  Not optimal but correct for v1.
async fn read_until_delimiter<S: AsyncRead>(
    stream: &mut S,
    delimiter: &[u8],
    max_bytes: usize,
) -> io::Result<Vec<u8>> {
    if delimiter.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "empty delimiter",
        ));
    }

    let mut accumulated = Vec::new();

    loop {
        if accumulated.len() >= max_bytes {
            return Err(io::Error::other(
                "response exceeded max_bytes before delimiter found",
            ));
        }

        // Read a single byte via a 1-byte buffer.
        let one = vec![0u8; 1];
        let result = stream.read(one).await;
        let n = result.0?;
        let one = result.1;

        if n == 0 {
            // EOF before delimiter
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "connection closed before delimiter found",
            ));
        }

        accumulated.push(one[0]);

        // Check if accumulated ends with delimiter.
        if accumulated.len() >= delimiter.len()
            && &accumulated[accumulated.len() - delimiter.len()..] == delimiter
        {
            // Strip the delimiter from the returned data.
            accumulated.truncate(accumulated.len() - delimiter.len());
            return Ok(accumulated);
        }
    }
}

/// Length-prefixed mode: read a `width`-byte big-endian length header, then
/// read exactly that many bytes.
async fn read_length_prefixed<S: AsyncRead + AsyncReadExt>(
    stream: &mut S,
    width: u8,
    max_bytes: usize,
) -> io::Result<Vec<u8>> {
    let header_buf = vec![0u8; width as usize];
    let result = stream.read_exact(header_buf).await;
    result.0?;
    let header_buf = result.1;

    let payload_len = match width {
        1 => header_buf[0] as usize,
        2 => u16::from_be_bytes([header_buf[0], header_buf[1]]) as usize,
        4 => u32::from_be_bytes([header_buf[0], header_buf[1], header_buf[2], header_buf[3]])
            as usize,
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("unsupported length-prefix width: {width}"),
            ));
        }
    };

    if payload_len > max_bytes {
        return Err(io::Error::other(format!(
            "length-prefixed payload size {payload_len} exceeds max_bytes {max_bytes}"
        )));
    }

    let payload_buf = vec![0u8; payload_len];
    let result = stream.read_exact(payload_buf).await;
    result.0?;
    Ok(result.1)
}

/// Fixed-size mode: read exactly `size` bytes.
async fn read_fixed<S: AsyncRead + AsyncReadExt>(
    stream: &mut S,
    size: usize,
) -> io::Result<Vec<u8>> {
    let buf = vec![0u8; size];
    let result = stream.read_exact(buf).await;
    result.0?;
    Ok(result.1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_raw_is_identity() {
        let data = b"hello";
        assert_eq!(encode_frame(data, &TcpFraming::Raw), data.to_vec());
    }

    #[test]
    fn encode_length_prefixed_2_bytes() {
        let data = b"hi";
        let encoded = encode_frame(data, &TcpFraming::LengthPrefixed { width: 2 });
        assert_eq!(&encoded[..2], &[0, 2]); // big-endian length = 2
        assert_eq!(&encoded[2..], b"hi");
    }

    #[test]
    fn encode_length_prefixed_4_bytes() {
        let data = b"test";
        let encoded = encode_frame(data, &TcpFraming::LengthPrefixed { width: 4 });
        assert_eq!(&encoded[..4], &[0, 0, 0, 4]);
        assert_eq!(&encoded[4..], b"test");
    }

    #[test]
    fn encode_delimiter_appends() {
        let data = b"PING";
        let encoded = encode_frame(data, &TcpFraming::Delimiter(b"\r\n".to_vec()));
        assert_eq!(&encoded, b"PING\r\n");
    }

    #[test]
    fn encode_fixed_size_pads() {
        let data = b"AB";
        let encoded = encode_frame(data, &TcpFraming::FixedSize(5));
        assert_eq!(encoded.len(), 5);
        assert_eq!(&encoded[..2], b"AB");
        assert_eq!(&encoded[2..], &[0, 0, 0]);
    }

    #[test]
    fn encode_fixed_size_truncates() {
        let data = b"ABCDEF";
        let encoded = encode_frame(data, &TcpFraming::FixedSize(3));
        assert_eq!(encoded.len(), 3);
        assert_eq!(&encoded, b"ABC");
    }
}
