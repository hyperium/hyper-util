use std::fmt;

use http::header::HeaderValue;
use super::builder::Builder;
use super::no_proxy::NoProxy;


/// A proxy matcher built using standard environment variables.
#[derive(Debug)]
pub struct Matcher {
    pub (crate) http: Option<Intercept>,
    pub (crate) https: Option<Intercept>,
    pub (crate) no: NoProxy,
}

#[derive(Clone)]
pub struct Intercept {
    pub (crate) uri: http::Uri,
    pub (crate) basic_auth: Option<http::header::HeaderValue>,
    pub (crate) raw_auth: Option<(String, String)>,
}


// ===== impl Matcher =====

impl Matcher {
    /// Create a matcher reading the current environment variables.
    pub fn from_env() -> Self {
        Builder::from_env().build()
    }

    /// Create a builder to configure a Matcher programmatically.
    pub fn builder() -> Builder {
        Builder::default()
    }

    /// Check if the destination should be intercepted by a proxy.
    ///
    /// If the proxy rules match the destination, a new `Uri` will be returned
    /// to connect to.
    pub fn intercept(&self, dst: &http::Uri) -> Option<&Intercept> {
        if self.no.contains(dst.host()?) {
            return None;
        }

        match dst.scheme_str() {
            Some("http") => self.http.as_ref(),
            Some("https") => self.https.as_ref(),
            _ => None,
        }
    }
}

// ===== impl Intercept =====

impl Intercept {
    pub fn uri(&self) -> &http::Uri {
        &self.uri
    }

    pub fn basic_auth(&self) -> Option<&HeaderValue> {
        self.basic_auth.as_ref()
    }

    pub fn raw_auth(&self) -> Option<(&str, &str)> {
        self.raw_auth.as_ref().map(|&(ref u, ref p)| (&**u, &**p))
    }
}

impl fmt::Debug for Intercept {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Intercept")
            .field("uri", &self.uri)
            // dont output auth, its sensitive
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use crate::client::proxy::no_proxy::DomainMatcher;
    use super::*;

    #[test]
    fn test_manual_configuration() {
        let matcher = Matcher::builder()
            .http_proxy("http://proxy.example.com:8080")
            .no_proxy("localhost, 127.0.0.1")
            .build();

        // HTTP URL should use the proxy
        let intercept = matcher.intercept(&"http://example.com".parse().unwrap());
        assert!(intercept.is_some());
        assert_eq!(
            intercept.unwrap().uri().to_string(),
            "http://proxy.example.com:8080/"
        );

        // No-proxy hosts should bypass the proxy
        let intercept = matcher.intercept(&"http://localhost".parse().unwrap());
        assert!(intercept.is_none());

        let intercept = matcher.intercept(&"http://127.0.0.1".parse().unwrap());
        assert!(intercept.is_none());
    }

    #[test]
    fn test_all_proxy_manual() {
        let matcher = Matcher::builder()
            .all_proxy("http://all.proxy.com:9999")
            .build();

        let intercept = matcher.intercept(&"http://example.com".parse().unwrap());
        assert_eq!(
            intercept.unwrap().uri().to_string(),
            "http://all.proxy.com:9999/"
        );

        let intercept = matcher.intercept(&"https://example.com".parse().unwrap());
        assert_eq!(
            intercept.unwrap().uri().to_string(),
            "http://all.proxy.com:9999/"
        );
    }

    #[test]
    fn test_domain_matcher() {
        let domains = vec![".foo.bar".into(), "bar.foo".into()];
        let matcher = DomainMatcher(domains);

        // domains match with leading `.`
        assert!(matcher.contains("foo.bar"));
        // subdomains match with leading `.`
        assert!(matcher.contains("www.foo.bar"));

        // domains match with no leading `.`
        assert!(matcher.contains("bar.foo"));
        // subdomains match with no leading `.`
        assert!(matcher.contains("www.bar.foo"));

        // non-subdomain string prefixes don't match
        assert!(!matcher.contains("notfoo.bar"));
        assert!(!matcher.contains("notbar.foo"));
    }

    macro_rules! p {
        ($($n:ident = $v:expr,)*) => ({Builder {
            $($n: $v.into(),)*
            ..Builder::default()
        }.build()});
    }

    fn intercept<'a>(p: &'a Matcher, u: &str) -> &'a Intercept {
        p.intercept(&u.parse().unwrap()).unwrap()
    }

    #[test]
    fn test_all_proxy() {
        let p = p! {
            all = "http://om.nom",
        };

        assert_eq!("http://om.nom", intercept(&p, "http://example.com").uri());

        assert_eq!("http://om.nom", intercept(&p, "https://example.com").uri());
    }

    #[test]
    fn test_specific_overrides_all() {
        let p = p! {
            all = "http://no.pe",
            http = "http://y.ep",
        };

        assert_eq!("http://no.pe", intercept(&p, "https://example.com").uri());

        // the http rule is "more specific" than the all rule
        assert_eq!("http://y.ep", intercept(&p, "http://example.com").uri());
    }
}
