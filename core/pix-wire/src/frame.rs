use crate::{MAX_ENCRYPTED_FRAME_BYTES, WireError};

const LENGTH_PREFIX_BYTES: usize = 4;

/// Adds a network-byte-order length prefix to one encrypted transport frame.
///
/// # Errors
///
/// Returns [`WireError`] when ciphertext is empty or exceeds the v1 frame limit.
pub fn encode_encrypted_frame(ciphertext: &[u8]) -> Result<Vec<u8>, WireError> {
    validate_ciphertext_size(ciphertext.len())?;
    let length =
        u32::try_from(ciphertext.len()).map_err(|_| WireError::FrameTooLarge(ciphertext.len()))?;
    let mut framed = Vec::with_capacity(LENGTH_PREFIX_BYTES + ciphertext.len());
    framed.extend_from_slice(&length.to_be_bytes());
    framed.extend_from_slice(ciphertext);
    Ok(framed)
}

/// Incrementally decodes length-prefixed encrypted frames from a byte stream.
#[derive(Debug, Default)]
pub struct EncryptedFrameDecoder {
    buffered: Vec<u8>,
    expected_length: Option<usize>,
}

impl EncryptedFrameDecoder {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            buffered: Vec::new(),
            expected_length: None,
        }
    }

    /// Appends stream bytes and returns every complete ciphertext frame.
    ///
    /// # Errors
    ///
    /// Returns [`WireError`] as soon as a prefix declares an empty or oversized
    /// frame. Incomplete valid frames remain buffered.
    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<Vec<u8>>, WireError> {
        self.buffered.extend_from_slice(bytes);
        let mut frames = Vec::new();
        loop {
            let expected = if let Some(expected) = self.expected_length {
                expected
            } else {
                if self.buffered.len() < LENGTH_PREFIX_BYTES {
                    break;
                }
                let prefix = [
                    self.buffered[0],
                    self.buffered[1],
                    self.buffered[2],
                    self.buffered[3],
                ];
                let expected = usize::try_from(u32::from_be_bytes(prefix))
                    .map_err(|_| WireError::FrameTooLarge(usize::MAX))?;
                validate_ciphertext_size(expected)?;
                self.buffered.drain(..LENGTH_PREFIX_BYTES);
                self.expected_length = Some(expected);
                expected
            };
            if self.buffered.len() < expected {
                break;
            }
            frames.push(self.buffered.drain(..expected).collect());
            self.expected_length = None;
        }
        Ok(frames)
    }

    #[must_use]
    pub fn has_partial_frame(&self) -> bool {
        self.expected_length.is_some() || !self.buffered.is_empty()
    }
}

fn validate_ciphertext_size(size: usize) -> Result<(), WireError> {
    if size == 0 {
        return Err(WireError::EmptyFrame);
    }
    if size > MAX_ENCRYPTED_FRAME_BYTES {
        return Err(WireError::FrameTooLarge(size));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{EncryptedFrameDecoder, encode_encrypted_frame};
    use crate::{MAX_ENCRYPTED_FRAME_BYTES, WireError};

    #[test]
    fn decodes_fragmented_and_coalesced_frames() {
        let first = encode_encrypted_frame(b"cipher-one").expect("first frame");
        let second = encode_encrypted_frame(b"cipher-two").expect("second frame");
        let joined = [first, second].concat();
        let mut decoder = EncryptedFrameDecoder::new();

        assert!(
            decoder
                .push(&joined[..3])
                .expect("prefix fragment")
                .is_empty()
        );
        let frames = decoder.push(&joined[3..]).expect("remaining bytes");
        assert_eq!(frames, vec![b"cipher-one".to_vec(), b"cipher-two".to_vec()]);
        assert!(!decoder.has_partial_frame());
    }

    #[test]
    fn rejects_declared_oversized_frame_before_payload_arrives() {
        let prefix = u32::try_from(MAX_ENCRYPTED_FRAME_BYTES + 1)
            .expect("limit fits u32")
            .to_be_bytes();
        let mut decoder = EncryptedFrameDecoder::new();
        assert!(matches!(
            decoder.push(&prefix),
            Err(WireError::FrameTooLarge(_))
        ));
    }
}
