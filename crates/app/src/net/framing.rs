//! Length-prefixed bulk frames over any async byte stream (TCP in prod,
//! `tokio::io::duplex` in tests).

use inputshare_protocol::{
    decode_msg, encode_bulk_frame, try_decode_bulk_frame, BulkMsg, CodecError, MAX_BULK_FRAME,
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub async fn write_frame<W: AsyncWrite + Unpin>(w: &mut W, msg: &BulkMsg) -> anyhow::Result<()> {
    let frame = encode_bulk_frame(msg)?;
    w.write_all(&frame).await?;
    Ok(())
}

/// Frame limit before authentication: an unauthenticated peer must not make us
/// allocate up to [`MAX_BULK_FRAME`].
pub const HANDSHAKE_MAX_FRAME: u32 = 64 * 1024;

/// Read exactly one frame of at most `max_len` bytes without over-reading
/// (used for the handshake, before the stream is handed to a [`FrameReader`]).
pub async fn read_frame<R: AsyncRead + Unpin>(r: &mut R, max_len: u32) -> anyhow::Result<BulkMsg> {
    let mut len_buf = [0u8; 4];
    r.read_exact(&mut len_buf).await?;
    let len = u32::from_le_bytes(len_buf);
    if len > max_len.min(MAX_BULK_FRAME) {
        anyhow::bail!("bulk frame too large: {len} bytes (limit {max_len})");
    }
    let mut body = vec![0u8; len as usize];
    r.read_exact(&mut body).await?;
    Ok(decode_msg(&body)?)
}

/// Buffered frame decoder for a long-lived bulk session.
pub struct FrameReader {
    buf: Vec<u8>,
    tmp: Box<[u8]>,
}

impl Default for FrameReader {
    fn default() -> Self {
        Self::new()
    }
}

impl FrameReader {
    pub fn new() -> Self {
        Self {
            buf: Vec::new(),
            tmp: vec![0u8; 64 * 1024].into_boxed_slice(),
        }
    }

    /// Next message, `Ok(None)` on clean EOF. A malformed or oversized frame is
    /// an error: a length-prefixed stream cannot be resynchronised after it.
    pub async fn next<R: AsyncRead + Unpin>(&mut self, r: &mut R) -> anyhow::Result<Option<BulkMsg>> {
        loop {
            match try_decode_bulk_frame(&self.buf) {
                Ok((msg, consumed)) => {
                    self.buf.drain(..consumed);
                    return Ok(Some(msg));
                }
                Err(CodecError::Incomplete) => {}
                Err(e) => anyhow::bail!("bulk decode: {e}"),
            }
            let n = r.read(&mut self.tmp).await?;
            if n == 0 {
                if !self.buf.is_empty() {
                    tracing::debug!(left = self.buf.len(), "bulk EOF inside a frame");
                }
                return Ok(None);
            }
            self.buf.extend_from_slice(&self.tmp[..n]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn frames_survive_byte_by_byte_delivery() {
        let (mut a, mut b) = tokio::io::duplex(16);
        let msgs = vec![
            BulkMsg::ClipboardText {
                text: "zażółć gęślą jaźń".into(),
            },
            BulkMsg::FileChunk {
                transfer_id: 9,
                offset: 0,
                data: (0..=255u8).cycle().take(70_000).collect(),
            },
            BulkMsg::Panic,
        ];
        let mut bytes = Vec::new();
        for m in &msgs {
            bytes.extend(encode_bulk_frame(m).unwrap());
        }
        let writer = tokio::spawn(async move {
            for byte in bytes {
                a.write_all(&[byte]).await.unwrap();
            }
        });
        let mut rd = FrameReader::new();
        match rd.next(&mut b).await.unwrap().unwrap() {
            BulkMsg::ClipboardText { text } => assert_eq!(text, "zażółć gęślą jaźń"),
            other => panic!("{other:?}"),
        }
        match rd.next(&mut b).await.unwrap().unwrap() {
            BulkMsg::FileChunk { data, .. } => {
                assert_eq!(data.len(), 70_000);
                assert_eq!(data[256], 0);
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(rd.next(&mut b).await.unwrap(), Some(BulkMsg::Panic)));
        writer.await.unwrap();
        assert!(rd.next(&mut b).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn oversized_frame_is_an_error() {
        let (mut a, mut b) = tokio::io::duplex(64);
        a.write_all(&(MAX_BULK_FRAME + 1).to_le_bytes()).await.unwrap();
        a.write_all(&[0; 8]).await.unwrap();
        drop(a);
        assert!(FrameReader::new().next(&mut b).await.is_err());

        let (mut a, mut b) = tokio::io::duplex(64);
        a.write_all(&(MAX_BULK_FRAME + 1).to_le_bytes()).await.unwrap();
        assert!(read_frame(&mut b, MAX_BULK_FRAME).await.is_err());

        // Pre-auth limit: a 1 MiB "Hello" is refused before allocating it.
        let (mut a, mut b) = tokio::io::duplex(64);
        a.write_all(&(1024u32 * 1024).to_le_bytes()).await.unwrap();
        assert!(read_frame(&mut b, HANDSHAKE_MAX_FRAME).await.is_err());
    }

    #[tokio::test]
    async fn garbage_body_is_an_error() {
        let (mut a, mut b) = tokio::io::duplex(64);
        a.write_all(&4u32.to_le_bytes()).await.unwrap();
        a.write_all(&[0xff, 0xff, 0xff, 0xff]).await.unwrap();
        assert!(FrameReader::new().next(&mut b).await.is_err());
    }

    #[tokio::test]
    async fn read_frame_does_not_over_read() {
        let (mut a, mut b) = tokio::io::duplex(1024);
        write_frame(&mut a, &BulkMsg::ClipboardRequest).await.unwrap();
        write_frame(&mut a, &BulkMsg::Panic).await.unwrap();
        assert!(matches!(
            read_frame(&mut b, HANDSHAKE_MAX_FRAME).await.unwrap(),
            BulkMsg::ClipboardRequest
        ));
        let mut rd = FrameReader::new();
        assert!(matches!(rd.next(&mut b).await.unwrap(), Some(BulkMsg::Panic)));
    }
}
