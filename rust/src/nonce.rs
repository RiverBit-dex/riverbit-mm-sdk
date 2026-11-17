//! Strictly increasing millisecond nonces.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::signing::Address;

/// A source of nonces: millisecond timestamps, strictly increasing and
/// unique even when many tasks ask at the same time.
///
/// The exchange deduplicates nonces per signer, so everything that signs with
/// one key in a process must share one source. [`NonceSource::for_signer`]
/// returns the shared one. Separate processes need a single owner per signer;
/// this is not a distributed allocator.
#[derive(Clone, Debug, Default)]
pub struct NonceSource(Arc<AtomicU64>);

impl NonceSource {
    /// A fresh, private source.
    pub fn new() -> Self {
        Self::default()
    }

    /// The process-wide source for `signer`. Calls with the same address
    /// return handles to the same counter.
    pub fn for_signer(signer: &Address) -> Self {
        static REGISTRY: OnceLock<Mutex<HashMap<Address, NonceSource>>> = OnceLock::new();
        let registry = REGISTRY.get_or_init(Default::default);
        let mut map = registry.lock().unwrap_or_else(|p| p.into_inner());
        map.entry(*signer).or_default().clone()
    }

    /// The next nonce: the current time in milliseconds, or one more than the
    /// last nonce handed out, whichever is larger.
    pub fn next(&self) -> u64 {
        self.next_at(now_ms())
    }

    /// Like [`next`](Self::next) with an explicit clock reading.
    pub fn next_at(&self, now_ms: u64) -> u64 {
        let mut prev = self.0.load(Ordering::Relaxed);
        loop {
            let candidate = now_ms.max(prev.saturating_add(1));
            match self
                .0
                .compare_exchange_weak(prev, candidate, Ordering::SeqCst, Ordering::Relaxed)
            {
                Ok(_) => return candidate,
                Err(actual) => prev = actual,
            }
        }
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn never_goes_backwards_or_repeats() {
        let n = NonceSource::new();
        assert_eq!(n.next_at(1000), 1000);
        assert_eq!(n.next_at(1000), 1001);
        assert_eq!(n.next_at(900), 1002);
        assert_eq!(n.next_at(5000), 5000);
    }

    #[test]
    fn unique_under_concurrency() {
        let n = NonceSource::new();
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let n = n.clone();
                std::thread::spawn(move || (0..2000).map(|_| n.next()).collect::<Vec<_>>())
            })
            .collect();
        let mut seen = HashSet::new();
        for t in threads {
            let values = t.join().unwrap();
            assert!(values.windows(2).all(|w| w[0] < w[1]), "per-thread order");
            for v in values {
                assert!(seen.insert(v), "duplicate nonce {v}");
            }
        }
        assert_eq!(seen.len(), 16000);
    }

    #[test]
    fn same_signer_shares_one_counter() {
        let addr = Address::from_bytes([0xab; 20]);
        let a = NonceSource::for_signer(&addr);
        let b = NonceSource::for_signer(&addr);
        let x = a.next_at(10_000_000_000_000);
        let y = b.next_at(1);
        assert!(y > x);
    }
}
