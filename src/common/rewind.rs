use std::{cmp, io};

use bytes::{Buf, Bytes};
use hyper::rt::{Read, ReadBufCursor, Write};

use std::{
    pin::Pin,
    task::{self, Poll},
};

/// Combine a buffer with an IO, rewinding reads to use the buffer.
#[derive(Debug)]
pub(crate) struct Rewind<T> {
    pub(crate) pre: Option<Bytes>,
    pub(crate) inner: T,
}

impl<T> Rewind<T> {
    #[cfg(all(feature = "server", any(feature = "http1", feature = "http2")))]
    pub(crate) fn new_buffered(io: T, buf: Bytes) -> Self {
        Rewind {
            pre: Some(buf),
            inner: io,
        }
    }
}

impl<T> Read for Rewind<T>
where
    T: Read + Unpin,
{
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut task::Context<'_>,
        mut buf: ReadBufCursor<'_>,
    ) -> Poll<io::Result<()>> {
        if let Some(mut prefix) = self.pre.take() {
            // If there are no remaining bytes, let the bytes get dropped.
            if !prefix.is_empty() {
                let copy_len = cmp::min(prefix.len(), buf.remaining());
                buf.put_slice(&prefix[..copy_len]);
                prefix.advance(copy_len);
                // Put back what's left
                if !prefix.is_empty() {
                    self.pre = Some(prefix);
                }

                return Poll::Ready(Ok(()));
            }
        }
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl<T> Write for Rewind<T>
where
    T: Write + Unpin,
{
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut task::Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut task::Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write_vectored(cx, bufs)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut task::Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut task::Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }

    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored()
    }
}

#[cfg(test)]
mod tests {
    use super::Rewind;
    use bytes::Bytes;
    use std::cmp;
    use std::io;
    use std::pin::Pin;
    use std::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};
    use hyper::rt::{Read, ReadBuf, ReadBufCursor};

    struct MockIo(&'static [u8]);

    impl Read for MockIo {
        fn poll_read(
            mut self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            mut buf: ReadBufCursor<'_>,
        ) -> Poll<io::Result<()>> {
            let len = cmp::min(self.0.len(), buf.remaining());
            buf.put_slice(&self.0[..len]);
            self.0 = &self.0[len..];
            Poll::Ready(Ok(()))
        }
    }

    fn noop_cx() -> Context<'static> {
        static VTABLE: RawWakerVTable = RawWakerVTable::new(
            |_| RawWaker::new(std::ptr::null(), &VTABLE),
            |_| {},
            |_| {},
            |_| {},
        );
        let waker = unsafe { Waker::from_raw(RawWaker::new(std::ptr::null(), &VTABLE)) };
        Context::from_waker(&waker)
    }

    #[test]
    fn read_full_from_prebuf() {
        let mut rewind = Rewind {
            pre: Some(Bytes::from_static(b"hello")),
            inner: MockIo(b"world"),
        };

        let mut buf = [0u8; 5];
        let mut read_buf = ReadBuf::new(&mut buf);
        let res = Pin::new(&mut rewind).poll_read(&mut noop_cx(), read_buf.unfilled());
        assert!(res.is_ready());
        assert_eq!(read_buf.filled(), b"hello");
        assert!(rewind.pre.is_none());
    }

    #[test]
    fn read_partial_from_prebuf_leaves_remainder() {
        let mut rewind = Rewind {
            pre: Some(Bytes::from_static(b"hello")),
            inner: MockIo(b"world"),
        };

        let mut buf = [0u8; 3];
        let mut read_buf = ReadBuf::new(&mut buf);
        let res = Pin::new(&mut rewind).poll_read(&mut noop_cx(), read_buf.unfilled());
        assert!(res.is_ready());
        assert_eq!(read_buf.filled(), b"hel");
        assert_eq!(rewind.pre.as_ref().map(|b| &b[..]), Some(&b"lo"[..]));
    }

    #[test]
    fn read_exhausts_prebuf_then_reads_inner() {
        let mut rewind = Rewind {
            pre: Some(Bytes::from_static(b"hi")),
            inner: MockIo(b"world"),
        };

        let mut buf = [0u8; 2];
        let mut read_buf = ReadBuf::new(&mut buf);
        let res = Pin::new(&mut rewind).poll_read(&mut noop_cx(), read_buf.unfilled());
        assert!(res.is_ready());
        assert_eq!(read_buf.filled(), b"hi");
        assert!(rewind.pre.is_none());

        let mut buf = [0u8; 5];
        let mut read_buf = ReadBuf::new(&mut buf);
        let res = Pin::new(&mut rewind).poll_read(&mut noop_cx(), read_buf.unfilled());
        assert!(res.is_ready());
        assert_eq!(read_buf.filled(), b"world");
    }

    #[test]
    fn read_with_empty_prebuf_uses_inner() {
        let mut rewind = Rewind {
            pre: Some(Bytes::from_static(b"")),
            inner: MockIo(b"world"),
        };

        let mut buf = [0u8; 5];
        let mut read_buf = ReadBuf::new(&mut buf);
        let res = Pin::new(&mut rewind).poll_read(&mut noop_cx(), read_buf.unfilled());
        assert!(res.is_ready());
        assert_eq!(read_buf.filled(), b"world");
        assert!(rewind.pre.is_none());
    }
}
