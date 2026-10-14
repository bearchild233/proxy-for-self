use std::io;

use serde::{Serialize, de::DeserializeOwned};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const MAX_FRAME_BYTES: usize = 1_048_576;

fn invalid(message: impl Into<Box<dyn std::error::Error + Send + Sync>>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn frame_length(header: [u8; 4]) -> io::Result<usize> {
    let length = u32::from_be_bytes(header) as usize;
    if length > MAX_FRAME_BYTES {
        return Err(invalid("frame exceeds MAX_FRAME_BYTES"));
    }
    Ok(length)
}

/// 编码完整帧；超限时调用方拿不到可发送的帧。
pub fn encode_frame<T: Serialize>(message: &T) -> io::Result<Vec<u8>> {
    let body = serde_json::to_vec(message).map_err(invalid)?;
    if body.len() > MAX_FRAME_BYTES {
        return Err(invalid("frame exceeds MAX_FRAME_BYTES"));
    }
    let mut frame = Vec::with_capacity(4 + body.len());
    frame.extend_from_slice(&(body.len() as u32).to_be_bytes());
    frame.extend_from_slice(&body);
    Ok(frame)
}

/// 解码恰好一帧，截断和尾随字节均作为协议错误处理。
pub fn decode_frame<T: DeserializeOwned>(frame: &[u8]) -> io::Result<T> {
    let header = frame
        .get(..4)
        .ok_or_else(|| invalid("missing frame header"))?;
    let length = frame_length(header.try_into().expect("four bytes"))?;
    if frame.len() != length + 4 {
        return Err(invalid("frame length mismatch"));
    }
    serde_json::from_slice(&frame[4..]).map_err(invalid)
}

/// 超限长度在分配正文缓冲区前拒绝；错误后必须断开连接。
pub async fn read_frame<R: AsyncRead + Unpin, T: DeserializeOwned>(
    reader: &mut R,
) -> io::Result<T> {
    let mut header = [0; 4];
    reader.read_exact(&mut header).await?;
    let length = frame_length(header)?;
    let mut body = vec![0; length];
    reader.read_exact(&mut body).await?;
    serde_json::from_slice(&body).map_err(invalid)
}

pub async fn write_frame<W: AsyncWrite + Unpin, T: Serialize>(
    writer: &mut W,
    message: &T,
) -> io::Result<()> {
    writer.write_all(&encode_frame(message)?).await
}
