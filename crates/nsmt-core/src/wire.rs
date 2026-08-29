//! 流式帧收发：在任意 `tokio::io::AsyncRead/AsyncWrite` 上按帧读写。
//!
//! 帧格式见 protocol.md §3.3。控制流（stream 0）逐帧读写；文件 chunk 走独立流。

use crate::error::NsmtError;
use crate::frame::{Frame, FrameCodec, MAX_PAYLOAD_LEN};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// 把 `reader/writer` 包装成帧流。读写可分离（quinn 的 RecvStream/SendStream）。
pub struct FrameStream<R, W> {
    reader: R,
    writer: W,
}

impl<R: AsyncRead + Unpin, W: AsyncWrite + Unpin> FrameStream<R, W> {
    pub fn new(reader: R, writer: W) -> Self {
        Self { reader, writer }
    }

    /// 写一帧。
    pub async fn send(&mut self, frame: &Frame) -> Result<(), NsmtError> {
        let wire = FrameCodec::encode(frame)?;
        self.writer.write_all(&wire).await?;
        self.writer.flush().await?;
        Ok(())
    }

    /// 读一帧。流正常关闭（EOF）时返回 `None`。
    pub async fn recv(&mut self) -> Result<Option<Frame>, NsmtError> {
        let mut header = [0u8; 12];
        let mut got = 0usize;
        while got < 12 {
            let n = self.reader.read(&mut header[got..]).await?;
            if n == 0 {
                if got == 0 {
                    return Ok(None); // clean EOF
                }
                return Err(NsmtError::Protocol("eof in frame header".into()));
            }
            got += n;
        }
        if header[0] != crate::frame::MAGIC {
            return Err(NsmtError::InvalidMagic(header[0]));
        }
        let version = header[1];
        if version != crate::frame::PROTOCOL_VERSION {
            return Err(NsmtError::UnsupportedVersion(version));
        }
        let payload_len =
            u32::from_le_bytes([header[8], header[9], header[10], header[11]]) as usize;
        if payload_len > MAX_PAYLOAD_LEN as usize {
            return Err(NsmtError::FrameTooLarge(payload_len));
        }
        let mut payload = vec![0u8; payload_len];
        let mut got = 0usize;
        while got < payload_len {
            let n = self.reader.read(&mut payload[got..]).await?;
            if n == 0 {
                return Err(NsmtError::Protocol("eof in frame payload".into()));
            }
            got += n;
        }
        let mut buf = Vec::with_capacity(12 + payload_len);
        buf.extend_from_slice(&header);
        buf.extend_from_slice(&payload);
        Ok(FrameCodec::decode(&buf)?.map(|mut f| {
            f.payload = payload;
            f
        }))
    }

    /// 写一个 JSON 消息帧。
    pub async fn send_json<T: serde::Serialize>(
        &mut self,
        frame_type: crate::frame::FrameType,
        stream_id: u32,
        value: &T,
    ) -> Result<(), NsmtError> {
        let frame = Frame::from_json(frame_type, stream_id, value)?;
        self.send(&frame).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::FrameType;
    use tokio::io::duplex;

    #[tokio::test]
    async fn roundtrip_stream() {
        // duplex(a,b): 写 a → 读 b；写 b → 读 a。
        // sender.writer=b → receiver.reader=a 构成单向通路；另一侧用 dummy。
        let (a, b) = duplex(1024);
        let (c, d) = duplex(1024);
        let mut sender = FrameStream::new(c, b);
        let mut receiver = FrameStream::new(a, d);

        let f = Frame::new(FrameType::Hello, 0, b"hello".to_vec());
        let sent = f.clone();
        tokio::spawn(async move {
            sender.send(&sent).await.unwrap();
        });
        let got = receiver.recv().await.unwrap().unwrap();
        assert_eq!(got, f);
    }
}
