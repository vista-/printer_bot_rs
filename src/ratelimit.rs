use std::sync::Arc;
use std::time::Duration;

use brakes::{
    backend::local::Memory,
    types::{sliding_window::SlidingWindowCounter},
    RateLimiter,
};

pub struct MultiRateLimiter {
    limiter_5min: Arc<RateLimiter<SlidingWindowCounter, Memory>>, // Replace with your actual type
    limiter_1hour: Arc<RateLimiter<SlidingWindowCounter, Memory>>,
    limiter_1day: Arc<RateLimiter<SlidingWindowCounter, Memory>>,
}

impl MultiRateLimiter {
    pub fn new() -> Self {
        Self {
            limiter_5min: Arc::new(
                RateLimiter::builder()
                    .with_backend(Memory::new())
                    .with_limiter(SlidingWindowCounter::new(5, Duration::from_secs(5 * 60)))
                    .build()
            ),
            limiter_1hour: Arc::new(
                RateLimiter::builder()
                    .with_backend(Memory::new())
                    .with_limiter(SlidingWindowCounter::new(15, Duration::from_secs(60 * 60)))
                    .build()
            ),
            limiter_1day: Arc::new(
                RateLimiter::builder()
                    .with_backend(Memory::new())
                    .with_limiter(SlidingWindowCounter::new(30, Duration::from_secs(24 * 60 * 60)))
                    .build()
            ),
        }
    }
    
    // Check all limits - OR logic (block if ANY limit is exceeded)
    pub async fn check_rate_limit(&self, key: &str) -> Result<(), String> {
        // Check 5-minute limit
        if self.limiter_5min.is_ratelimited(key).is_err() {
            return Err("5-minute rate limit exceeded".to_string());
        }
        
        // Check hourly limit
        if self.limiter_1hour.is_ratelimited(key).is_err() {
            return Err("Hourly rate limit exceeded".to_string());
        }
        
        // Check daily limit
        if self.limiter_1day.is_ratelimited(key).is_err() {
            return Err("Daily rate limit exceeded".to_string());
        }
        
        Ok(())
    }

    pub fn get_usage(&self, key: &str) -> String {
        let usage_5min = self.limiter_5min.get_usage(key);
        let usage_1hour = self.limiter_1hour.get_usage(key);
        let usage_1day = self.limiter_1day.get_usage(key);
        
        format!(
            "5min: {:?}, 1hour: {:?}, 1day: {:?}",
            usage_5min, usage_1hour, usage_1day
        )
    }
}