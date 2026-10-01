use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

#[derive(Clone)]
pub struct Shutdown {
    inner: Arc<(Mutex<bool>, Condvar)>,
}

impl Shutdown {
    pub fn new() -> Self {
        Self {
            inner: Arc::new((Mutex::new(false), Condvar::new())),
        }
    }

    pub fn stop(&self) {
        let (lock, cvar) = &*self.inner;
        *lock.lock().unwrap() = true;
        cvar.notify_all();
    }

    pub fn is_stopped(&self) -> bool {
        *self.inner.0.lock().unwrap()
    }

    pub fn wait_timeout(&self, timeout: Duration) -> bool {
        let (lock, cvar) = &*self.inner;
        let mut stopped = lock.lock().unwrap();
        let start = Instant::now();

        while !*stopped {
            let elapsed = start.elapsed();

            if elapsed >= timeout {
                return false;
            }

            let (guard, wait_result) = cvar.wait_timeout(stopped, timeout - elapsed).unwrap();

            stopped = guard;

            if wait_result.timed_out() {
                return *stopped;
            }
        }

        true
    }
}
