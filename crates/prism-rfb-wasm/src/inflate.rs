//! A persistent zlib stream (ZRLE and Tight keep their dictionaries across
//! rectangles), with a cap on how much one rectangle may decompress to.

use miniz_oxide::inflate::stream::{InflateState, inflate};
use miniz_oxide::{DataFormat, MZError, MZFlush, MZStatus};

use crate::{Error, ErrorKind};

const CHUNK: usize = 256 * 1024;

pub struct Inflater(Box<InflateState>);

impl std::fmt::Debug for Inflater {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Inflater")
    }
}

impl Default for Inflater {
    fn default() -> Self {
        Self(InflateState::new_boxed(DataFormat::Zlib))
    }
}

impl Inflater {
    /// Starts a new stream (Tight's per-stream reset bits).
    pub fn reset(&mut self) {
        self.0.reset(DataFormat::Zlib);
    }

    /// Decompresses all of `input` into `out` (cleared first). Fails if the
    /// output would exceed `limit` bytes or the stream is corrupt.
    pub fn inflate(
        &mut self,
        mut input: &[u8],
        out: &mut Vec<u8>,
        limit: usize,
    ) -> Result<(), Error> {
        out.clear();
        loop {
            let len = out.len();
            // One byte more than allowed, so overruns are detectable.
            let room = (limit + 1 - len).min(CHUNK);
            out.resize(len + room, 0);
            let r = inflate(&mut self.0, input, &mut out[len..], MZFlush::None);
            out.truncate(len + r.bytes_written);
            input = &input[r.bytes_consumed..];
            if out.len() > limit {
                return Err(Error::new(
                    ErrorKind::Inflate,
                    "decompressed data exceeds the rectangle",
                ));
            }
            match r.status {
                Ok(MZStatus::StreamEnd) => return Ok(()),
                Ok(_) => {}
                // No progress possible: fine once all input is consumed.
                Err(MZError::Buf) if input.is_empty() => return Ok(()),
                Err(e) => return Err(Error::new(ErrorKind::Inflate, format!("zlib: {e:?}"))),
            }
            let drained = r.bytes_written < room;
            if input.is_empty() && drained {
                return Ok(());
            }
            if r.bytes_consumed == 0 && r.bytes_written == 0 {
                return Err(Error::new(ErrorKind::Inflate, "zlib stream stalled"));
            }
        }
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use miniz_oxide::deflate::core::{
        CompressorOxide, TDEFLFlush, compress, create_comp_flags_from_zip_params,
    };

    /// Compresses `chunks` as one zlib stream, sync-flushing after each
    /// chunk the way VNC servers do, and returns each chunk's bytes.
    pub fn zlib_chunks(chunks: &[&[u8]]) -> Vec<Vec<u8>> {
        let mut c = CompressorOxide::new(create_comp_flags_from_zip_params(6, 1, 0));
        chunks
            .iter()
            .map(|data| {
                let mut out = vec![0; data.len() * 2 + 64];
                let (_, _, written) = compress(&mut c, data, &mut out, TDEFLFlush::Sync);
                out.truncate(written);
                out
            })
            .collect()
    }

    #[test]
    fn persistent_stream_across_chunks() {
        let a = vec![7u8; 1000];
        let b: Vec<u8> = (0..=255).collect();
        let z = zlib_chunks(&[&a, &b]);
        let mut inf = Inflater::default();
        let mut out = Vec::new();
        inf.inflate(&z[0], &mut out, 1000).unwrap();
        assert_eq!(out, a);
        inf.inflate(&z[1], &mut out, 256).unwrap();
        assert_eq!(out, b);
    }

    #[test]
    fn limit_enforced() {
        let z = zlib_chunks(&[&[0u8; 100]]);
        let mut out = Vec::new();
        assert!(Inflater::default().inflate(&z[0], &mut out, 99).is_err());
    }
}
