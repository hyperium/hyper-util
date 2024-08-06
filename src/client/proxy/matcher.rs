use std::fmt;
use std::net::IpAddr;

use http::header::HeaderValue;
use ipnet::IpNet;
use percent_encoding::percent_decode_str;

/// A proxy matcher built using standard environment variables.
pub struct Matcher {
    http: Option<Intercept>,
    https: Option<Intercept>,
    no: NoProxy,
}

#[derive(Clone)]
pub struct Intercept {
    uri: http::Uri,
    basic_auth: Option<http::header::HeaderValue>,
    raw_auth: Option<(String, String)>,
}

#[derive(Default)]
struct Builder {
    is_cgi: bool,
    all: String,
    http: String,
    https: String,
    no: String,
}

struct NoProxy {
    ips: IpMatcher,
    domains: DomainMatcher,
}

struct DomainMatcher(Vec<String>);

struct IpMatcher(Vec<Ip>);

enum Ip {
    Address(IpAddr),
    Network(IpNet),
}

// ===== impl Matcher =====

impl Matcher {
    /// Create a matcher reading the current environment variables.
    pub fn from_env() -> Self {
        Builder::from_env().build()
    }

    /*
    pub fn builder() -> Builder {
        Builder::from_env().build()
    }
    */

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

// ===== impl Builder =====

impl Builder {
    fn from_env() -> Self {
        Builder {
            is_cgi: std::env::var_os("REQUEST_METHOD").is_some(),
            all: get_first_env(&["ALL_PROXY", "all_proxy"]),
            http: get_first_env(&["HTTP_PROXY", "http_proxy"]),
            https: get_first_env(&["HTTPS_PROXY", "https_proxy"]),
            no: get_first_env(&["NO_PROXY", "no_proxy"]),
        }
    }

    fn build(self) -> Matcher {
        if self.is_cgi {
            return Matcher {
                http: None,
                https: None,
                no: NoProxy::empty(),
            };
        }

        let all = parse_env_uri(&self.all);

        Matcher {
            http: parse_env_uri(&self.http).or_else(|| all.clone()),
            https: parse_env_uri(&self.https).or(all),
            no: NoProxy::from_string(&self.no),
        }
    }
}

fn get_first_env(names: &[&str]) -> String {
    for name in names {
        if let Ok(val) = std::env::var(name) {
            return val;
        }
    }

    String::new()
}

fn parse_env_uri(val: &str) -> Option<Intercept> {
    let uri = val.parse::<http::Uri>().ok()?;
    let mut builder = http::Uri::builder();
    let mut is_httpish = false;
    let mut basic_auth = None;
    let mut raw_auth = None;

    builder = builder.scheme(match uri.scheme() {
        Some(s) => {
            if s == &http::uri::Scheme::HTTP || s == &http::uri::Scheme::HTTPS {
                is_httpish = true;
                s.clone()
            } else if s.as_str() == "socks5" || s.as_str() == "socks5h" {
                s.clone()
            } else {
                // can't use this proxy scheme
                return None;
            }
        }
        // if no scheme provided, assume they meant 'http'
        None => {
            is_httpish = true;
            http::uri::Scheme::HTTP
        },
    });

    let authority = uri.authority()?;

    if let Some((userinfo, host_port)) = authority.as_str().split_once('@') {
        let (user, pass) = userinfo.split_once(':')?;
        let user = percent_decode_str(user).decode_utf8_lossy();
        let pass = percent_decode_str(pass).decode_utf8_lossy();
        if is_httpish {
            basic_auth = Some(encode_basic_auth(&user, Some(&pass)));
        } else {
            raw_auth = Some((user.into(), pass.into()));
        }
        builder = builder.authority(host_port);
    } else {
        builder = builder.authority(authority.clone());
    }

    // removing any path, but we MUST specify one or the builder errors
    builder = builder.path_and_query("/");

    let dst = builder.build().ok()?;

    Some(Intercept {
        uri: dst,
        basic_auth,
        raw_auth,
    })
}

fn encode_basic_auth(user: &str, pass: Option<&str>) -> HeaderValue {
    use base64::prelude::BASE64_STANDARD;
    use base64::write::EncoderWriter;
    use std::io::Write;

    let mut buf = b"Basic ".to_vec();
    {
        let mut encoder = EncoderWriter::new(&mut buf, &BASE64_STANDARD);
        let _ = write!(encoder, "{user}:");
        if let Some(password) = pass {
            let _ = write!(encoder, "{password}");
        }
    }
    let mut header = HeaderValue::from_bytes(&buf).expect("base64 is always valid HeaderValue");
    header.set_sensitive(true);
    header
}

impl NoProxy {
    /*
    fn from_env() -> NoProxy {
        let raw = std::env::var("NO_PROXY")
            .or_else(|_| std::env::var("no_proxy"))
            .unwrap_or_default();

        Self::from_string(&raw)
    }
    */

    fn empty() -> NoProxy {
        NoProxy {
            ips: IpMatcher(Vec::new()),
            domains: DomainMatcher(Vec::new()),
        }
    }

    /// Returns a new no-proxy configuration based on a `no_proxy` string (or `None` if no variables
    /// are set)
    /// The rules are as follows:
    /// * The environment variable `NO_PROXY` is checked, if it is not set, `no_proxy` is checked
    /// * If neither environment variable is set, `None` is returned
    /// * Entries are expected to be comma-separated (whitespace between entries is ignored)
    /// * IP addresses (both IPv4 and IPv6) are allowed, as are optional subnet masks (by adding /size,
    /// for example "`192.168.1.0/24`").
    /// * An entry "`*`" matches all hostnames (this is the only wildcard allowed)
    /// * Any other entry is considered a domain name (and may contain a leading dot, for example `google.com`
    /// and `.google.com` are equivalent) and would match both that domain AND all subdomains.
    ///
    /// For example, if `"NO_PROXY=google.com, 192.168.1.0/24"` was set, all of the following would match
    /// (and therefore would bypass the proxy):
    /// * `http://google.com/`
    /// * `http://www.google.com/`
    /// * `http://192.168.1.42/`
    ///
    /// The URL `http://notgoogle.com/` would not match.
    fn from_string(no_proxy_list: &str) -> Self {
        let mut ips = Vec::new();
        let mut domains = Vec::new();
        let parts = no_proxy_list.split(',').map(str::trim);
        for part in parts {
            match part.parse::<IpNet>() {
                // If we can parse an IP net or address, then use it, otherwise, assume it is a domain
                Ok(ip) => ips.push(Ip::Network(ip)),
                Err(_) => match part.parse::<IpAddr>() {
                    Ok(addr) => ips.push(Ip::Address(addr)),
                    Err(_) => domains.push(part.to_owned()),
                },
            }
        }
        NoProxy {
            ips: IpMatcher(ips),
            domains: DomainMatcher(domains),
        }
    }

    fn contains(&self, host: &str) -> bool {
        // According to RFC3986, raw IPv6 hosts will be wrapped in []. So we need to strip those off
        // the end in order to parse correctly
        let host = if host.starts_with('[') {
            let x: &[_] = &['[', ']'];
            host.trim_matches(x)
        } else {
            host
        };
        match host.parse::<IpAddr>() {
            // If we can parse an IP addr, then use it, otherwise, assume it is a domain
            Ok(ip) => self.ips.contains(ip),
            Err(_) => self.domains.contains(host),
        }
    }
}

impl IpMatcher {
    fn contains(&self, addr: IpAddr) -> bool {
        for ip in &self.0 {
            match ip {
                Ip::Address(address) => {
                    if &addr == address {
                        return true;
                    }
                }
                Ip::Network(net) => {
                    if net.contains(&addr) {
                        return true;
                    }
                }
            }
        }
        false
    }
}

impl DomainMatcher {
    // The following links may be useful to understand the origin of these rules:
    // * https://curl.se/libcurl/c/CURLOPT_NOPROXY.html
    // * https://github.com/curl/curl/issues/1208
    fn contains(&self, domain: &str) -> bool {
        let domain_len = domain.len();
        for d in &self.0 {
            if d == domain || d.strip_prefix('.') == Some(domain) {
                return true;
            } else if domain.ends_with(d) {
                if d.starts_with('.') {
                    // If the first character of d is a dot, that means the first character of domain
                    // must also be a dot, so we are looking at a subdomain of d and that matches
                    return true;
                } else if domain.as_bytes().get(domain_len - d.len() - 1) == Some(&b'.') {
                    // Given that d is a prefix of domain, if the prior character in domain is a dot
                    // then that means we must be matching a subdomain of d, and that matches
                    return true;
                }
            } else if d == "*" {
                return true;
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn test_no_proxy_wildcard() {
        let no_proxy = NoProxy::from_string("*");
        assert!(no_proxy.contains("any.where"));
    }

    #[test]
    fn test_no_proxy_ip_ranges() {
        let no_proxy =
            NoProxy::from_string(".foo.bar, bar.baz,10.42.1.1/24,::1,10.124.7.8,2001::/17");

        let should_not_match = [
            // random url, not in no_proxy
            "hyper.rs",
            // make sure that random non-subdomain string prefixes don't match
            "notfoo.bar",
            // make sure that random non-subdomain string prefixes don't match
            "notbar.baz",
            // ipv4 address out of range
            "10.43.1.1",
            // ipv4 address out of range
            "10.124.7.7",
            // ipv6 address out of range
            "[ffff:db8:a0b:12f0::1]",
            // ipv6 address out of range
            "[2005:db8:a0b:12f0::1]",
        ];

        for host in &should_not_match {
            assert!(!no_proxy.contains(host), "should not contain {:?}", host);
        }

        let should_match = [
            // make sure subdomains (with leading .) match
            "hello.foo.bar",
            // make sure exact matches (without leading .) match (also makes sure spaces between entries work)
            "bar.baz",
            // make sure subdomains (without leading . in no_proxy) match
            "foo.bar.baz",
            // make sure subdomains (without leading . in no_proxy) match - this differs from cURL
            "foo.bar",
            // ipv4 address match within range
            "10.42.1.100",
            // ipv6 address exact match
            "[::1]",
            // ipv6 address match within range
            "[2001:db8:a0b:12f0::1]",
            // ipv4 address exact match
            "10.124.7.8",
        ];

        for host in &should_match {
            assert!(no_proxy.contains(host), "should contain {:?}", host);
        }
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

        assert_eq!(
            "http://om.nom",
            intercept(&p, "http://example.com").uri()
        );

        assert_eq!(
            "http://om.nom",
            intercept(&p, "https://example.com").uri()
        );
    }

    #[test]
    fn test_specific_overrides_all() {
        let p = p! {
            all = "http://no.pe",
            http = "http://y.ep",
        };

        assert_eq!(
            "http://no.pe",
            intercept(&p, "https://example.com").uri()
        );

        // the http rule is "more specific" than the all rule
        assert_eq!(
            "http://y.ep",
            intercept(&p, "http://example.com").uri()
        );
    }
}
