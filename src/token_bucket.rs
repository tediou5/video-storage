use std::time::Duration;
use tokio::time::{Instant, sleep};

pub(crate) struct TokenBucket {
    capacity: f64,
    tokens: f64,
    refill_rate: f64,
    last: Instant,
}

impl TokenBucket {
    pub(crate) fn new(capacity: f64, refill_rate: f64) -> Self {
        TokenBucket {
            capacity,
            tokens: capacity,
            refill_rate,
            last: Instant::now(),
        }
    }

    pub(crate) async fn consume(&mut self, amount: usize) {
        loop {
            let now = Instant::now();
            let elapsed = now.duration_since(self.last).as_secs_f64();
            self.tokens = (self.tokens + elapsed * self.refill_rate).min(self.capacity);
            self.last = now;
            if self.tokens >= amount as f64 {
                self.tokens -= amount as f64;
                break;
            }
            let need = amount as f64 - self.tokens;
            let wait = need / self.refill_rate;
            sleep(Duration::from_secs_f64(wait)).await;
        }
    }
}
