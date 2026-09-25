use super::matcher::Builder;

struct WinProxyParser {
    http: Option<String>,
    https: Option<String>,
    all: Option<String>,
}

impl WinProxyParser {
    fn parse(proxy_server_setting: &str) -> Self {
        let mut http = None;
        let mut https = None;
        let mut all = None;

        for proxy in proxy_server_setting
            .split(';')
            .map(str::trim)
            .filter(|p| !p.is_empty())
        {
            let Some((schema, authority)) = proxy.split_once('=') else {
                all = Some(proxy.into());
                continue;
            };

            let mut builder = http::Uri::builder();
            builder = builder.path_and_query("/");
            builder = builder.authority(authority);

            match schema {
                "http" => {
                    builder = builder.scheme("http");
                    http = builder.build().ok().map(|v| v.to_string());
                }
                "https" => {
                    builder = builder.scheme("https");
                    https = builder.build().ok().map(|v| v.to_string());
                }
                "socks" => {
                    builder = builder.scheme("socks");
                    all = builder.build().ok().map(|v| v.to_string());
                }
                // For unknown or no scheme, ignore for now.
                _ => (),
            }
        }

        Self { http, https, all }
    }
}

fn ipv4_wildcard_to_cidr(value: &str) -> Option<String> {
    let parts = value.split('.').collect::<Vec<_>>();
    let wildcard = parts.iter().position(|part| *part == "*")?;

    if wildcard == 0 || wildcard > 3 || parts[wildcard..].iter().any(|part| *part != "*") {
        return None;
    }

    let mut octets = [0; 4];
    for (index, part) in parts[..wildcard].iter().enumerate() {
        octets[index] = part.parse().ok()?;
    }

    Some(format!(
        "{}.{}.{}.{}/{}",
        octets[0],
        octets[1],
        octets[2],
        octets[3],
        wildcard * 8
    ))
}

pub(super) fn normalize_proxy_override(value: &str) -> String {
    value
        .split(';')
        .map(|entry| {
            let entry = entry.trim();
            ipv4_wildcard_to_cidr(entry).unwrap_or_else(|| entry.to_string())
        })
        .collect::<Vec<_>>()
        .join(",")
        .replace("*.", "")
}

pub(super) fn with_system(builder: &mut Builder) {
    let settings = if let Ok(settings) = windows_registry::CURRENT_USER
        .open("Software\\Microsoft\\Windows\\CurrentVersion\\Internet Settings")
    {
        settings
    } else {
        return;
    };

    if settings.get_u32("ProxyEnable").unwrap_or(0) == 0 {
        return;
    }

    if let Ok(val) = settings.get_string("ProxyServer") {
        let parsed = WinProxyParser::parse(&val);
        if let Some(val) = parsed.http {
            if builder.http.is_empty() {
                builder.http = val;
            }
        }
        if let Some(val) = parsed.https {
            if builder.https.is_empty() {
                builder.https = val;
            }
        }
        if let Some(val) = parsed.all {
            if builder.all.is_empty() {
                builder.all = val;
            }
        }
    }

    if builder.no.is_empty() {
        if let Ok(val) = settings.get_string("ProxyOverride") {
            builder.no = normalize_proxy_override(&val);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::WinProxyParser;

    #[test]
    fn per_scheme_values_go_to_their_own_slots() {
        let proxy = WinProxyParser::parse("http=127.0.0.1:8080;https=127.0.0.1:8443");
        assert_eq!(proxy.http.as_deref(), Some("http://127.0.0.1:8080/"));
        assert_eq!(proxy.https.as_deref(), Some("https://127.0.0.1:8443/"));
        assert_eq!(proxy.all, None);
    }

    #[test]
    fn all_schemes_together() {
        let proxy =
            WinProxyParser::parse("http=10.0.0.1:8080;https=10.0.0.1:8443;socks=10.0.0.1:1080");
        assert_eq!(proxy.http.as_deref(), Some("http://10.0.0.1:8080/"));
        assert_eq!(proxy.https.as_deref(), Some("https://10.0.0.1:8443/"));
        assert_eq!(proxy.all.as_deref(), Some("socks://10.0.0.1:1080/"));
    }

    #[test]
    fn single_proxy_without_scheme_goes_to_all() {
        let proxy = WinProxyParser::parse("127.0.0.1:8080/");
        assert_eq!(proxy.http, None);
        assert_eq!(proxy.https, None);
        assert_eq!(proxy.all.as_deref(), Some("127.0.0.1:8080/"));
    }

    #[test]
    fn unknown_schemes_are_ignored() {
        let proxy = WinProxyParser::parse("ftp=127.0.0.1:8021/");
        assert_eq!(proxy.http, None);
        assert_eq!(proxy.https, None);
        assert_eq!(proxy.all, None);
    }

    #[test]
    fn whitespace_and_empty_segments_are_ignored() {
        let proxy = WinProxyParser::parse("  http=127.0.0.1:8080 ; ; ");
        assert_eq!(proxy.http.as_deref(), Some("http://127.0.0.1:8080/"));
        assert_eq!(proxy.https, None);
        assert_eq!(proxy.all, None);
    }

    #[test]
    fn trailing_separator() {
        let proxy = WinProxyParser::parse("socks=127.0.0.1:1080;");
        assert_eq!(proxy.all.as_deref(), Some("socks://127.0.0.1:1080/"));
    }
}
