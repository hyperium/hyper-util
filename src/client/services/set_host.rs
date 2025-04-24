use http::{header::HOST, uri::Port, HeaderValue, Request, Uri};
use hyper::service::Service;

/// A `Service` that sets the `Host` header, if it's missing, based on the request URI.
pub struct SetHost<S> {
    inner: S,
}

impl<S> SetHost<S> {
    /// Create a new `SetHost` service.
    pub fn new(inner: S) -> Self {
        Self { inner }
    }
}

impl<S, B> Service<Request<B>> for SetHost<S>
where
    S: Service<Request<B>>,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = S::Future;

    fn call(&self, mut req: Request<B>) -> Self::Future {
        let uri = req.uri().clone();
        req.headers_mut().entry(HOST).or_insert_with(|| {
            let hostname = uri.host().expect("authority implies host");
            if let Some(port) = get_non_default_port(&uri) {
                let s = format!("{}:{}", hostname, port);
                HeaderValue::from_str(&s)
            } else {
                HeaderValue::from_str(hostname)
            }
            .expect("uri host is valid header value")
        });
        self.inner.call(req)
    }
}

fn get_non_default_port(uri: &Uri) -> Option<Port<&str>> {
    match (uri.port().map(|p| p.as_u16()), is_schema_secure(uri)) {
        (Some(443), true) => None,
        (Some(80), false) => None,
        _ => uri.port(),
    }
}

fn is_schema_secure(uri: &Uri) -> bool {
    uri.scheme_str()
        .map(|scheme_str| matches!(scheme_str, "wss" | "https"))
        .unwrap_or_default()
}
