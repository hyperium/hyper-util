use http::{uri::Scheme, Method, Request, Uri};
use hyper::service::Service;
use tracing::warn;

/// A `Service` that normalizes the request target.
pub struct Http1RequestTarget<S> {
    inner: S,
    is_proxied: bool,
}

impl<S> Http1RequestTarget<S> {
    /// Create a new `Http1RequestTarget` service.
    pub fn new(inner: S, is_proxied: bool) -> Self {
        Self { inner, is_proxied }
    }
}

impl<S, B> Service<Request<B>> for Http1RequestTarget<S>
where
    S: Service<Request<B>>,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = S::Future;

    fn call(&self, mut req: Request<B>) -> Self::Future {
        // CONNECT always sends authority-form, so check it first...
        if req.method() == Method::CONNECT {
            authority_form(req.uri_mut());
        } else if self.is_proxied {
            absolute_form(req.uri_mut());
        } else {
            origin_form(req.uri_mut());
        }
        self.inner.call(req)
    }
}

fn origin_form(uri: &mut Uri) {
    let path = match uri.path_and_query() {
        Some(path) if path.as_str() != "/" => {
            let mut parts = ::http::uri::Parts::default();
            parts.path_and_query = Some(path.clone());
            Uri::from_parts(parts).expect("path is valid uri")
        }
        _none_or_just_slash => {
            debug_assert!(Uri::default() == "/");
            Uri::default()
        }
    };
    *uri = path
}

fn absolute_form(uri: &mut Uri) {
    debug_assert!(uri.scheme().is_some(), "absolute_form needs a scheme");
    debug_assert!(
        uri.authority().is_some(),
        "absolute_form needs an authority"
    );
    // If the URI is to HTTPS, and the connector claimed to be a proxy,
    // then it *should* have tunneled, and so we don't want to send
    // absolute-form in that case.
    if uri.scheme() == Some(&Scheme::HTTPS) {
        origin_form(uri);
    }
}

fn authority_form(uri: &mut Uri) {
    if let Some(path) = uri.path_and_query() {
        // `https://hyper.rs` would parse with `/` path, don't
        // annoy people about that...
        if path != "/" {
            warn!("HTTP/1.1 CONNECT request stripping path: {:?}", path);
        }
    }
    *uri = match uri.authority() {
        Some(auth) => {
            let mut parts = ::http::uri::Parts::default();
            parts.authority = Some(auth.clone());
            Uri::from_parts(parts).expect("authority is valid")
        }
        None => {
            unreachable!("authority_form with relative uri");
        }
    };
}
