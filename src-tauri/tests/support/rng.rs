//! Seeded chaos driver for the soak: reproducible operation sequences without
//! a dependency on rand.

/// Seeded, dependency-free. Reproduces the OPERATION SEQUENCE only — timing
/// races are not replayable (see the spec's residual risks).
pub struct XorShift64(u64);
impl XorShift64 {
    pub fn new(seed: u64) -> Self {
        Self(seed.max(1))
    }
    pub fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    pub fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}
