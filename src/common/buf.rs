use hyper::body::Buf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::Notify;

/// Signaler returned as part of `NotifyOnEos::new` that can be polled to receive information,
/// when the buffer gets advanced to the end.
// Cannot be Clone due to usage of `Notify::notify_one` in `NotifyOnEos::advance`,
// revisit once `Notify::notify_all` stabilizes.
pub struct EosSignaler {
    notifier: Arc<Notify>,
}

impl EosSignaler {
    pub async fn wait_till_eos(self) {
        self.notifier.notified().await;
    }
}

/// Wrapper for `bytes::Buf` that returns a `EosSignaler` that can be polled to receive information,
/// when the buffer gets advanced to the end.
///
/// NOTE: For the notification to work, caller must ensure that `Buf::advance` gets called
/// enough times to advance to the end of the buffer (so that `Buf::has_remaining` afterwards returns `0`).
pub struct NotifyOnEos<B> {
    inner: B,
    notifier: Arc<Notify>,
    // It'd be better if we consumed the signaler, making it inaccessible after notification.
    // Unfortunately, that would require something like AtomicOption.
    // arc_swap::ArcSwapOption was tried, but it can only return an Arc, and the pointed-to value cannot be consumed.
    // One could write an AtomicOption type (like this https://docs.rs/atomic-option/0.1.2/atomic_option/),
    // but it requires both unsafe and heap allocation, which is not worth it.
    has_already_signaled: AtomicBool,
}

impl<B> NotifyOnEos<B> {
    pub fn new(inner: B) -> (Self, EosSignaler) {
        let notifier = Arc::new(Notify::new());
        let this = Self {
            inner,
            notifier: notifier.clone(),
            has_already_signaled: AtomicBool::new(false),
        };
        let signal = EosSignaler { notifier };
        (this, signal)
    }
}

impl<B: Buf> Buf for NotifyOnEos<B> {
    fn remaining(&self) -> usize {
        self.inner.remaining()
    }

    fn chunk(&self) -> &[u8] {
        self.inner.chunk()
    }

    fn advance(&mut self, cnt: usize) {
        self.inner.advance(cnt);
        if !self.inner.has_remaining() && !self.has_already_signaled.swap(true, Ordering::AcqRel) {
            // tokio::sync::Notify has private method `notify_all` that, once stabilized,
            // would allow us to make `EosSignaler` Cloneable with better ergonomics
            // to await EOS from multiple places.
            self.notifier.notify_one();
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::common::buf::NotifyOnEos;
    use hyper::body::{Buf, Bytes};
    use std::time::Duration;

    #[tokio::test]
    async fn test_get_notified_immediately() {
        let buf = Bytes::from_static(b"abc");
        let (mut buf, signaler) = NotifyOnEos::new(buf);
        buf.advance(3);
        signaler.wait_till_eos().await;
    }

    #[tokio::test]
    async fn test_get_notified_after_1ms() {
        let buf = Bytes::from_static(b"abc");
        let (mut buf, signaler) = NotifyOnEos::new(buf);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(1)).await;
            buf.advance(3);
        });
        signaler.wait_till_eos().await;
    }
}
