use system_configuration::core_foundation::array::CFArray;
use system_configuration::core_foundation::base::{CFType, TCFType};
use system_configuration::core_foundation::dictionary::CFDictionary;
use system_configuration::core_foundation::number::CFNumber;
use system_configuration::core_foundation::string::{CFString, CFStringRef};
use system_configuration::dynamic_store::SCDynamicStoreBuilder;
use system_configuration::sys::schema_definitions::{
    kSCPropNetProxiesExceptionsList, kSCPropNetProxiesHTTPEnable, kSCPropNetProxiesHTTPPort,
    kSCPropNetProxiesHTTPProxy, kSCPropNetProxiesHTTPSEnable, kSCPropNetProxiesHTTPSPort,
    kSCPropNetProxiesHTTPSProxy,
};

use super::matcher::Builder;

pub(super) fn with_system(builder: &mut Builder) {
    let store = if let Some(store) = SCDynamicStoreBuilder::new("hyper-util").build() {
        store
    } else {
        return;
    };

    let proxies_map = if let Some(proxies_map) = store.get_proxies() {
        proxies_map
    } else {
        return;
    };

    with_proxies(builder, &proxies_map);
}

fn with_proxies(builder: &mut Builder, proxies_map: &CFDictionary<CFString, CFType>) {
    if builder.http.is_empty() {
        let http_proxy_config = parse_setting_from_dynamic_store(
            proxies_map,
            unsafe { kSCPropNetProxiesHTTPEnable },
            unsafe { kSCPropNetProxiesHTTPProxy },
            unsafe { kSCPropNetProxiesHTTPPort },
        );
        if let Some(http) = http_proxy_config {
            builder.http = http;
        }
    }

    if builder.https.is_empty() {
        let https_proxy_config = parse_setting_from_dynamic_store(
            proxies_map,
            unsafe { kSCPropNetProxiesHTTPSEnable },
            unsafe { kSCPropNetProxiesHTTPSProxy },
            unsafe { kSCPropNetProxiesHTTPSPort },
        );

        if let Some(https) = https_proxy_config {
            builder.https = https;
        }
    }

    if builder.no.is_empty() {
        if let Some(exceptions) = proxies_map
            .find(unsafe { kSCPropNetProxiesExceptionsList })
            .and_then(|value| value.downcast::<CFArray>())
        {
            builder.no = exceptions
                .iter()
                .filter_map(|value| {
                    // SAFETY: ExceptionsList contains Core Foundation objects.
                    let value = unsafe { CFType::wrap_under_get_rule(*value) };
                    value.downcast::<CFString>()
                })
                .map(|value| {
                    let value = value.to_string();
                    let value = value.trim();
                    let value = value.strip_prefix("*.").unwrap_or(value);
                    expand_ipv4_cidr(value).unwrap_or_else(|| value.to_owned())
                })
                .collect::<Vec<_>>()
                .join(",");
        }
    }
}

fn expand_ipv4_cidr(value: &str) -> Option<String> {
    let (address, prefix) = value.split_once('/')?;
    let parts = address.split('.').collect::<Vec<_>>();
    if parts.len() > 4 {
        return None;
    }

    // macOS includes abbreviated networks such as 169.254/16 by default.
    let mut octets = [0; 4];
    for (index, part) in parts.iter().enumerate() {
        octets[index] = part.parse().ok()?;
    }

    ipnet::Ipv4Net::new(octets.into(), prefix.parse().ok()?)
        .ok()
        .map(|network| network.to_string())
}

fn parse_setting_from_dynamic_store(
    proxies_map: &CFDictionary<CFString, CFType>,
    enabled_key: CFStringRef,
    host_key: CFStringRef,
    port_key: CFStringRef,
) -> Option<String> {
    let proxy_enabled = proxies_map
        .find(enabled_key)
        .and_then(|flag| flag.downcast::<CFNumber>())
        .and_then(|flag| flag.to_i32())
        .unwrap_or(0)
        == 1;

    if proxy_enabled {
        let proxy_host = proxies_map
            .find(host_key)
            .and_then(|host| host.downcast::<CFString>())
            .map(|host| host.to_string());
        let proxy_port = proxies_map
            .find(port_key)
            .and_then(|port| port.downcast::<CFNumber>())
            .and_then(|port| port.to_i32());

        return match (proxy_host, proxy_port) {
            (Some(proxy_host), Some(proxy_port)) => Some(format!("{proxy_host}:{proxy_port}")),
            (Some(proxy_host), None) => Some(proxy_host),
            (None, Some(_)) => None,
            (None, None) => None,
        };
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proxies_with_exceptions(exceptions: CFType) -> CFDictionary<CFString, CFType> {
        CFDictionary::from_CFType_pairs(&[
            (
                unsafe { CFString::wrap_under_get_rule(kSCPropNetProxiesHTTPEnable) },
                CFNumber::from(1).as_CFType(),
            ),
            (
                unsafe { CFString::wrap_under_get_rule(kSCPropNetProxiesHTTPProxy) },
                CFString::new("proxy.example").as_CFType(),
            ),
            (
                unsafe { CFString::wrap_under_get_rule(kSCPropNetProxiesHTTPSEnable) },
                CFNumber::from(1).as_CFType(),
            ),
            (
                unsafe { CFString::wrap_under_get_rule(kSCPropNetProxiesHTTPSProxy) },
                CFString::new("proxy.example").as_CFType(),
            ),
            (
                unsafe { CFString::wrap_under_get_rule(kSCPropNetProxiesExceptionsList) },
                exceptions,
            ),
        ])
    }

    #[test]
    fn system_exceptions_bypass_http_and_https_proxies() {
        let exceptions = CFArray::from_CFTypes(&[
            CFString::new("example.com"),
            CFString::new("*.local"),
            CFString::new("127.0.0.1"),
            CFString::new("192.168.0.0/16"),
            CFString::new("::1"),
            CFString::new("2001:db8::/32"),
        ]);
        let proxies = proxies_with_exceptions(exceptions.as_CFType());
        let mut builder = Builder::default();
        with_proxies(&mut builder, &proxies);
        let matcher = builder.build();

        for scheme in ["http", "https"] {
            for host in [
                "example.com",
                "www.example.com",
                "printer.local",
                "127.0.0.1",
                "192.168.1.2",
                "[::1]",
                "[2001:db8::42]",
            ] {
                let uri = format!("{scheme}://{host}/").parse().unwrap();
                assert!(matcher.intercept(&uri).is_none(), "should bypass {uri}");
            }
            for host in ["hyper.rs", "notexample.com", "notlocal", "192.169.1.2"] {
                let uri = format!("{scheme}://{host}/").parse().unwrap();
                assert!(matcher.intercept(&uri).is_some(), "should proxy {uri}");
            }
        }
    }

    #[test]
    fn configured_no_proxy_takes_precedence_over_system_exceptions() {
        let exceptions = CFArray::from_CFTypes(&[CFString::new("system.example")]);
        let proxies = proxies_with_exceptions(exceptions.as_CFType());
        let mut builder = Builder::default().no("env.example");
        with_proxies(&mut builder, &proxies);
        let matcher = builder.build();

        assert!(
            matcher
                .intercept(&"http://env.example/".parse().unwrap())
                .is_none()
        );
        assert!(
            matcher
                .intercept(&"http://system.example/".parse().unwrap())
                .is_some()
        );
    }

    #[test]
    fn default_macos_exceptions_are_supported() {
        let exceptions =
            CFArray::from_CFTypes(&[CFString::new("*.local"), CFString::new("169.254/16")]);
        let proxies = proxies_with_exceptions(exceptions.as_CFType());
        let mut builder = Builder::default();
        with_proxies(&mut builder, &proxies);
        let matcher = builder.build();

        for host in ["printer.local", "169.254.1.2"] {
            let uri = format!("http://{host}/").parse().unwrap();
            assert!(matcher.intercept(&uri).is_none(), "should bypass {uri}");
        }
        assert!(
            matcher
                .intercept(&"http://169.253.1.2/".parse().unwrap())
                .is_some()
        );
    }

    #[test]
    fn empty_or_invalid_exceptions_are_ignored() {
        for exceptions in [
            CFArray::<CFString>::from_CFTypes(&[]).as_CFType(),
            CFString::new("not an array").as_CFType(),
            CFArray::from_CFTypes(&[CFNumber::from(1)]).as_CFType(),
        ] {
            let proxies = proxies_with_exceptions(exceptions);
            let mut builder = Builder::default();
            with_proxies(&mut builder, &proxies);
            assert!(builder.no.is_empty());
            assert!(
                builder
                    .build()
                    .intercept(&"http://hyper.rs/".parse().unwrap())
                    .is_some()
            );
        }

        let mut builder = Builder::default().http("proxy.example");
        with_proxies(&mut builder, &CFDictionary::from_CFType_pairs(&[]));
        assert!(builder.no.is_empty());
        assert!(
            builder
                .build()
                .intercept(&"http://hyper.rs/".parse().unwrap())
                .is_some()
        );
    }
}
