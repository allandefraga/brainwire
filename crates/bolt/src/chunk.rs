//! Bolt chunk framing: each chunk is a 2-byte big-endian size and its data; a
//! message ends with a zero-size chunk. A zero-size chunk with no data before it
//! is a NOOP (keep-alive) and is skipped.

const MAX_CHUNK: usize = 0xFFFF;

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
    current: Vec<u8>,
}

impl ChunkReader {
    pub fn push(&mut self, bytes: &[u8]) {
        self.buffer.extend_from_slice(bytes);
    }

    pub fn next_message(&mut self) -> Option<Vec<u8>> {
        loop {
            if self.buffer.len() < 2 {
                return None;
            }
            let size = u16::from_be_bytes([self.buffer[0], self.buffer[1]]) as usize;
            if size == 0 {
                self.buffer.drain(..2);
                if self.current.is_empty() {
                    continue; // NOOP
                }
                return Some(std::mem::take(&mut self.current));
            }
            if self.buffer.len() < 2 + size {
                return None;
            }
            self.current.extend_from_slice(&self.buffer[2..2 + size]);
            self.buffer.drain(..2 + size);
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
}
