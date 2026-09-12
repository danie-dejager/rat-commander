//! Randomness: unguessable tokens from the operating system, and a small fast
//! generator for things that only need to look random (the screensaver).

/// SplitMix64: tiny, fast, and statistically fine for animation. Not for
/// secrets — [`token`] is.
#[derive(Debug, Clone)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng(seed)
    }

    /// A generator seeded from the operating system.
    pub fn seeded() -> Self {
        let mut seed = [0u8; 8];
        random_bytes(&mut seed);
        Rng(u64::from_le_bytes(seed))
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A number in `0..n` (`n` must not be zero).
    pub fn below(&mut self, n: u64) -> u64 {
        self.next_u64() % n
    }

    /// A number in `[0, 1)`.
    pub fn unit(&mut self) -> f32 {
        (self.next_u64() >> 40) as f32 / (1u64 << 24) as f32
    }

    /// `true` with probability `p`.
    pub fn chance(&mut self, p: f32) -> bool {
        self.unit() < p
    }
}

/// Fill `buf` with random bytes from the OS. Falls back to bytes mixed from the
/// clock and the process id only if the OS refuses, which in practice it doesn't.
pub fn random_bytes(buf: &mut [u8]) {
    if getrandom::getrandom(buf).is_ok() {
        return;
    }
    let clock = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as u64);
    let mut rng = Rng::new(clock ^ (u64::from(std::process::id()) << 32));
    for b in buf {
        *b = rng.next_u64() as u8;
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
    use super::*;

    #[test]
    fn tokens_are_hex_of_the_asked_length_and_differ() {
        let a = token(16);
        assert_eq!(a.len(), 32);
        assert!(a.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_ne!(a, token(16));
    }

    #[test]
    fn a_seed_repeats_its_sequence_and_stays_in_range() {
        let (mut a, mut b) = (Rng::new(42), Rng::new(42));
        for _ in 0..1000 {
            assert_eq!(a.next_u64(), b.next_u64());
            assert!(a.below(7) < 7);
            let u = a.unit();
            assert!((0.0..1.0).contains(&u));
            b.below(7);
            b.unit();
        }
    }
}
