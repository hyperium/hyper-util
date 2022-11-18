use crate::common::ready;
use pin_project_lite::pin_project;
use std::{
    pin::Pin,
    task::{Context, Poll},
};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

pin_project! {
    pub(crate) struct PrependAsyncRead<T1, T2> {
        #[pin]
        first: T1,
        #[pin]
        second: T2,
        state: State,
    }
}

impl<T1, T2> PrependAsyncRead<T1, T2> {
    pub(crate) fn new(first: T1, second: T2) -> Self {
        Self {
            first,
            second,
            state: State::First,
        }
    }
}

enum State {
    First,
    Second,
}

impl<T1: AsyncRead, T2: AsyncRead> AsyncRead for PrependAsyncRead<T1, T2> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let mut this = self.project();

        loop {
            match &this.state {
                State::First => {
                    let old_len = buf.filled().len();

                    match ready!(this.first.as_mut().poll_read(cx, buf)) {
                        Ok(()) => {
                            if buf.filled().len() == old_len {
                                *this.state = State::Second;
                            } else {
                                return Poll::Ready(Ok(()));
                            }
                        }
                        Err(e) => return Poll::Ready(Err(e)),
                    }
                }
                State::Second => return this.second.as_mut().poll_read(cx, buf),
            }
        }
    }
}

impl<T1, T2: AsyncWrite> AsyncWrite for PrependAsyncRead<T1, T2> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, std::io::Error>> {
        self.project().second.poll_write(cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), std::io::Error>> {
        self.project().second.poll_flush(cx)
    }

    fn poll_shutdown(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(), std::io::Error>> {
        self.project().second.poll_shutdown(cx)
    }
}
