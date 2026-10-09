use std::sync::atomic::{AtomicI64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use plume_lite_protocol::{EPOCH_MS, Snowflake};

const SEQUENCE_BITS: u32 = 12;
const WORKER_BITS: u32 = 10;

/// Lock-free Snowflake generator.
///
/// The state packs `(milliseconds << 12) | sequence` in one atomic. Each call takes
/// `max(now, previous + 1)`: when the 4096 ids of a millisecond are exhausted (or the
/// clock goes backwards) it borrows the next millisecond instead of blocking, so ids
/// stay unique and strictly increasing per worker.
pub struct IdGenerator {
    worker_id: i64,
    state: AtomicI64,
}

impl IdGenerator {
    pub fn new(worker_id: u16) -> Self {
        assert!(i64::from(worker_id) < 1 << WORKER_BITS, "worker id out of range");
        Self { worker_id: i64::from(worker_id), state: AtomicI64::new(0) }
    }

    pub fn next(&self) -> Snowflake {
        let now = now_since_epoch_ms() << SEQUENCE_BITS;
        let mut previous = self.state.load(Ordering::Relaxed);
        loop {
            let next = now.max(previous + 1);
            match self.state.compare_exchange_weak(previous, next, Ordering::Relaxed, Ordering::Relaxed) {
                Ok(_) => {
                    let millis = next >> SEQUENCE_BITS;
                    let sequence = next & ((1 << SEQUENCE_BITS) - 1);
                    return Snowflake(
                        (millis << (WORKER_BITS + SEQUENCE_BITS)) | (self.worker_id << SEQUENCE_BITS) | sequence,
                    );
                }
                Err(actual) => previous = actual,
            }
        }
    }
}

fn now_since_epoch_ms() -> i64 {
    let unix_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0);
    (unix_ms - EPOCH_MS).max(0)
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::sync::Arc;

    use super::*;

    #[test]
    fn ids_are_unique_and_increasing_across_threads() {
        let ids = Arc::new(IdGenerator::new(3));
        let handles: Vec<_> = (0..4)
            .map(|_| {
                let ids = ids.clone();
                std::thread::spawn(move || {
                    let batch: Vec<_> = (0..20_000).map(|_| ids.next()).collect();
                    assert!(batch.windows(2).all(|w| w[0] < w[1]));
                    batch
                })
            })
            .collect();
        let mut all = HashSet::new();
        for handle in handles {
            for id in handle.join().unwrap() {
                assert!(all.insert(id), "duplicate id {id}");
            }
        }
        assert_eq!(all.len(), 80_000);
    }

    #[test]
    fn ids_embed_worker_and_current_time() {
        let id = IdGenerator::new(1023).next();
        assert_eq!((id.0 >> SEQUENCE_BITS) & 0x3FF, 1023);
        let drift = (id.timestamp_ms() - chrono::Utc::now().timestamp_millis()).abs();
        assert!(drift < 1000, "drift {drift}ms");
    }
}
