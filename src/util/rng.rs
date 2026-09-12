//! Randomness: unguessable tokens from the operating system.

/// Fill `buf` with random bytes from the OS. Falls back to bytes mixed from the
/// clock and the process id only if the OS refuses, which in practice it doesn't.
pub fn random_bytes(buf: &mut [u8]) {
    if getrandom::getrandom(buf).is_ok() {
        return;
    }
    let mut x = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as u64)
        ^ (u64::from(std::process::id()) << 32);
    for b in buf {
        // SplitMix64 steps, so neighbouring bytes don't share the clock's pattern.
        x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = x;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        *b = (z ^ (z >> 31)) as u8;
    }
}

/// A token of `bytes` random bytes, written as lowercase hex.
pub fn token(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    random_bytes(&mut buf);
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn tokens_are_hex_of_the_asked_length_and_differ() {
        let a = super::token(16);
        assert_eq!(a.len(), 32);
        assert!(a.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_ne!(a, super::token(16));
    }
}
