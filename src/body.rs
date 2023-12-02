use std::pin::pin;

use http::{Request, Response};

/// Wrapping [http_body::Body]
pub struct Body(
    Box<
        dyn http_body::Body<
                Data = Box<dyn bytes::Buf + Send>,
                Error = Box<dyn std::error::Error + Send + Sync>,
            > + Unpin
            + Send,
    >,
);

impl Body {
    /// Create new instance from any `Body`
    pub fn new<B, D, E>(body: B) -> Self
    where
        B: http_body::Body<Data = D, Error = E> + Unpin + Send + 'static,
        D: bytes::Buf + Send + 'static,
        E: std::error::Error + Send + Sync + 'static,
    {
        Self(Box::new(BodyWrapper(body)))
    }

    /// Convert `Body` in [Request]
    pub fn convert_request<B, D, E>(request: Request<B>) -> Request<Self>
    where
        B: http_body::Body<Data = D, Error = E> + Unpin + Send + 'static,
        D: bytes::Buf + Send + 'static,
        E: std::error::Error + Send + Sync + 'static,
    {
        let (parts, body) = request.into_parts();
        let body = Self::new(body);
        Request::from_parts(parts, body)
    }

    /// Convert `Body` in [Response]
    pub fn convert_response<B, D, E>(response: Response<B>) -> Response<Self>
    where
        B: http_body::Body<Data = D, Error = E> + Unpin + Send + 'static,
        D: bytes::Buf + Send + 'static,
        E: std::error::Error + Send + Sync + 'static,
    {
        let (parts, body) = response.into_parts();
        let body = Self::new(body);
        Response::from_parts(parts, body)
    }
}

impl http_body::Body for Body {
    type Data = Box<dyn bytes::Buf + Send>;
    type Error = Box<dyn std::error::Error + Send + Sync>;

    fn poll_frame(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<http_body::Frame<Self::Data>, Self::Error>>> {
        pin!(&mut self.0).poll_frame(cx)
    }

    fn is_end_stream(&self) -> bool {
        self.0.is_end_stream()
    }

    fn size_hint(&self) -> http_body::SizeHint {
        self.0.size_hint()
    }
}

struct BodyWrapper<B, D, E>(B)
where
    B: http_body::Body<Data = D, Error = E> + Unpin + Send,
    D: bytes::Buf + Send + 'static,
    E: std::error::Error + Send + Sync + 'static;

impl<B, D, E> http_body::Body for BodyWrapper<B, D, E>
where
    B: http_body::Body<Data = D, Error = E> + Unpin + Send,
    D: bytes::Buf + Send + 'static,
    E: std::error::Error + Send + Sync + 'static,
{
    type Data = Box<dyn bytes::Buf + Send>;
    type Error = Box<dyn std::error::Error + Send + Sync>;

    fn poll_frame(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<http_body::Frame<Self::Data>, Self::Error>>> {
        pin!(&mut self.0)
            .poll_frame(cx)
            .map_ok(|o| {
                o.map_data(|d| {
                    let d: Box<dyn bytes::Buf + Send> = Box::new(d);
                    d
                })
            })
            .map_err(|e| {
                let e: Box<dyn std::error::Error + Send + Sync> = Box::new(e);
                e
            })
    }

    fn is_end_stream(&self) -> bool {
        self.0.is_end_stream()
    }

    fn size_hint(&self) -> http_body::SizeHint {
        self.0.size_hint()
    }
}
