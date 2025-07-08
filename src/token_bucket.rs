use std::time::Duration;
use tokio::time::{Instant, sleep};
use tracing::trace;

pub(crate) struct TokenBucket {
    tokens: f64,
    refill_rate: f64,
    last: Instant,
}

impl TokenBucket {
    pub(crate) fn new(init_tokens: f64, refill_rate: f64) -> Self {
        TokenBucket {
            tokens: init_tokens,
            refill_rate,
            last: Instant::now(),
        }
    }

    pub(crate) async fn consume(&mut self, amount: usize) {
        if self.refill_rate == 0.0 {
            trace!("Token bucket refill rate is 0, skipping token consumption");
            return;
        }

        loop {
            let now = Instant::now();
            let elapsed = now.duration_since(self.last).as_secs_f64();
            self.last = now;
            self.tokens += elapsed * self.refill_rate;
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
