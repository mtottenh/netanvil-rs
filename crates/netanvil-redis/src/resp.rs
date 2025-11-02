//! RESP (REdis Serialization Protocol) encoder and decoder.
//!
//! Implements the RESP2 wire format used by Redis clients and servers.
//! See <https://redis.io/docs/reference/protocol-spec/>.

use std::fmt;
use std::io;
use std::pin::Pin;

use compio::buf::BufResult;
use compio::io::AsyncReadExt;

// ---------------------------------------------------------------------------
// Encoding (client -> server)
// ---------------------------------------------------------------------------

/// Encode a Redis command and its arguments as a RESP array of bulk strings.
///
/// The command itself is the first element; `args` follow.
/// Example: `encode_command("SET", &["mykey".into(), "myvalue".into()])` produces
/// `*3\r\n$3\r\nSET\r\n$5\r\nmykey\r\n$7\r\nmyvalue\r\n`.
pub fn encode_command(command: &str, args: &[String]) -> Vec<u8> {
    let num_elements = 1 + args.len();
    let mut buf =
        Vec::with_capacity(64 + command.len() + args.iter().map(|a| a.len() + 16).sum::<usize>());

    // Array header
    buf.extend_from_slice(b"*");
    buf.extend_from_slice(num_elements.to_string().as_bytes());
    buf.extend_from_slice(b"\r\n");

    // Command as bulk string
    write_bulk_string(&mut buf, command.as_bytes());

    // Each argument as bulk string
    for arg in args {
        write_bulk_string(&mut buf, arg.as_bytes());
    }

    buf
}

fn write_bulk_string(buf: &mut Vec<u8>, data: &[u8]) {
    buf.extend_from_slice(b"$");
    buf.extend_from_slice(data.len().to_string().as_bytes());
    buf.extend_from_slice(b"\r\n");
    buf.extend_from_slice(data);
    buf.extend_from_slice(b"\r\n");
}

// ---------------------------------------------------------------------------
// Response types
// ---------------------------------------------------------------------------

/// A decoded RESP value from a Redis server response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RespValue {
    /// `+OK\r\n` — simple status string.
    SimpleString(String),
    /// `-ERR message\r\n` — error.
    Error(String),
    /// `:42\r\n` — integer.
    Integer(i64),
    /// `$6\r\nfoobar\r\n` — bulk string. `None` for null bulk string (`$-1\r\n`).
    BulkString(Option<Vec<u8>>),
    /// `*3\r\n...` — array of RESP values.
    Array(Vec<RespValue>),
}

impl fmt::Display for RespValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RespValue::SimpleString(s) => write!(f, "+{s}"),
            RespValue::Error(s) => write!(f, "-{s}"),
            RespValue::Integer(n) => write!(f, ":{n}"),
            RespValue::BulkString(Some(b)) => {
                write!(f, "${}", String::from_utf8_lossy(b))
            }
            RespValue::BulkString(None) => write!(f, "$nil"),
            RespValue::Array(arr) => {
                write!(f, "*[")?;
                for (i, v) in arr.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{v}")?;
                }
                write!(f, "]")
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Decoding (server -> client)
// ---------------------------------------------------------------------------

/// Read one complete RESP value from a stream.
///
/// Returns `(value, bytes_consumed)`. `max_bytes` limits the total bytes read
/// to prevent unbounded allocation from a misbehaving server.
pub async fn read_response<S: compio::io::AsyncRead>(
    stream: &mut S,
    max_bytes: usize,
) -> io::Result<(RespValue, usize)> {
    let mut buf = Vec::with_capacity(256);
    let mut total_read = 0usize;

    read_value(stream, &mut buf, &mut total_read, max_bytes).await
}

/// Recursively read a single RESP value.
///
/// Returns a boxed future because RESP arrays contain nested values,
/// making this function recursive. Rust async fns require boxing for
/// recursive calls.
fn read_value<'a, S: compio::io::AsyncRead + 'a>(
    stream: &'a mut S,
    buf: &'a mut Vec<u8>,
    total_read: &'a mut usize,
    max_bytes: usize,
) -> Pin<Box<dyn std::future::Future<Output = io::Result<(RespValue, usize)>> + 'a>> {
    Box::pin(async move {
        // Read the type byte + line
        let line = read_line(stream, buf, total_read, max_bytes).await?;

        if line.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "empty RESP line",
            ));
        }

        let type_byte = line[0];
        let payload = &line[1..];
        let payload_str = std::str::from_utf8(payload).map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("non-UTF8 RESP line: {e}"),
            )
        })?;

        match type_byte {
            b'+' => Ok((
                RespValue::SimpleString(payload_str.to_string()),
                *total_read,
            )),
            b'-' => Ok((RespValue::Error(payload_str.to_string()), *total_read)),
            b':' => {
                let n: i64 = payload_str.parse().map_err(|e| {
                    io::Error::new(io::ErrorKind::InvalidData, format!("bad integer: {e}"))
                })?;
                Ok((RespValue::Integer(n), *total_read))
            }
            b'$' => {
                let len: i64 = payload_str.parse().map_err(|e| {
                    io::Error::new(io::ErrorKind::InvalidData, format!("bad bulk length: {e}"))
                })?;
                if len < 0 {
                    // Null bulk string
                    return Ok((RespValue::BulkString(None), *total_read));
                }
                let len = len as usize;
                if *total_read + len + 2 > max_bytes {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("bulk string too large: {len} bytes"),
                    ));
                }
                // Read exactly `len` bytes + \r\n
                let data_buf = vec![0u8; len + 2];
                let BufResult(result, data_buf) = stream.read_exact(data_buf).await;
                result?;
                *total_read += len + 2;
                Ok((
                    RespValue::BulkString(Some(data_buf[..len].to_vec())),
                    *total_read,
                ))
            }
            b'*' => {
                let count: i64 = payload_str.parse().map_err(|e| {
                    io::Error::new(io::ErrorKind::InvalidData, format!("bad array count: {e}"))
                })?;
                if count < 0 {
                    // Null array — treat as empty
                    return Ok((RespValue::Array(vec![]), *total_read));
                }
                let count = count as usize;
                let mut elements = Vec::with_capacity(count.min(1024));
                for _ in 0..count {
                    let (val, _) = read_value(stream, buf, total_read, max_bytes).await?;
                    elements.push(val);
                }
                Ok((RespValue::Array(elements), *total_read))
            }
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unknown RESP type byte: 0x{type_byte:02x}"),
            )),
        }
    })
}

/// Read bytes until `\r\n` is found, returning the line without the delimiter.
async fn read_line<S: compio::io::AsyncRead>(
    stream: &mut S,
    buf: &mut Vec<u8>,
    total_read: &mut usize,
    max_bytes: usize,
) -> io::Result<Vec<u8>> {
    let mut line = Vec::with_capacity(64);

    loop {
        if *total_read >= max_bytes {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "exceeded max_bytes reading RESP line",
            ));
        }

        // Read one byte at a time (RESP lines are short; this is simple and correct)
        let one = vec![0u8; 1];
        let BufResult(result, one) = stream.read_exact(one).await;
        result?;
        *total_read += 1;

        let byte = one[0];
        if byte == b'\r' {
            // Expect \n next
            let nl = vec![0u8; 1];
            let BufResult(result, _nl) = stream.read_exact(nl).await;
            result?;
            *total_read += 1;
            break;
        }
        line.push(byte);
    }

    // Store in buf for potential reuse
    buf.clear();
    buf.extend_from_slice(&line);

    Ok(line)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_ping() {
        let encoded = encode_command("PING", &[]);
        assert_eq!(encoded, b"*1\r\n$4\r\nPING\r\n");
    }

    #[test]
    fn encode_set() {
        let encoded = encode_command("SET", &["mykey".into(), "myvalue".into()]);
        assert_eq!(
            encoded,
            b"*3\r\n$3\r\nSET\r\n$5\r\nmykey\r\n$7\r\nmyvalue\r\n"
        );
    }

    #[test]
    fn encode_get() {
        let encoded = encode_command("GET", &["mykey".into()]);
        assert_eq!(encoded, b"*2\r\n$3\r\nGET\r\n$5\r\nmykey\r\n");
    }

    #[test]
    fn encode_empty_arg() {
        let encoded = encode_command("SET", &["key".into(), "".into()]);
        assert_eq!(encoded, b"*3\r\n$3\r\nSET\r\n$3\r\nkey\r\n$0\r\n\r\n");
    }

    // Async tests using compio runtime.
    #[compio::test]
    async fn decode_simple_string() {
        let data = b"+OK\r\n";
        let mut cursor = &data[..];
        let (val, bytes) = read_response(&mut cursor, 1024).await.unwrap();
        assert_eq!(val, RespValue::SimpleString("OK".into()));
        assert_eq!(bytes, 5);
    }

    #[compio::test]
    async fn decode_error() {
        let data = b"-ERR unknown command\r\n";
        let mut cursor = &data[..];
        let (val, _) = read_response(&mut cursor, 1024).await.unwrap();
        assert_eq!(val, RespValue::Error("ERR unknown command".into()));
    }

    #[compio::test]
    async fn decode_integer() {
        let data = b":42\r\n";
        let mut cursor = &data[..];
        let (val, _) = read_response(&mut cursor, 1024).await.unwrap();
        assert_eq!(val, RespValue::Integer(42));
    }

    #[compio::test]
    async fn decode_negative_integer() {
        let data = b":-1\r\n";
        let mut cursor = &data[..];
        let (val, _) = read_response(&mut cursor, 1024).await.unwrap();
        assert_eq!(val, RespValue::Integer(-1));
    }

    #[compio::test]
    async fn decode_bulk_string() {
        let data = b"$6\r\nfoobar\r\n";
        let mut cursor = &data[..];
        let (val, _) = read_response(&mut cursor, 1024).await.unwrap();
        assert_eq!(val, RespValue::BulkString(Some(b"foobar".to_vec())));
    }

    #[compio::test]
    async fn decode_null_bulk_string() {
        let data = b"$-1\r\n";
        let mut cursor = &data[..];
        let (val, _) = read_response(&mut cursor, 1024).await.unwrap();
        assert_eq!(val, RespValue::BulkString(None));
    }

    #[compio::test]
    async fn decode_empty_bulk_string() {
        let data = b"$0\r\n\r\n";
        let mut cursor = &data[..];
        let (val, _) = read_response(&mut cursor, 1024).await.unwrap();
        assert_eq!(val, RespValue::BulkString(Some(vec![])));
    }

    #[compio::test]
    async fn decode_array() {
        // *3\r\n:1\r\n:2\r\n:3\r\n
        let data = b"*3\r\n:1\r\n:2\r\n:3\r\n";
        let mut cursor = &data[..];
        let (val, _) = read_response(&mut cursor, 1024).await.unwrap();
        assert_eq!(
            val,
            RespValue::Array(vec![
                RespValue::Integer(1),
                RespValue::Integer(2),
                RespValue::Integer(3),
            ])
        );
    }

    #[compio::test]
    async fn decode_mixed_array() {
        // *2\r\n+OK\r\n$5\r\nhello\r\n
        let data = b"*2\r\n+OK\r\n$5\r\nhello\r\n";
        let mut cursor = &data[..];
        let (val, _) = read_response(&mut cursor, 1024).await.unwrap();
        assert_eq!(
            val,
            RespValue::Array(vec![
                RespValue::SimpleString("OK".into()),
                RespValue::BulkString(Some(b"hello".to_vec())),
            ])
        );
    }

    #[compio::test]
    async fn decode_empty_array() {
        let data = b"*0\r\n";
        let mut cursor = &data[..];
        let (val, _) = read_response(&mut cursor, 1024).await.unwrap();
        assert_eq!(val, RespValue::Array(vec![]));
    }
}
