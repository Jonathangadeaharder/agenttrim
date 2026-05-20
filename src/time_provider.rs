/// Abstraction for wall-clock time. Production uses `SystemTime`; tests use a frozen epoch.
pub trait TimeProvider: Send + Sync {
    fn now_secs(&self) -> i64;
}

/// Production time provider backed by `SystemTime::now()`.
pub struct SystemTimeProvider;

impl TimeProvider for SystemTimeProvider {
    fn now_secs(&self) -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64
    }
}

/// Test provider that returns a fixed timestamp.
pub struct FrozenTimeProvider(pub i64);

impl TimeProvider for FrozenTimeProvider {
    fn now_secs(&self) -> i64 {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_system_time_provider_returns_non_zero() {
        let p = SystemTimeProvider;
        assert!(p.now_secs() > 1_000_000_000, "epoch time should be > 2001");
    }

    #[test]
    fn test_frozen_time_provider_returns_fixed_value() {
        let p = FrozenTimeProvider(42);
        assert_eq!(p.now_secs(), 42);
    }

    #[test]
    fn test_frozen_time_provider_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<FrozenTimeProvider>();
    }
}
