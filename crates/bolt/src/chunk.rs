//! Bolt chunk framing: each chunk is a 2-byte big-endian size and its data; a
//! message ends with a zero-size chunk. A zero-size chunk with no data before it
//! is a NOOP (keep-alive) and is skipped.

const MAX_CHUNK: usize = 0xFFFF;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("message payload exceeds its size limit")]
pub struct MessageTooLarge;

pub fn encode_message(payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + 4 + 2 * (payload.len() / MAX_CHUNK));
    for chunk in payload.chunks(MAX_CHUNK) {
        out.extend_from_slice(&(chunk.len() as u16).to_be_bytes());
        out.extend_from_slice(chunk);
    }
    out.extend_from_slice(&[0, 0]);
    out
}

#[derive(Default)]
pub struct ChunkReader {
    buffer: Vec<u8>,
    offset: usize,
    current: Vec<u8>,
}

impl ChunkReader {
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
        loop {
            let remaining = &self.buffer[self.offset..];
            if remaining.len() < 2 {
                return Ok(None);
            }
            let size = u16::from_be_bytes([remaining[0], remaining[1]]) as usize;
            if self
                .current
                .len()
                .checked_add(size)
                .is_none_or(|total| total > max_message_bytes)
            {
                return Err(MessageTooLarge);
            }
            if size == 0 {
                self.offset += 2;
                if self.current.is_empty() {
                    continue; // NOOP
                }
                return Ok(Some(std::mem::take(&mut self.current)));
            }
            if remaining.len() < 2 + size {
                return Ok(None);
            }
            self.current.extend_from_slice(&remaining[2..2 + size]);
            self.offset += 2 + size;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
