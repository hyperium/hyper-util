use hyper::body::Buf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::Notify;

#[derive(Clone)]
pub struct EosSignaler {
    notifier: Arc<Notify>,
}

impl EosSignaler {
    fn notify_eos(&self) {
        self.notifier.notify_waiters();
    }

    pub async fn wait_till_eos(self) {
        self.notifier.notified().await;
    }
}

pub struct AlertOnEos<B> {
    inner: B,
    signaler: EosSignaler,
    // It'd be better if we consumed the signaler, making it inaccessible after notification.
    // Unfortunately, that would require something like AtomicOption.
    // arc_swap::ArcSwapOption was tried, but it can only return an Arc, and the pointed-to value cannot be consumed.
    // One could write an AtomicOption type (like this https://docs.rs/atomic-option/0.1.2/atomic_option/),
    // but it requires both unsafe and heap allocation, which is not worth it.
    has_already_signaled: AtomicBool,
}

impl<B> AlertOnEos<B> {
    pub fn new(inner: B) -> (Self, EosSignaler) {
        let signal = EosSignaler {
            notifier: Arc::new(Notify::new()),
        };
        let this = Self {
            inner,
            signaler: signal.clone(),
            has_already_signaled: AtomicBool::new(false),
        };
        (this, signal)
    }
}

impl<B: Buf> Buf for AlertOnEos<B> {
    fn remaining(&self) -> usize {
        self.inner.remaining()
    }

    fn chunk(&self) -> &[u8] {
        self.inner.chunk()
    }

    fn advance(&mut self, cnt: usize) {
        self.inner.advance(cnt);
        if !self.inner.has_remaining() && !self.has_already_signaled.swap(true, Ordering::AcqRel) {
            self.signaler.notify_eos();
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::common::buf::AlertOnEos;
    use hyper::body::Bytes;
    use std::time::Duration;

    #[tokio::test]
    async fn test_get_notified() {
        let buf = Bytes::from_static(b"");
        let (_buf, signaler) = AlertOnEos::new(buf);
        let result = tokio::time::timeout(Duration::from_secs(1), signaler.wait_till_eos()).await;
        assert_eq!(result, Ok(()));
    }
}
