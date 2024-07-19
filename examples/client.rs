use std::env;

use http_body_util::Empty;
use hyper::Request;
use hyper_util::client::legacy::{connect::HttpConnector, Client};
use tracing::{info_span, Instrument};
use tracing_subscriber::{
    fmt::{self, format::FmtSpan},
    prelude::*,
    EnvFilter,
};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let fmt_layer = fmt::layer()
        .with_span_events(FmtSpan::CLOSE) // show time elapsed in spans
        .with_timer(fmt::time::Uptime::default());
    tracing_subscriber::registry()
        .with(EnvFilter::from_default_env())
        .with(fmt_layer)
        .init();

    let url = match env::args().nth(1) {
        Some(url) => url,
        None => {
            eprintln!("Usage: client <url>");
            return Ok(());
        }
    };

    // HTTPS requires picking a TLS implementation, so give a better
    // warning if the user tries to request an 'https' URL.
    let url = url.parse::<hyper::Uri>()?;
    if url.scheme_str() != Some("http") {
        eprintln!("This example only works with 'http' URLs.");
        return Ok(());
    }

    let client = Client::builder(hyper_util::rt::TokioExecutor::new()).build(HttpConnector::new());

    let req = Request::builder()
        .uri(url)
        .body(Empty::<bytes::Bytes>::new())?;

    let span = info_span!("request", uri = %req.uri());
    let resp = client.request(req).instrument(span).await?;

    eprintln!("{:?} {:?}", resp.version(), resp.status());
    eprintln!("{:#?}", resp.headers());

    Ok(())
}
