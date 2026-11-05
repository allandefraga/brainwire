//! Bolt chunk framing: each chunk is a 2-byte big-endian size and its data; a
//! message ends with a zero-size chunk. A zero-size chunk with no data before it
//! is a NOOP (keep-alive) and is skipped.

use crate::ingress::IngressLease;

const MAX_CHUNK: usize = 0xFFFF;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("message payload exceeds its size limit")]
pub struct MessageTooLarge;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReadError {
    TooLarge,
    /// Refused before copying; carries the size hint (requested, available) and the
    /// reader skips the rest of this message, so the connection stays usable.
    IngressExhausted {
        requested: u64,
        available: u64,
    },
}

/// After a refusal, the next message this small (RESET, ROLLBACK) is admitted even when
/// the shared pool is still full, so the session can recover.
/// ponytail: <= 1 KiB per refused connection escapes the pool; connection limits bound it.
const CONTROL_MESSAGE_BYTES: usize = 1024;

pub fn encode_message(payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + 4 + 2 * (payload.len() / MAX_CHUNK));
    for chunk in payload.chunks(MAX_CHUNK) {
        out.extend_from_slice(&(chunk.len() as u16).to_be_bytes());
        out.extend_from_slice(chunk);
    }
    out.extend_from_slice(&[0, 0]);
    out
}

/// Writes Bolt framing with bounded scratch space, without copying the entire payload.
#[cfg(test)]
pub(crate) async fn write_message<W: tokio::io::AsyncWrite + Unpin>(
    stream: &mut W,
    payload: &[u8],
) -> std::io::Result<()> {
    use tokio::io::AsyncWriteExt;
    if payload.is_empty() {
        return stream.write_all(&[0, 0]).await;
    }
    let mut frame = Vec::new();
    frame
        .try_reserve_exact(payload.len().min(MAX_CHUNK) + 4)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::OutOfMemory, error))?;
    let mut chunks = payload.chunks(MAX_CHUNK).peekable();
    while let Some(chunk) = chunks.next() {
        frame.clear();
        frame.extend_from_slice(&(chunk.len() as u16).to_be_bytes());
        frame.extend_from_slice(chunk);
        if chunks.peek().is_none() {
            frame.extend_from_slice(&[0, 0]);
        }
        stream.write_all(&frame).await?;
    }
    Ok(())
}

/// Encodes a response directly into bounded Bolt frames. Dropping this future
/// drops its scratch and borrowed traversal state; a partial message is fatal.
pub(crate) async fn write_packstream_message<W: tokio::io::AsyncWrite + Unpin>(
    stream: &mut W,
    value: &crate::packstream::PackValue,
) -> std::io::Result<()> {
    use crate::packstream::PackFragments;
    use tokio::io::AsyncWriteExt;
    let mut fragments = PackFragments::new(value)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    // Reject depth/format errors before sending any bytes. Engine work may
    // already have committed, so an encoding failure must close the connection.
    let mut encoded_len = 0usize;
    for (index, fragment) in fragments.by_ref().enumerate() {
        let fragment = fragment
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
        encoded_len = encoded_len
            .checked_add(fragment.as_slice().len())
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "encoded response size overflow",
                )
            })?;
        // Timers and cancellation need a poll point even when no IO occurs yet.
        if index % 1024 == 1023 {
            tokio::task::yield_now().await;
        }
    }
    fragments.rewind(value);
    let mut frame = Vec::new();
    frame
        .try_reserve_exact(encoded_len.min(MAX_CHUNK) + 4)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::OutOfMemory, error))?;
    frame.extend_from_slice(&[0, 0]);
    for (index, fragment) in fragments.enumerate() {
        let fragment = fragment
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
        let mut data = fragment.as_slice();
        while !data.is_empty() {
            let take = data.len().min(MAX_CHUNK + 2 - frame.len());
            frame.extend_from_slice(&data[..take]);
            data = &data[take..];
            if frame.len() == MAX_CHUNK + 2 {
                frame[..2].copy_from_slice(&(MAX_CHUNK as u16).to_be_bytes());
                stream.write_all(&frame).await?;
                tokio::task::yield_now().await;
                frame.clear();
                frame.extend_from_slice(&[0, 0]);
            }
        }
        if index % 1024 == 1023 {
            tokio::task::yield_now().await;
        }
    }
    let payload_len = frame.len() - 2;
    if payload_len != 0 {
        frame[..2].copy_from_slice(&(payload_len as u16).to_be_bytes());
        frame.extend_from_slice(&[0, 0]);
    }
    stream.write_all(&frame).await
}

#[derive(Default)]
pub struct ChunkReader {
    buffer: Vec<u8>,
    offset: usize,
    current: Vec<u8>,
    /// Capacity charged to the current message (recovery may bypass admission).
    current_admitted: usize,
    /// Skipping the remaining chunks of a refused message, up to its terminator.
    discarding: bool,
    /// The previous message was refused: admit one small recovery message.
    recovering: bool,
}

impl ChunkReader {
    /// Allow the same bounded recovery message after a decode-time refusal.
    pub(crate) fn allow_recovery_message(&mut self) {
        self.recovering = true;
    }

    pub fn push(&mut self, bytes: &[u8]) {
        if self.offset != 0 {
            // Compact once per incoming read, rather than per chunk or NOOP.
            self.buffer.drain(..self.offset);
            self.offset = 0;
        }
        self.buffer.extend_from_slice(bytes);
    }

    pub fn next_message(&mut self) -> Option<Vec<u8>> {
        self.next_message_with_limit(usize::MAX)
            .expect("message payload fits in memory")
    }

    /// Counts assembled payload bytes only, excluding chunk headers and NOOPs.
    /// An oversized announcement is rejected before waiting for its chunk body.
    pub fn next_message_with_limit(
        &mut self,
        max_message_bytes: usize,
    ) -> Result<Option<Vec<u8>>, MessageTooLarge> {
        self.next_message_admitted(max_message_bytes, None)
            .map_err(|_| MessageTooLarge)
    }

    pub(crate) fn next_message_admitted(
        &mut self,
        max_message_bytes: usize,
        mut lease: Option<&mut IngressLease>,
    ) -> Result<Option<Vec<u8>>, ReadError> {
        loop {
            let remaining = &self.buffer[self.offset..];
            if remaining.len() < 2 {
                return Ok(None);
            }
            let size = u16::from_be_bytes([remaining[0], remaining[1]]) as usize;
            if self.discarding {
                if remaining.len() < 2 + size {
                    return Ok(None);
                }
                self.offset += 2 + size;
                self.discarding = size != 0;
                continue;
            }
            if self
                .current
                .len()
                .checked_add(size)
                .is_none_or(|total| total > max_message_bytes)
            {
                return Err(ReadError::TooLarge);
            }
            if size == 0 {
                self.offset += 2;
                if self.current.is_empty() {
                    continue; // NOOP
                }
                self.recovering = false;
                self.current_admitted = 0;
                return Ok(Some(std::mem::take(&mut self.current)));
            }
            if remaining.len() < 2 + size {
                return Ok(None);
            }
            let required = self.current.len() + size;
            let capacity_before_growth = self.current.capacity();
            let growing = required > capacity_before_growth;
            let growth_limit = if self.recovering && required <= CONTROL_MESSAGE_BYTES {
                max_message_bytes.min(CONTROL_MESSAGE_BYTES)
            } else {
                max_message_bytes
            };
            let mut capacity = if growing {
                required
                    .max(self.current.capacity().saturating_mul(2))
                    .min(growth_limit)
            } else {
                self.current.capacity()
            };
            // The old and replacement allocations may coexist during realloc.
            let extra = if growing {
                capacity
            } else {
                capacity.saturating_sub(self.current_admitted)
            };
            let mut charged = false;
            let mut refusal = None;
            if let Some(lease) = lease.as_deref_mut() {
                match lease.grow(extra) {
                    Ok(()) => charged = true,
                    Err(_) if self.recovering && required <= CONTROL_MESSAGE_BYTES => {
                        // Allocate the whole recovery allowance once: repeated
                        // exempt reallocations could overlap two uncharged buffers.
                        capacity = max_message_bytes.min(CONTROL_MESSAGE_BYTES);
                        let overlap = self
                            .current
                            .capacity()
                            .saturating_sub(self.current_admitted);
                        if growing && overlap.saturating_add(capacity) > CONTROL_MESSAGE_BYTES {
                            refusal = Some(lease.hint(extra));
                        }
                    }
                    Err(_) => refusal = Some(lease.hint(extra)),
                }
            }
            if refusal.is_none() && growing {
                if self
                    .current
                    .try_reserve_exact(capacity - self.current.len())
                    .is_err()
                {
                    if charged {
                        lease.as_deref_mut().unwrap().release(extra);
                    }
                    refusal = Some((capacity as u64, 0));
                } else {
                    let actual = self.current.capacity();
                    if charged {
                        // Allocators may grant more capacity than requested.
                        let lease = lease.as_deref_mut().unwrap();
                        let overshoot = actual.saturating_sub(capacity);
                        if lease.grow(overshoot).is_err() {
                            // Keep the replacement charged until it is discarded.
                            refusal = Some(lease.hint(overshoot));
                        } else {
                            lease.release(self.current_admitted);
                            self.current_admitted = actual;
                        }
                    } else if lease.is_some() {
                        let overlap = capacity_before_growth.saturating_sub(self.current_admitted);
                        if overlap.saturating_add(actual) > CONTROL_MESSAGE_BYTES {
                            refusal = Some((actual as u64, 0));
                        }
                    }
                }
            } else if charged {
                self.current_admitted = capacity;
            }
            if let Some((requested, available)) = refusal {
                self.current = Vec::new();
                self.current_admitted = 0;
                self.offset += 2 + size;
                self.discarding = true;
                self.recovering = true;
                return Err(ReadError::IngressExhausted {
                    requested,
                    available,
                });
            }
            self.current.extend_from_slice(&remaining[2..2 + size]);
            self.offset += 2 + size;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct RecordingWriter {
        bytes: Vec<u8>,
        largest_write: usize,
        partial: Option<usize>,
        stop_after: Option<usize>,
        fail: bool,
    }

    impl tokio::io::AsyncWrite for RecordingWriter {
        fn poll_write(
            mut self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
            bytes: &[u8],
        ) -> std::task::Poll<std::io::Result<usize>> {
            self.largest_write = self.largest_write.max(bytes.len());
            let available = self
                .stop_after
                .map_or(usize::MAX, |limit| limit.saturating_sub(self.bytes.len()));
            if available == 0 && self.fail {
                return std::task::Poll::Ready(Err(std::io::ErrorKind::BrokenPipe.into()));
            }
            let size = bytes
                .len()
                .min(self.partial.unwrap_or(usize::MAX))
                .min(available);
            self.bytes.extend_from_slice(&bytes[..size]);
            std::task::Poll::Ready(Ok(size))
        }

        fn poll_flush(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }

        fn poll_shutdown(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }
    }

    #[tokio::test]
    async fn streamed_framing_matches_wire_bytes_and_bounds_each_write() {
        for size in [
            0,
            1,
            MAX_CHUNK - 1,
            MAX_CHUNK,
            MAX_CHUNK + 1,
            MAX_CHUNK * 3 + 17,
        ] {
            let payload: Vec<_> = (0..size).map(|n| (n % 251) as u8).collect();
            for partial in [None, Some(7)] {
                let mut writer = RecordingWriter {
                    partial,
                    ..Default::default()
                };
                write_message(&mut writer, &payload).await.unwrap();
                assert_eq!(
                    writer.bytes,
                    encode_message(&payload),
                    "size={size}, partial={partial:?}"
                );
                assert!(
                    writer.largest_write <= MAX_CHUNK + 4,
                    "one bounded frame per write"
                );
            }
        }
    }

    #[tokio::test]
    async fn packstream_preflight_can_be_cancelled_before_large_output() {
        use crate::packstream::{pack, PackValue};
        use std::future::Future;
        use std::task::Poll;
        let value = PackValue::List(vec![PackValue::Null; 10_000]);
        let mut writer = RecordingWriter::default();
        let mut sending = Box::pin(write_packstream_message(&mut writer, &value));
        let first = std::future::poll_fn(|cx| Poll::Ready(sending.as_mut().poll(cx))).await;
        assert!(
            first.is_pending(),
            "large preflight must let cancellation and timers run"
        );
        drop(sending);
        assert!(
            writer.bytes.is_empty(),
            "cancelled preflight must not send a partial message"
        );
        write_packstream_message(&mut writer, &value).await.unwrap();
        let mut payload = Vec::new();
        pack(&value, &mut payload);
        assert_eq!(writer.bytes, encode_message(&payload));
    }

    #[tokio::test]
    async fn large_packstream_validation_and_encoding_yield_safely() {
        use crate::packstream::PackValue;
        use std::future::Future;
        use std::task::Poll;
        let mut values = vec![PackValue::Null; 10_000];
        values.push(PackValue::Struct {
            tag: 0x71,
            fields: vec![PackValue::Null; 16],
        });
        let mut writer = RecordingWriter::default();
        assert_eq!(
            write_packstream_message(&mut writer, &PackValue::List(values))
                .await
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::InvalidData
        );
        assert!(
            writer.bytes.is_empty(),
            "late validation failure sent output"
        );
        let value = PackValue::String("x".repeat(MAX_CHUNK * 4));
        let mut sending = Box::pin(write_packstream_message(&mut writer, &value));
        let first = std::future::poll_fn(|cx| Poll::Ready(sending.as_mut().poll(cx))).await;
        assert!(first.is_pending(), "ready writer monopolized serialization");
        drop(sending);
        // Cancellation after framing starts leaves a partial message: close the connection.
        assert!(!writer.bytes.is_empty());
        assert!(writer.bytes.len() <= MAX_CHUNK + 2);
    }

    #[tokio::test]
    async fn packstream_writes_match_existing_encoding_with_bounded_frames() {
        use crate::packstream::{pack, PackValue};
        let value = PackValue::Struct {
            tag: 0x71,
            fields: vec![PackValue::List(vec![
                PackValue::String("x".repeat(MAX_CHUNK * 4 + 17)),
                PackValue::Map(vec![("key".into(), PackValue::Int(i64::MIN))]),
            ])],
        };
        let mut payload = Vec::new();
        pack(&value, &mut payload);
        for partial in [None, Some(7)] {
            let mut writer = RecordingWriter {
                partial,
                ..Default::default()
            };
            write_packstream_message(&mut writer, &value).await.unwrap();
            assert_eq!(writer.bytes, encode_message(&payload));
            assert!(writer.largest_write <= MAX_CHUNK + 4);
        }
    }

    #[tokio::test]
    async fn packstream_framing_matches_at_exact_encoded_chunk_boundaries() {
        use crate::packstream::{pack, PackValue};
        for encoded_len in [MAX_CHUNK - 1, MAX_CHUNK, MAX_CHUNK + 1] {
            // These strings use a three-byte PackStream length header.
            let value = PackValue::String("x".repeat(encoded_len - 3));
            let mut payload = Vec::new();
            pack(&value, &mut payload);
            assert_eq!(payload.len(), encoded_len);
            for partial in [None, Some(7)] {
                let mut writer = RecordingWriter {
                    partial,
                    ..Default::default()
                };
                write_packstream_message(&mut writer, &value).await.unwrap();
                assert_eq!(
                    writer.bytes,
                    encode_message(&payload),
                    "encoded_len={encoded_len}, partial={partial:?}"
                );
                assert!(writer.largest_write <= MAX_CHUNK + 4);
            }
        }
    }

    #[tokio::test]
    async fn packstream_preflight_rejects_invalid_output_without_writing() {
        use crate::packstream::{PackValue, MAX_DEPTH, MAX_ENCODE_DEPTH};
        let mut writer = RecordingWriter::default();
        let invalid = PackValue::Struct {
            tag: 0x71,
            fields: vec![PackValue::Null; 16],
        };
        assert_eq!(
            write_packstream_message(&mut writer, &invalid)
                .await
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::InvalidData
        );
        assert!(writer.bytes.is_empty());
        let mut nested = PackValue::Null;
        for _ in 0..MAX_DEPTH + 4 {
            nested = PackValue::List(vec![nested]);
        }
        write_packstream_message(&mut writer, &nested)
            .await
            .unwrap();
        writer.bytes.clear();
        for _ in MAX_DEPTH + 4..MAX_ENCODE_DEPTH + 1 {
            nested = PackValue::List(vec![nested]);
        }
        // Put a full frame before the invalid value: preflight still emits nothing.
        let invalid = PackValue::List(vec![PackValue::String("x".repeat(MAX_CHUNK * 2)), nested]);
        assert_eq!(
            write_packstream_message(&mut writer, &invalid)
                .await
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::InvalidData
        );
        assert!(writer.bytes.is_empty());
    }

    #[tokio::test]
    async fn packstream_writer_honors_backpressure_cancellation_and_io_failures() {
        use crate::packstream::PackValue;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let value = PackValue::String("x".repeat(MAX_CHUNK * 4));
        let (mut writer, mut reader) = tokio::io::duplex(32);
        let timeout = tokio::time::timeout(
            std::time::Duration::from_millis(10),
            write_packstream_message(&mut writer, &value),
        )
        .await;
        assert!(timeout.is_err());
        writer.shutdown().await.unwrap();
        let mut partial = Vec::new();
        reader.read_to_end(&mut partial).await.unwrap();
        assert_eq!(partial.len(), 32);
        for fail in [false, true] {
            let mut writer = RecordingWriter {
                stop_after: Some(MAX_CHUNK + 9),
                fail,
                ..Default::default()
            };
            let error = write_packstream_message(&mut writer, &value)
                .await
                .unwrap_err();
            assert_eq!(
                error.kind(),
                if fail {
                    std::io::ErrorKind::BrokenPipe
                } else {
                    std::io::ErrorKind::WriteZero
                }
            );
            assert_eq!(writer.bytes.len(), MAX_CHUNK + 9);
        }
    }

    #[tokio::test]
    async fn streamed_framing_propagates_write_zero_and_mid_message_failure() {
        let payload = vec![7; MAX_CHUNK * 2];
        for fail in [false, true] {
            let mut writer = RecordingWriter {
                stop_after: Some(MAX_CHUNK + 9),
                fail,
                ..Default::default()
            };
            let error = write_message(&mut writer, &payload).await.unwrap_err();
            assert_eq!(
                error.kind(),
                if fail {
                    std::io::ErrorKind::BrokenPipe
                } else {
                    std::io::ErrorKind::WriteZero
                }
            );
            assert_eq!(writer.bytes.len(), MAX_CHUNK + 9);
        }
    }

    #[test]
    fn fragmented_payload_rechecks_falling_process_headroom() {
        use crate::ingress::IngressPool;
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        };
        let available = Arc::new(AtomicUsize::new(100));
        let observed = Arc::clone(&available);
        let pool = IngressPool::with_process_headroom(
            1000,
            Some(Arc::new(move || observed.load(Ordering::Acquire))),
        );
        let mut lease = pool.lease();
        let mut reader = ChunkReader::default();
        reader.push(&[0, 4, 1, 2, 3, 4]);
        assert_eq!(
            reader.next_message_admitted(100, Some(&mut lease)),
            Ok(None)
        );
        assert_eq!(pool.reserved(), 4);
        available.store(0, Ordering::Release);
        reader.push(&[0, 1, 5]);
        assert!(matches!(
            reader.next_message_admitted(100, Some(&mut lease)),
            Err(ReadError::IngressExhausted { .. })
        ));
        assert_eq!(reader.current.capacity(), 0);
        drop(reader);
        drop(lease);
        assert_eq!(pool.reserved(), 0);
    }

    #[test]
    fn fragmented_payload_reserves_capacity_before_geometric_growth() {
        use crate::ingress::IngressPool;
        let pool = IngressPool::new(24);
        let mut lease = pool.lease();
        let mut reader = ChunkReader::default();
        reader.push(&[0, 16]);
        reader.push(&[7; 16]);
        assert_eq!(
            reader.next_message_admitted(100, Some(&mut lease)),
            Ok(None)
        );
        assert_eq!(pool.reserved(), 16);
        reader.push(&[0, 1, 8]);
        assert_eq!(
            reader.next_message_admitted(100, Some(&mut lease)),
            Err(ReadError::IngressExhausted {
                requested: 48,
                available: 24
            })
        );
        assert_eq!(reader.current.capacity(), 0);
        drop(lease);
        assert_eq!(pool.reserved(), 0);
    }

    #[test]
    fn fragmented_payload_releases_old_capacity_after_growth() {
        use crate::ingress::IngressPool;
        let pool = IngressPool::new(48);
        let mut lease = pool.lease();
        let mut reader = ChunkReader::default();
        reader.push(&[0, 16]);
        reader.push(&[7; 16]);
        assert_eq!(
            reader.next_message_admitted(100, Some(&mut lease)),
            Ok(None)
        );
        reader.push(&[0, 1, 8]);
        assert_eq!(
            reader.next_message_admitted(100, Some(&mut lease)),
            Ok(None)
        );
        assert_eq!(reader.current.capacity(), 32);
        assert_eq!(pool.reserved(), 32);
        reader.push(&[0, 0]);
        let payload = reader
            .next_message_admitted(100, Some(&mut lease))
            .unwrap()
            .unwrap();
        assert_eq!(payload.len(), 17);
        assert_eq!(reader.current_admitted, 0);
        assert_eq!(pool.reserved(), payload.capacity());
        drop(payload);
        drop(lease);
        assert_eq!(pool.reserved(), 0);
    }

    #[test]
    fn one_byte_chunks_use_geometric_capacity_and_account_all_of_it() {
        use crate::ingress::IngressPool;
        let pool = IngressPool::new(4096);
        let mut lease = pool.lease();
        let mut reader = ChunkReader::default();
        let mut growths = 0;
        let mut previous = 0;
        for _ in 0..1000 {
            reader.push(&[0, 1, 7]);
            assert_eq!(
                reader.next_message_admitted(2048, Some(&mut lease)),
                Ok(None)
            );
            assert_eq!(pool.reserved(), reader.current.capacity());
            if reader.current.capacity() != previous {
                growths += 1;
                previous = reader.current.capacity();
            }
        }
        assert!(
            growths <= 11,
            "one-byte chunks must not reallocate every byte"
        );
        assert_eq!(reader.current.capacity(), 1024);
    }

    #[test]
    fn fragmented_recovery_caps_geometric_slack_at_one_kibibyte() {
        use crate::ingress::IngressPool;
        let pool = IngressPool::new(0);
        let mut lease = pool.lease();
        let mut reader = ChunkReader::default();
        reader.allow_recovery_message();
        reader.push(&[3, 0]);
        reader.push(&[7; 768]);
        assert_eq!(
            reader.next_message_admitted(4096, Some(&mut lease)),
            Ok(None)
        );
        reader.push(&[0, 1, 7]);
        assert_eq!(
            reader.next_message_admitted(4096, Some(&mut lease)),
            Ok(None)
        );
        assert_eq!(reader.current.capacity(), CONTROL_MESSAGE_BYTES);
        reader.push(&[0, 255]);
        reader.push(&[7; 255]);
        assert_eq!(
            reader.next_message_admitted(4096, Some(&mut lease)),
            Ok(None)
        );
        assert_eq!(reader.current.len(), CONTROL_MESSAGE_BYTES);
        assert_eq!(pool.reserved(), 0);
        reader.push(&[0, 1, 7]);
        assert!(matches!(
            reader.next_message_admitted(4096, Some(&mut lease)),
            Err(ReadError::IngressExhausted { .. })
        ));
        assert_eq!(reader.current.capacity(), 0);
        assert_eq!(pool.reserved(), 0);
    }

    #[test]
    fn partially_admitted_recovery_can_bypass_growth_then_resume_admission() {
        use crate::ingress::IngressPool;
        let pool = IngressPool::new(1536);
        let mut lease = pool.lease();
        let mut reader = ChunkReader::default();
        reader.allow_recovery_message();
        reader.push(&[2, 0]);
        reader.push(&[7; 512]);
        assert_eq!(
            reader.next_message_admitted(4096, Some(&mut lease)),
            Ok(None)
        );
        assert_eq!(reader.current_admitted, 512);
        let mut occupied = pool.lease();
        occupied.grow(1024).unwrap();
        reader.push(&[0, 1, 7]);
        assert_eq!(
            reader.next_message_admitted(4096, Some(&mut lease)),
            Ok(None)
        );
        assert_eq!(reader.current.capacity(), CONTROL_MESSAGE_BYTES);
        assert_eq!(reader.current_admitted, 512);
        assert_eq!(pool.reserved(), 1536);
        drop(occupied);
        reader.push(&[0, 1, 7]);
        assert_eq!(
            reader.next_message_admitted(4096, Some(&mut lease)),
            Ok(None)
        );
        assert_eq!(reader.current_admitted, CONTROL_MESSAGE_BYTES);
        assert_eq!(pool.reserved(), CONTROL_MESSAGE_BYTES);
        reader.push(&[0, 0]);
        let payload = reader
            .next_message_admitted(4096, Some(&mut lease))
            .unwrap()
            .unwrap();
        assert_eq!(payload.len(), 514);
        drop(payload);
        drop(lease);
        assert_eq!(pool.reserved(), 0);
    }

    #[test]
    fn recovery_capacity_is_bounded_and_can_become_admitted_without_underflow() {
        use crate::ingress::IngressPool;
        let pool = IngressPool::new(2048);
        let mut occupied = pool.lease();
        occupied.grow(2048).unwrap();
        let mut lease = pool.lease();
        let mut reader = ChunkReader::default();
        reader.allow_recovery_message();
        reader.push(&[0, 16]);
        reader.push(&[7; 16]);
        assert_eq!(
            reader.next_message_admitted(4096, Some(&mut lease)),
            Ok(None)
        );
        assert_eq!(reader.current_admitted, 0);
        drop(occupied);
        reader.push(&[0, 1, 8]);
        assert_eq!(
            reader.next_message_admitted(4096, Some(&mut lease)),
            Ok(None)
        );
        assert_eq!(pool.reserved(), CONTROL_MESSAGE_BYTES);
        assert_eq!(reader.current_admitted, CONTROL_MESSAGE_BYTES);
        let mut occupied = pool.lease();
        occupied.grow(1024).unwrap();
        reader.push(&[4, 0]);
        reader.push(&[7; 1024]);
        assert!(matches!(
            reader.next_message_admitted(4096, Some(&mut lease)),
            Err(ReadError::IngressExhausted { .. })
        ));
        drop(lease);
        assert_eq!(pool.reserved(), 1024);
        drop(occupied);
        assert_eq!(pool.reserved(), 0);
    }

    #[test]
    fn shared_payload_admission_is_before_assembly_and_fragment_safe() {
        use crate::ingress::IngressPool;
        let pool = IngressPool::new(4);
        let mut a = ChunkReader::default();
        let mut b = ChunkReader::default();
        let mut al = pool.lease();
        let mut bl = pool.lease();
        a.push(&[0, 3, 1]);
        assert_eq!(a.next_message_admitted(10, Some(&mut al)).unwrap(), None);
        assert_eq!(pool.reserved(), 0);
        a.push(&[2, 3]);
        assert_eq!(a.next_message_admitted(10, Some(&mut al)).unwrap(), None);
        assert_eq!(pool.reserved(), 3);
        assert_eq!(a.next_message_admitted(10, Some(&mut al)).unwrap(), None);
        assert_eq!(pool.reserved(), 3);
        b.push(&[0, 2, 4, 5, 0, 1, 6, 0, 0]);
        assert_eq!(
            b.next_message_admitted(10, Some(&mut bl)),
            Err(ReadError::IngressExhausted {
                requested: 2,
                available: 1
            })
        );
        assert_eq!(pool.reserved(), 3);
        // The refused message is skipped to its terminator, nothing is copied.
        assert_eq!(b.next_message_admitted(10, Some(&mut bl)).unwrap(), None);
        assert_eq!(pool.reserved(), 3);
        // One small recovery message (RESET) passes while the pool is still full.
        b.push(&[0, 2, 7, 8, 0, 0]);
        assert_eq!(
            b.next_message_admitted(10, Some(&mut bl)).unwrap(),
            Some(vec![7, 8])
        );
        // The grace is spent: the next one waits for the pool again.
        b.push(&[0, 2, 9, 9, 0, 0]);
        assert!(b.next_message_admitted(10, Some(&mut bl)).is_err());
        drop(al);
        drop(bl);
        assert_eq!(pool.reserved(), 0);
    }

    #[test]
    fn small_message_is_one_chunk_and_a_terminator() {
        assert_eq!(encode_message(&[1, 2, 3]), vec![0, 3, 1, 2, 3, 0, 0]);
    }

    #[test]
    fn big_message_is_split_at_65535() {
        let payload = vec![7u8; 70_000];
        let framed = encode_message(&payload);
        assert_eq!(&framed[..2], &[0xFF, 0xFF]);
        let mut reader = ChunkReader::default();
        reader.push(&framed);
        assert_eq!(reader.next_message().unwrap(), payload);
    }

    #[test]
    fn reader_waits_for_the_terminator_and_skips_noop_chunks() {
        let mut reader = ChunkReader::default();
        reader.push(&[0, 0]); // a NOOP chunk between messages
        reader.push(&[0, 2, 9]);
        assert_eq!(reader.next_message(), None);
        reader.push(&[8, 0, 0]);
        assert_eq!(reader.next_message(), Some(vec![9, 8]));
        assert_eq!(reader.next_message(), None);
    }

    #[test]
    fn bounded_reader_accepts_fragmented_exact_boundary_with_noops() {
        let mut reader = ChunkReader::default();
        let framed = [0, 0, 0, 2, 9, 8, 0, 1, 7, 0, 0, 0, 0];
        let mut messages = Vec::new();
        for byte in framed {
            reader.push(&[byte]);
            if let Some(message) = reader.next_message_with_limit(3).unwrap() {
                messages.push(message);
            }
        }
        assert_eq!(messages, vec![vec![9, 8, 7]]);
        assert_eq!(reader.next_message_with_limit(3), Ok(None));
        reader.push(&[0, 0, 0, 0]);
        assert_eq!(reader.next_message_with_limit(0), Ok(None));
    }

    #[test]
    fn bounded_reader_rejects_announced_payload_before_accepting_chunk_body() {
        let mut reader = ChunkReader::default();
        reader.push(&[0, 2, 9, 8, 0]);
        assert_eq!(reader.next_message_with_limit(3), Ok(None));
        reader.push(&[2]);
        assert_eq!(reader.next_message_with_limit(3), Err(MessageTooLarge));
        assert_eq!(reader.current, vec![9, 8]);
        assert_eq!(&reader.buffer[reader.offset..], &[0, 2]);
    }

    #[test]
    fn many_noops_and_coalesced_messages_consume_without_shifting_the_buffer() {
        let mut bytes = vec![0; 64 * 1024];
        for _ in 0..1000 {
            bytes.extend_from_slice(&encode_message(&[7]));
        }
        let mut reader = ChunkReader::default();
        reader.push(&bytes);
        for _ in 0..1000 {
            assert_eq!(reader.next_message_with_limit(1), Ok(Some(vec![7])));
        }
        assert_eq!(reader.next_message_with_limit(1), Ok(None));
        assert_eq!(reader.offset, bytes.len());
        assert_eq!(
            reader.buffer, bytes,
            "consumption never shifts remaining bytes"
        );
    }

    #[test]
    fn compaction_preserves_a_fragmented_pending_chunk_and_message_payload() {
        let mut reader = ChunkReader::default();
        let mut bytes = encode_message(&[9]);
        bytes.extend_from_slice(&[0, 0, 0, 2, 1, 2, 0]);
        reader.push(&bytes);
        assert_eq!(reader.next_message_with_limit(3), Ok(Some(vec![9])));
        assert_eq!(reader.next_message_with_limit(3), Ok(None));
        reader.push(&[1]);
        assert_eq!(reader.buffer, vec![0, 1]);
        assert_eq!(reader.offset, 0);
        assert_eq!(reader.next_message_with_limit(3), Ok(None));
        reader.push(&[3, 0, 0]);
        assert_eq!(reader.next_message_with_limit(3), Ok(Some(vec![1, 2, 3])));
        assert_eq!(reader.next_message_with_limit(3), Ok(None));
    }
}
