use solana_program_log::logger::{Argument, Log};

/// Format the first 8 bytes of a 32-byte address as a 16-char hex string
/// suitable for use in `log!()` macro calls.
///
/// Returns a `HexBuf` which implements [`Log`] for direct use in `log!()`.
pub fn short_hex(addr: &[u8]) -> HexBuf {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut buf = [0u8; 16];
    let len = core::cmp::min(addr.len(), 8);
    for i in 0..len {
        buf[i * 2] = HEX[(addr[i] >> 4) as usize];
        buf[i * 2 + 1] = HEX[(addr[i] & 0x0f) as usize];
    }
    HexBuf(buf)
}

/// Fixed-size hex buffer that implements [`solana_program_log::Log`] for use in `log!()`.
pub struct HexBuf([u8; 16]);

impl HexBuf {
    /// Return the hex string as a `&str`.
    pub fn as_str(&self) -> &str {
        // SAFETY: All bytes are ASCII hex characters (0-9, a-f) which are valid UTF-8.
        unsafe { core::str::from_utf8_unchecked(&self.0) }
    }
}

// SAFETY: `write_with_args` delegates to the `&str` implementation which correctly
// reports the number of bytes written to the buffer.
unsafe impl Log for HexBuf {
    fn write_with_args(
        &self,
        buffer: &mut [core::mem::MaybeUninit<u8>],
        args: &[Argument],
    ) -> usize {
        self.as_str().write_with_args(buffer, args)
    }
}
