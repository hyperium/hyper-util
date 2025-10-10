//! todo

use std::task::{Context, Poll};

use http::header::{HeaderValue, HOST};
use http::{Method, Request, Uri};
use tower_service::Service;

/// todo
#[derive(Clone, Debug)]
pub struct SetHost<S> {
    inner: S,
}

/// todo
#[derive(Clone, Debug)]
pub struct Http1RequestTarget<S> {
    inner: S,
}

// ===== impl SetHost =====

impl<S> SetHost<S> {
    /// todo
    pub fn new(inner: S) -> Self {
        SetHost { inner }
    }

    /// Access the inner service.
    pub fn inner(&self) -> &S {
        &self.inner
    }
}

impl<S, ReqBody> Service<Request<ReqBody>> for SetHost<S>
where
    S: Service<Request<ReqBody>>,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = S::Future;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, mut req: Request<ReqBody>) -> Self::Future {
        if req.uri().authority().is_some() {
            let uri = req.uri().clone();
            req.headers_mut().entry(HOST).or_insert_with(|| {
                let hostname = uri.host().expect("authority implies host");
                if let Some(port) = get_non_default_port(&uri) {
                    let s = format!("{hostname}:{port}");
                    HeaderValue::from_str(&s)
                } else {
                    HeaderValue::from_str(hostname)
                }
                .expect("uri host is valid header value")
            });
        }
        self.inner.call(req)
    }
}

fn get_non_default_port(uri: &Uri) -> Option<http::uri::Port<&str>> {
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

// ===== impl Http1RequestTarget =====

impl<S> Http1RequestTarget<S> {
    /// todo
    pub fn new(inner: S) -> Self {
        Http1RequestTarget { inner }
    }

    /// Access the inner service.
    pub fn inner(&self) -> &S {
        &self.inner
    }
}

impl<S, ReqBody> Service<Request<ReqBody>> for Http1RequestTarget<S>
where
    S: Service<Request<ReqBody>>,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = S::Future;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, mut req: Request<ReqBody>) -> Self::Future {
        // CONNECT always sends authority-form, so check it first...
        if req.method() == Method::CONNECT {
            authority_form(req.uri_mut());
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

fn authority_form(uri: &mut Uri) {
    if let Some(path) = uri.path_and_query() {
        // `https://hyper.rs` would parse with `/` path, don't
        // annoy people about that...
        if path != "/" {
            tracing::debug!("HTTP/1.1 CONNECT request stripping path: {:?}", path);
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
