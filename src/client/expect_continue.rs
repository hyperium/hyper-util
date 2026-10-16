//! Client side `Expect: 100-Continue` support
//!
//! This module contains the `ExpectContinueBody` request body wrapper
//! and the `wrap` convenience helper for building it.

use std::{
    pin::Pin,
    task::{Context, Poll},
};

use futures_channel::oneshot;
use http::{HeaderValue, Request, StatusCode, header};
use http_body::{Body, Frame};
use hyper::rt::Sleep;
use pin_project_lite::pin_project;

pin_project! {
    /// ExpectContinueBody is a request body wrapper that withholds
    /// its data until the client is cleared to send.
    ///
    /// The body is cleared to send when a `100-continue` is received
    /// (delivered via hyper's `on_informational` hook) or the optional timeout elapses.
    ///
    /// HTTP/1.1 only. Use the `wrap` helper for building this conveniently.
    pub struct ExpectContinueBody<B> {
        #[pin]
        inner: B,
        signal: Option<oneshot::Receiver<()>>,
        sleep: Option<Pin<Box<dyn Sleep>>>,
        released: bool,
    }
}

impl<B> ExpectContinueBody<B> {
    pub(crate) fn new(
        inner: B,
        signal: oneshot::Receiver<()>,
        sleep: Option<Pin<Box<dyn Sleep>>>,
    ) -> Self {
        Self {
            inner,
            signal: Some(signal),
            sleep,
            released: false,
        }
    }
}

impl<B: Body> Body for ExpectContinueBody<B> {
    type Data = B::Data;
    type Error = B::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<B::Data>, B::Error>>> {
        let this = self.project();
        if !*this.released {
            if let Some(rx) = this.signal.as_mut() {
                if Pin::new(rx).poll(cx).is_ready() {
                    *this.released = true;
                }
            }

            if !*this.released {
                if let Some(sleep) = this.sleep.as_mut() {
                    if sleep.as_mut().poll(cx).is_ready() {
                        *this.released = true;
                    }
                }
            }

            if !*this.released {
                return Poll::Pending;
            }
        }

        this.inner.poll_frame(cx)
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> http_body::SizeHint {
        self.inner.size_hint()
    }
}

/// wrap returns a request whose body is withheld until 100/timeout
///
/// It adds `Expect: 100-continue` header to the request if absent.
/// Wraps the body in `ExpectContinueBody`.
/// Registers an `on_informational` hook that releases the body on a 100.
pub fn wrap<B>(
    req: Request<B>,
    sleep: Option<Pin<Box<dyn Sleep>>>,
) -> Request<ExpectContinueBody<B>> {
    let (tx, rx) = oneshot::channel();

    let (mut parts, body) = req.into_parts();
    parts
        .headers
        .entry(header::EXPECT)
        .or_insert(HeaderValue::from_static("100-continue"));

    let mut req = Request::from_parts(parts, ExpectContinueBody::new(body, rx, sleep));

    let tx = std::sync::Mutex::new(Some(tx));
    hyper::ext::on_informational(&mut req, move |res| {
        if res.status() == StatusCode::CONTINUE {
            if let Some(tx) = tx.lock().unwrap().take() {
                let _ = tx.send(());
            }
        }
    });

    req
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use bytes::Bytes;
    use futures_util::future::poll_fn;
    use http_body_util::{BodyExt, Full};

    use super::*;

    use crate::rt::TokioTimer;
    use hyper::rt::Timer;

    #[tokio::test]
    async fn withholds_body_until_signalled() {
        let (tx, rx) = oneshot::channel::<()>();
        let mut body = std::pin::pin!(ExpectContinueBody::new(
            Full::new(Bytes::from("hi")),
            rx,
            None,
        ));

        let first = poll_fn(|cx| Poll::Ready(body.as_mut().poll_frame(cx))).await;
        assert!(first.is_pending());

        tx.send(()).unwrap();

        let frame = body.frame().await.unwrap().unwrap();
        assert_eq!(frame.into_data().unwrap(), Bytes::from("hi"));
    }

    #[tokio::test(start_paused = true)]
    async fn release_body_with_timeout() {
        let (_tx, rx) = oneshot::channel::<()>();

        let sleep = TokioTimer.sleep(Duration::from_millis(100));
        let mut body = std::pin::pin!(ExpectContinueBody::new(
            Full::new(Bytes::from("hi")),
            rx,
            Some(sleep),
        ));

        let first = poll_fn(|cx| Poll::Ready(body.as_mut().poll_frame(cx))).await;
        assert!(first.is_pending());

        tokio::time::advance(Duration::from_millis(100)).await;

        let frame = body.frame().await.unwrap().unwrap();
        assert_eq!(frame.into_data().unwrap(), Bytes::from("hi"));
    }

    #[tokio::test]
    async fn release_on_signal_cancel() {
        let (tx, rx) = oneshot::channel::<()>();
        let mut body = std::pin::pin!(ExpectContinueBody::new(
            Full::new(Bytes::from("hi")),
            rx,
            None,
        ));

        let first = poll_fn(|cx| Poll::Ready(body.as_mut().poll_frame(cx))).await;
        assert!(first.is_pending());

        drop(tx);

        let frame = body.frame().await.unwrap().unwrap();
        assert_eq!(frame.into_data().unwrap(), Bytes::from("hi"));
    }
}
