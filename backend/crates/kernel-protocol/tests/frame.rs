use kernel_protocol::*;
use serde_json::{Value, json};
use std::{
    future::Future,
    task::{Context, Poll, Waker},
};

// 内存切片的异步读写立即就绪，无需扩大协议 crate 的 Tokio 特性。
fn ready<T>(future: impl Future<Output = T>) -> T {
    let mut future = std::pin::pin!(future);
    match future
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()))
    {
        Poll::Ready(value) => value,
        Poll::Pending => panic!("in-memory IO must be ready"),
    }
}

#[test]
fn sync_and_async_frames_and_exact_limit() {
    let value = json!({"text":"中文"});
    let frame = encode_frame(&value).unwrap();
    assert_eq!(
        u32::from_be_bytes(frame[..4].try_into().unwrap()) as usize,
        frame.len() - 4
    );
    assert_eq!(decode_frame::<Value>(&frame).unwrap(), value);
    assert_eq!(
        ready(read_frame::<_, Value>(&mut frame.as_slice())).unwrap(),
        value
    );
    let mut output = Vec::new();
    ready(write_frame(&mut output, &value)).unwrap();
    assert_eq!(output, frame);
    let boundary = Value::String("x".repeat(MAX_FRAME_BYTES - 2));
    assert_eq!(encode_frame(&boundary).unwrap().len(), MAX_FRAME_BYTES + 4);
}

#[test]
fn oversize_rejected_before_reading_body_or_writing_any_bytes() {
    let header = ((MAX_FRAME_BYTES + 1) as u32).to_be_bytes();
    let error = ready(read_frame::<_, Value>(&mut header.as_slice())).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    assert!(decode_frame::<Value>(&header).is_err());
    let oversized = Value::String("x".repeat(MAX_FRAME_BYTES));
    let mut output = Vec::new();
    assert!(encode_frame(&oversized).is_err());
    assert!(ready(write_frame(&mut output, &oversized)).is_err());
    assert!(output.is_empty());
}

#[test]
fn malformed_and_truncated_frames_rejected() {
    for frame in [
        vec![],
        vec![0, 0, 0],
        vec![0, 0, 0, 1],
        vec![0, 0, 0, 0],
        vec![0, 0, 0, 1, 255],
    ] {
        assert!(decode_frame::<Value>(&frame).is_err());
        assert!(ready(read_frame::<_, Value>(&mut frame.as_slice())).is_err());
    }
    let mut trailing = encode_frame(&json!({})).unwrap();
    trailing.push(0);
    assert!(decode_frame::<Value>(&trailing).is_err());
}
