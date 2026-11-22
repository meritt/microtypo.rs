/// The clock and the random bytes an engine uses; the core performs no I/O, so each binding supplies them.
///
/// ```
/// use std::sync::LazyLock;
/// use std::time::Instant;
///
/// use microtypo_core::Host;
///
/// static START: LazyLock<Instant> = LazyLock::new(Instant::now);
///
/// let host = Host {
///     now_ms: || START.elapsed().as_secs_f64() * 1000.0,
///     fill_random: |bytes| getrandom::fill(bytes).expect("the system CSPRNG is available"),
/// };
/// # let _ = host;
/// ```
#[derive(Debug, Clone, Copy)]
pub struct Host {
    /// Milliseconds on a clock that does not go back; `maxProcessingMs` is the difference of two readings.
    pub now_ms: fn() -> f64,
    /// Fills the bytes from a cryptographically secure generator, which the nonces of protected URLs, addresses and tokens come from so input cannot forge a placeholder. A generator that cannot deliver panics rather than return guessable bytes.
    pub fill_random: fn(&mut [u8]),
}
