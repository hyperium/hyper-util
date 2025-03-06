use http::HeaderValue;
use percent_encoding::percent_decode_str;
use super::matcher::Intercept;


pub fn get_first_env(names: &[&str]) -> String {
    for name in names {
        if let Ok(val) = std::env::var(name) {
            return val;
        }
    }

    String::new()
}

pub fn parse_env_uri(val: &str) -> Option<Intercept> {
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
        }
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

pub fn encode_basic_auth(user: &str, pass: Option<&str>) -> HeaderValue {
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
