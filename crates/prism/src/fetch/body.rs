//! Response body framing: content-length, chunked, or until close.

use std::io;

use tokio::io::{AsyncBufReadExt, AsyncReadExt};

use super::Reader;

enum Framing {
    Length(u64),
    /// Bytes left in the current chunk, or `None` between chunks.
    Chunked(Option<u64>),
    Close,
    Done,
}

pub struct BodyReader {
    inner: Reader,
    framing: Framing,
}

impl BodyReader {
    pub fn new(inner: Reader, length: Option<u64>, chunked: bool) -> Self {
        let framing = match (chunked, length) {
            (true, _) => Framing::Chunked(None),
            (false, Some(n)) => Framing::Length(n),
            (false, None) => Framing::Close,
        };
        BodyReader { inner, framing }
    }

    /// Reads up to `buf.len()` body bytes; 0 at the end.
    pub async fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            match &mut self.framing {
                Framing::Done => return Ok(0),
                Framing::Close => return self.inner.read(buf).await,
                Framing::Length(left) => {
                    if *left == 0 {
                        self.framing = Framing::Done;
                        continue;
                    }
                    let want = buf.len().min(usize::try_from(*left).unwrap_or(usize::MAX));
                    let n = self.inner.read(&mut buf[..want]).await?;
                    if n == 0 {
                        return Err(io::ErrorKind::UnexpectedEof.into());
                    }
                    *left -= n as u64;
                    return Ok(n);
                }
                Framing::Chunked(None) => {
                    let mut line = String::new();
                    if self.inner.read_line(&mut line).await? == 0 {
                        return Err(io::ErrorKind::UnexpectedEof.into());
                    }
                    let size = line.split(';').next().unwrap_or("").trim();
                    let size = u64::from_str_radix(size, 16).map_err(|_| {
                        io::Error::new(io::ErrorKind::InvalidData, "bad chunk size")
                    })?;
                    if size == 0 {
                        // Trailers end at a blank line.
                        loop {
                            let mut t = String::new();
                            if self.inner.read_line(&mut t).await? == 0 || t.trim().is_empty() {
                                break;
                            }
                        }
                        self.framing = Framing::Done;
                    } else {
                        self.framing = Framing::Chunked(Some(size));
                    }
                }
                Framing::Chunked(Some(left)) => {
                    let want = buf.len().min(usize::try_from(*left).unwrap_or(usize::MAX));
                    let n = self.inner.read(&mut buf[..want]).await?;
                    if n == 0 {
                        return Err(io::ErrorKind::UnexpectedEof.into());
                    }
                    *left -= n as u64;
                    if *left == 0 {
                        let mut crlf = String::new();
                        self.inner.read_line(&mut crlf).await?;
                        self.framing = Framing::Chunked(None);
                    }
                    return Ok(n);
                }
            }
        }
    }
}
