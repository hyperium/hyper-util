#![cfg(all(feature = "client-legacy", feature = "http1"))]

// -------------------------------------------------------
// Below is our custom code for testing hyper legacy-client behavior with mock connections for PR #184
// We use fully qualified paths for all types and identifiers to make this code
// copy/paste-able without relying on external 'use' statements. Detailed inline
// comments explain the purpose and logic of each section.

//XXX: can manually run like this:
// $ cargo test --features="http1,http2,server,client-legacy" --test legacy_client -- test_connection_error_propagation test_incomplete_message_error --nocapture
// $ cargo test --all-features --test legacy_client -- --nocapture
// $ cargo test --all-features --test legacy_client

use std::error::Error; // needed for .source() eg. error[E0599]: no method named `source` found for struct `hyper_util::client::legacy::Error` in the current scope

// Helper function to debug byte slices by attempting to interpret them as UTF-8.
// If the bytes are valid UTF-8, they are printed as a string; otherwise, they are
// printed as a raw byte array. This aids in debugging tokio_test::io::Mock mismatches.
fn debug_bytes(bytes: &[u8], label: &str) {
    // Try to convert the byte slice to a UTF-8 string.
    // If successful, print it with the provided label for context.
    if let Ok(s) = std::str::from_utf8(bytes) {
        eprintln!("{}: {}", label, s);
    } else {
        // If the bytes are not valid UTF-8, print them as a raw byte array.
        eprintln!("{}: {:?}", label, bytes);
    }
}

// Struct representing a mock connection for testing hyper client behavior.
// Implements hyper::rt::Read, hyper::rt::Write, and hyper_util::client::legacy::connect::Connection
// traits to simulate I/O operations. Uses tokio_test::io::Mock for controlled I/O behavior.
struct MockConnection {
    // The underlying mock I/O object, wrapped in hyper_util::rt::TokioIo for compatibility.
    inner: hyper_util::rt::TokioIo<tokio_test::io::Mock>,
    // Atomic flag to signal a connection failure, controlling poll_read behavior.
    failed: std::sync::Arc<std::sync::atomic::AtomicBool>,
    // The error to return when failed=true, simulating an I/O failure.
    error: std::sync::Arc<std::io::Error>,
    // Optional channel to signal unexpected writes, used for debugging.
    error_tx: Option<tokio::sync::mpsc::Sender<()>>,
    // Tracks total bytes written, for logging and verification.
    bytes_written: usize,
}

impl MockConnection {
    // Constructor for MockConnection, initializing all fields.
    // Takes a mock I/O object, failure flag, error, and optional error channel.
    fn new(
        mock: tokio_test::io::Mock,
        failed: std::sync::Arc<std::sync::atomic::AtomicBool>,
        error: std::sync::Arc<std::io::Error>,
        error_tx: Option<tokio::sync::mpsc::Sender<()>>,
    ) -> Self {
        MockConnection {
            inner: hyper_util::rt::TokioIo::new(mock),
            failed,
            error,
            error_tx,
            bytes_written: 0,
        }
    }
}

// Implement hyper::rt::Read trait to handle read operations on the mock connection.
// Controls whether an error or mock I/O data is returned based on the failed flag.
impl hyper::rt::Read for MockConnection {
    // Polls the connection for reading, filling the provided buffer.
    // If failed=true, returns the stored error; otherwise, delegates to the mock I/O.
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: hyper::rt::ReadBufCursor<'_>,
    ) -> std::task::Poll<std::result::Result<(), std::io::Error>> {
        // Log the current state of the failed flag for debugging.
        eprintln!(
            "poll_read: failed={}",
            self.failed.load(std::sync::atomic::Ordering::SeqCst)
        );
        // Check if the connection is marked as failed.
        // If true, return the stored error immediately to simulate a connection failure.
        if self.failed.load(std::sync::atomic::Ordering::SeqCst) {
            // Log the error being returned for traceability.
            eprintln!("poll_read: returning error: {}", self.error);
            // Create a new io::Error with the same kind and message as the stored error.
            return std::task::Poll::Ready(std::result::Result::Err(std::io::Error::new(
                self.error.kind(),
                self.error.to_string(),
            )));
        }
        // If not failed, delegate to the mock I/O to simulate normal read behavior.
        // This may return EOF (Poll::Ready(Ok(0))) for empty IoBuilder.
        let inner = std::pin::Pin::new(&mut self.inner);
        inner.poll_read(cx, buf)
    }
}

// Implement hyper::rt::Write trait to handle write operations on the mock connection.
// Logs writes and signals unexpected writes via error_tx.
impl hyper::rt::Write for MockConnection {
    // Polls the connection for writing, sending the provided buffer.
    // Logs the write operation and tracks total bytes written.
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::result::Result<usize, std::io::Error>> {
        // Log the size of the buffer being written for debugging.
        eprintln!("poll_write: {} bytes", buf.len());
        // Debug the buffer contents as UTF-8 or raw bytes.
        debug_bytes(buf, "poll_write buffer");
        // Delegate the write to the mock I/O object.
        let inner = std::pin::Pin::new(&mut self.inner);
        match inner.poll_write(cx, buf) {
            // If the write succeeds, update the bytes_written counter and log the result.
            std::task::Poll::Ready(std::result::Result::Ok(bytes)) => {
                // Increment the total bytes written for tracking.
                self.bytes_written += bytes;
                // Log the number of bytes written and the running total.
                eprintln!(
                    "poll_write: wrote {} bytes, total={}",
                    bytes, self.bytes_written
                );
                // If error_tx is present, signal an unexpected write (used in error tests).
                // This helps detect writes when the connection should fail early.
                if let Some(tx) = self.error_tx.take() {
                    // Log that an unexpected write is being signaled.
                    eprintln!("poll_write: signaling unexpected write");
                    // Send a message through the channel, ignoring errors if the receiver is closed.
                    let _ = tx.try_send(());
                }
                // Return the successful write result.
                std::task::Poll::Ready(std::result::Result::Ok(bytes))
            }
            // For pending or error results, propagate them directly.
            other => other,
        }
    }

    // Polls the connection to flush any buffered data.
    // Delegates to the mock I/O object.
    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::result::Result<(), std::io::Error>> {
        // Log the flush operation for debugging.
        eprintln!("poll_flush");
        // Delegate the flush to the mock I/O object.
        let inner = std::pin::Pin::new(&mut self.inner);
        inner.poll_flush(cx)
    }

    // Polls the connection to shut down the write side.
    // Delegates to the mock I/O object.
    fn poll_shutdown(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::result::Result<(), std::io::Error>> {
        // Log the shutdown operation for debugging.
        eprintln!("poll_shutdown");
        // Delegate the shutdown to the mock I/O object.
        let inner = std::pin::Pin::new(&mut self.inner);
        inner.poll_shutdown(cx)
    }
}

// Implement hyper_util::client::legacy::connect::Connection trait to provide connection metadata.
// Required for hyper to use MockConnection as a valid connection.
impl hyper_util::client::legacy::connect::Connection for MockConnection {
    // Returns metadata about the connection.
    // In this case, a default Connected object indicating a new connection.
    fn connected(&self) -> hyper_util::client::legacy::connect::Connected {
        hyper_util::client::legacy::connect::Connected::new()
    }
}

// Struct representing a mock connector for creating MockConnection instances.
// Implements tower_service::Service to integrate with hyper’s client.
#[derive(Clone)]
struct MockConnector {
    // The IoBuilder used to create mock I/O objects for each connection.
    io_builder: tokio_test::io::Builder,
    // Optional error to simulate a connection failure, passed to MockConnection.
    conn_error: Option<std::sync::Arc<std::io::Error>>,
}

impl MockConnector {
    // Constructor for MockConnector, initializing the IoBuilder and optional error.
    fn new(
        io_builder: tokio_test::io::Builder,
        conn_error: Option<std::sync::Arc<std::io::Error>>,
    ) -> Self {
        MockConnector {
            io_builder,
            conn_error,
        }
    }
}

// Implement tower_service::Service for MockConnector to create MockConnection instances.
// Takes a hyper::Uri and returns a future resolving to a MockConnection.
impl tower_service::Service<hyper::Uri> for MockConnector {
    type Response = crate::MockConnection;
    type Error = std::io::Error;
    type Future = std::pin::Pin<
        Box<
            dyn std::future::Future<Output = std::result::Result<Self::Response, Self::Error>>
                + Send,
        >,
    >;

    // Polls the connector to check if it’s ready to handle a request.
    // Always ready, as we don’t have resource constraints.
    fn poll_ready(
        &mut self,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::result::Result<(), Self::Error>> {
        std::task::Poll::Ready(std::result::Result::Ok(()))
    }

    // Creates a new MockConnection for the given URI.
    // Configures the connection based on io_builder and conn_error.
    fn call(&mut self, _req: hyper::Uri) -> Self::Future {
        // Clone the IoBuilder to create a fresh mock I/O object.
        let mut io_builder = self.io_builder.clone();
        // Clone the optional connection error for this call.
        let conn_error = self.conn_error.clone();
        // Return a pinned future that creates the MockConnection.
        Box::pin(async move {
            // Build the mock I/O object from the IoBuilder.
            // This defines the I/O behavior (e.g., EOF for empty builder).
            let mock = io_builder.build();
            // Create an atomic flag to track connection failure, initially false.
            let failed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            // Set the default error for non-failure cases.
            // Used when conn_error is None, simulating a clean EOF or connection close.
            let error = if let Some(ref err) = conn_error {
                err.clone()
            } else {
                std::sync::Arc::new(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "connection closed",
                ))
            };
            // Create an mpsc channel for signaling unexpected writes, if conn_error is set.
            // This helps debug cases where writes occur despite an expected failure.
            let error_tx = if conn_error.is_some() {
                // Create a channel with a buffer of 1 for signaling writes.
                let (tx, mut rx) = tokio::sync::mpsc::channel::<()>(1);
                // Spawn a task to log unexpected writes when received.
                tokio::spawn(async move {
                    // Wait for a message indicating a write occurred.
                    if rx.recv().await.is_some() {
                        // Log the unexpected write for debugging.
                        eprintln!("Unexpected write occurred");
                    }
                });
                Some(tx)
            } else {
                None
            };
            // If a connection error is provided, mark the connection as failed.
            // This causes poll_read to return the error immediately.
            if let Some(err_clone) = conn_error {
                // Set the failed flag to true atomically.
                failed.store(true, std::sync::atomic::Ordering::SeqCst);
                // Log the simulated error for traceability.
                eprintln!("Simulated conn task error: {}", err_clone);
            }
            // Create and return the MockConnection with all configured components.
            std::result::Result::Ok(crate::MockConnection::new(mock, failed, error, error_tx))
        })
    }
}

// Test for connection error propagation with PR #184.
// Simulates a connection failure by setting failed=true and returning a custom io::Error.
// Verifies the error propagates through hyper’s client as a hyper::Error(Io, ...).
#[cfg(feature = "http1")]
#[tokio::test]
async fn test_connection_error_propagation_pr184() {
    // Define the error message for the simulated connection failure.
    // Reused for creating the error and verifying the result.
    let err_str = "mock connection failure";
    // Create an io::Error with Other kind and the custom message.
    // Wrapped in Arc for sharing across threads and MockConnection.
    let io_error = std::sync::Arc::new(std::io::Error::new(std::io::ErrorKind::Other, err_str));
    // Create an empty IoBuilder, as no I/O is expected.
    // The error triggers before any reads or writes occur.
    let io_builder = tokio_test::io::Builder::new();
    // Create a MockConnector with the error to simulate a failed connection.
    // The error will set failed=true in MockConnection.
    let connector = crate::MockConnector::new(io_builder, Some(io_error.clone()));
    // Build the hyper client with TokioExecutor and our connector.
    // pool_max_idle_per_host(0) disables connection pooling for a fresh connection.
    let client = hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new())
        .pool_max_idle_per_host(0)
        .build::<_, http_body_util::Empty<hyper::body::Bytes>>(connector);
    // Build a GET request to a mock URI with custom headers.
    // Uses mixed-case headers to match your style, ensuring case-insensitive handling.
    let request = hyper::Request::builder()
        .uri("http://mocked")
        .header("hoSt", "mocked")
        .header("conNection", "close")
        .body(http_body_util::Empty::<hyper::body::Bytes>::new())
        .expect("failed to build request");
    // Send the request and capture the result.
    // Expect it to fail due to the simulated connection error.
    let result = client.request(request).await;
    // Extract the error, as the request should fail.
    let err = result.expect_err("expected request to fail");
    // Log the full error for debugging, including its structure.
    // Matches your detailed logging style for traceability.
    eprintln!("Actually gotten error is: {:?}", err);
    // Downcast the error to a hyper::Error to verify its type.
    // Expect a hyper::Error wrapping an io::Error from MockConnection.
    let hyper_err = err
        .source()
        .and_then(|e| e.downcast_ref::<hyper::Error>())
        .expect("expected hyper::Error");
    // Downcast the hyper::Error’s source to an io::Error.
    // Verify it matches the simulated error from MockConnection.
    let io_err = hyper_err
        .source()
        .and_then(|e| e.downcast_ref::<std::io::Error>())
        .expect(&format!("expected io::Error but got {:?}", hyper_err));
    // Verify the io::Error has the expected kind (Other).
    assert_eq!(io_err.kind(), std::io::ErrorKind::Other);
    // Verify the io::Error’s message matches err_str.
    assert_eq!(io_err.to_string(), err_str);
}

// Test for consistent IncompleteMessage error with or without PR #184.
// Simulates a connection that returns EOF immediately, causing hyper’s HTTP/1.1 parser
// to fail with IncompleteMessage due to no response data.
// Uses MockConnector with conn_error=None to keep failed=false, ensuring EOF behavior.
#[cfg(feature = "http1")]
#[tokio::test]
async fn test_incomplete_message_error_pr184() {
    // Create an empty IoBuilder to simulate a connection with no data.
    // No write or read expectations, so poll_read returns EOF (Poll::Ready(Ok(0))).
    // This triggers IncompleteMessage in hyper’s parser.
    let io_builder = tokio_test::io::Builder::new();
    // Create MockConnector with no error (conn_error=None).
    // Keeps failed=false in MockConnection, so poll_read delegates to the mock’s EOF.
    let connector = crate::MockConnector::new(io_builder, None);
    // Build the hyper client with TokioExecutor and our connector.
    // pool_max_idle_per_host(0) disables pooling for a fresh connection.
    let client = hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new())
        .pool_max_idle_per_host(0)
        .build::<_, http_body_util::Empty<hyper::body::Bytes>>(connector);
    // Build a GET request to a mock URI with headers.
    // Uses mixed-case headers to match test_connection_error_propagation_pr184.
    // Empty body ensures focus on response parsing failure.
    let request = hyper::Request::builder()
        .uri("http://mocked")
        .header("hoSt", "mocked")
        .header("conNection", "close")
        .body(http_body_util::Empty::<hyper::body::Bytes>::new())
        .expect("failed to build request");
    // Send the request and capture the result.
    // Expect failure due to EOF causing IncompleteMessage.
    let result = client.request(request).await;
    // Extract the error, as the request should fail.
    // Without PR #184, expect ChannelClosed; with PR #184, expect IncompleteMessage.
    let err = result.expect_err("expected request to fail");
    // Log the full error for debugging, matching your style.
    eprintln!("Actually gotten error is: {:?}", err);
    // Downcast to hyper::Error to verify the error type.
    // Expect IncompleteMessage (with PR #184) or ChannelClosed (without).
    let hyper_err = err
        .source()
        .and_then(|e| e.downcast_ref::<hyper::Error>())
        .expect("expected hyper::Error");
    // Verify the error is IncompleteMessage when PR #184 is applied.
    // This checks the parser’s failure due to EOF.
    assert!(
        hyper_err.is_incomplete_message(),
        "expected IncompleteMessage, got {:?}",
        hyper_err
    );
    // Confirm no io::Error is present, as this is a parsing failure, not I/O.
    // Ensures we’re testing the correct error type.
    assert!(
        hyper_err
            .source()
            .and_then(|e| e.downcast_ref::<std::io::Error>())
            .is_none(),
        "expected no io::Error, got {:?}",
        hyper_err
    );
}

// Test for a successful HTTP/1.1 connection using a mock connector.
// Simulates a server that accepts a request and responds with a 200 OK.
// Verifies the client correctly sends the request and receives the response.
#[cfg(feature = "http1")]
#[tokio::test]
async fn test_successful_connection() {
    // Define the expected server response: a valid HTTP/1.1 200 OK with no body.
    let response = b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n";
    // Define the expected client request, including headers and CRLF termination.
    // This ensures the client sends the correct request format.
    let expected_request = b"GET / HTTP/1.1\r\nhost: mocked\r\nconnection: close\r\n\r\n";
    // Create an IoBuilder to simulate the server’s I/O behavior.
    // Expect the client to write the request and read the response.
    let mut io_builder = tokio_test::io::Builder::new();
    // Configure the IoBuilder to expect the request and provide the response.
    io_builder.write(expected_request).read(response);
    // Finalize the IoBuilder for use in the connector.
    let io_builder = io_builder;
    // Create a MockConnector with no error (conn_error=None).
    // Ensures failed=false, allowing normal I/O operations.
    let connector = crate::MockConnector::new(io_builder, None);
    // Build the hyper client with TokioExecutor and our connector.
    // pool_max_idle_per_host(0) ensures a fresh connection.
    let client = hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new())
        .pool_max_idle_per_host(0)
        .build::<_, http_body_util::Empty<hyper::body::Bytes>>(connector);
    // Build a GET request to a mock URI with headers.
    // Uses mixed-case headers to match your style and verify case-insensitive handling.
    let request = hyper::Request::builder()
        .uri("http://mocked")
        .header("hOst", "mocked")
        .header("coNnection", "close")
        .body(http_body_util::Empty::<hyper::body::Bytes>::new())
        .expect("failed to build request");
    // Send the request and capture the response.
    // Expect a successful response due to the configured IoBuilder.
    let response = client
        .request(request)
        .await
        .expect("request should succeed");
    // Verify the response status is 200 OK.
    assert_eq!(response.status(), 200);
}
