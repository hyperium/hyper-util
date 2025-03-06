use super::no_proxy::NoProxy;
use super::utils::{get_first_env, parse_env_uri};
use super::Matcher;

#[derive(Default)]
pub struct Builder {
    pub(crate) is_cgi: bool,
    pub(crate) all: String,
    pub(crate) http: String,
    pub(crate) https: String,
    pub(crate) no: String,
}

// ===== impl Builder =====
impl Builder {
    pub(crate) fn from_env() -> Self {
        Builder {
            is_cgi: std::env::var_os("REQUEST_METHOD").is_some(),
            all: get_first_env(&["ALL_PROXY", "all_proxy"]),
            http: get_first_env(&["HTTP_PROXY", "http_proxy"]),
            https: get_first_env(&["HTTPS_PROXY", "https_proxy"]),
            no: get_first_env(&["NO_PROXY", "no_proxy"]),
        }
    }

    /// Set a proxy for all schemes (ALL_PROXY equivalent).
    pub fn all_proxy(mut self, proxy: impl Into<String>) -> Self {
        self.all = proxy.into();
        self
    }

    /// Set a proxy for HTTP schemes (HTTP_PROXY equivalent).
    pub fn http_proxy(mut self, proxy: impl Into<String>) -> Self {
        self.http = proxy.into();
        self
    }

    /// Set a proxy for HTTPS schemes (HTTPS_PROXY equivalent).
    pub fn https_proxy(mut self, proxy: impl Into<String>) -> Self {
        self.https = proxy.into();
        self
    }

    /// Set no-proxy rules (NO_PROXY equivalent).
    pub fn no_proxy(mut self, no_proxy: impl Into<String>) -> Self {
        self.no = no_proxy.into();
        self
    }

    pub(crate) fn build(self) -> Matcher {
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

// ===== MacOS Builder System Proxies =====
#[cfg(feature = "system-proxies")]
#[cfg(target_os = "macos")]
mod macos_proxies {
    use super::*;

    use system_configuration::core_foundation::array::CFArray;
    use system_configuration::core_foundation::base::{CFType, TCFType, TCFTypeRef};
    use system_configuration::core_foundation::dictionary::CFDictionary;
    use system_configuration::core_foundation::number::CFNumber;
    use system_configuration::core_foundation::string::{CFString, CFStringRef};
    use system_configuration::dynamic_store::{SCDynamicStore, SCDynamicStoreBuilder};

    impl Builder {
        // Helper function to check if a proxy is enabled
        fn is_proxy_enabled(&self, prefix: &str, proxies: &CFDictionary<CFString, CFType>) -> bool {
            let key = format!("{}Enable", prefix);
            proxies
                .find(CFString::new(&key))
                .map(|val| {
                    // Try to get the value as i32 directly
                    unsafe {
                        let num_ref = val.as_concrete_TypeRef();
                        if num_ref.is_null() {
                            return false;
                        }
                        let num = CFNumber::wrap_under_get_rule(num_ref as *const _);
                        num.to_i32() == Some(1)
                    }
                })
                .unwrap_or(false)
        }
        // Helper function to get a string value
        fn get_string(
            &self,
            key: &str,
            proxies: &CFDictionary<CFString, CFType>,
        ) -> Option<String> {
            proxies
                .find(CFString::new(key))
                .map(|val| unsafe {
                    let str_ref = val.as_concrete_TypeRef();
                    if str_ref.is_null() {
                        return None;
                    }
                    let cfstr = CFString::wrap_under_get_rule(str_ref as *const _);
                    Some(cfstr.to_string())
                })
                .flatten()
        }
        // Helper function to get an integer value
        fn get_int(&self, key: &str, proxies: &CFDictionary<CFString, CFType>) -> Option<i32> {
            proxies
                .find(CFString::new(key))
                .map(|val| unsafe {
                    let num_ref = val.as_concrete_TypeRef();
                    if num_ref.is_null() {
                        return None;
                    }
                    let num = CFNumber::wrap_under_get_rule(num_ref as *const _);
                    num.to_i32()
                })
                .flatten()
        }

        pub fn from_system_proxy(mut self) -> Self {
            let store = SCDynamicStoreBuilder::new("proxy-fetcher").build();

            if let Some(proxies) = store.get_proxies() {
                let (http, https, no) = self.extract_system_proxy(proxies);

                if let Some(http_proxy) = http {
                    self.http = http_proxy;
                }
                if let Some(https_proxy) = https {
                    self.https = https_proxy;
                }
                if let Some(no_proxy) = no {
                    self.no = no_proxy;
                }
            }

            self
        }
        pub(crate) fn extract_system_proxy(
            &self,
            proxies: CFDictionary<CFString, CFType>,
        ) -> (Option<String>, Option<String>, Option<String>) {
            let mut http: Option<String> = None;
            let mut https: Option<String> = None;
            let mut no: Option<String> = None;

            // Process HTTP proxy
            if self.is_proxy_enabled("HTTP", &proxies) {
                if let Some(host) = self.get_string("HTTPProxy", &proxies) {
                    let port = self.get_int("HTTPPort", &proxies);
                    http = match port {
                        Some(p) => Some(format!("http://{}:{}", host, p)),
                        None => Some(format!("http://{}", host)),
                    };
                }
            }

            // Process HTTPS proxy
            if self.is_proxy_enabled("HTTPS", &proxies) {
                if let Some(host) = self.get_string("HTTPSProxy", &proxies) {
                    let port = self.get_int("HTTPSPort", &proxies);
                    https = match port {
                        Some(p) => Some(format!("https://{}:{}", host, p)),
                        None => Some(format!("https://{}", host)),
                    };
                }
            }

            // Process exceptions (NO_PROXY)
            if let Some(exceptions_ref) = proxies.find(CFString::new("ExceptionsList")) {
                if let Some(arr) = exceptions_ref.downcast::<CFArray>() {
                    let exceptions: Vec<String> = arr
                        .iter()
                        .filter_map(|item| unsafe {
                            // Get the raw pointer value
                            let ptr = item.as_void_ptr();
                            if ptr.is_null() {
                                return None;
                            }
                            // Try to convert it to a CFString
                            let cfstr = CFString::wrap_under_get_rule(ptr as *const _);
                            Some(cfstr.to_string())
                        })
                        .collect();
                    no = Some(exceptions.join(","));
                }
            }

            (http, https, no)
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::client::proxy::Matcher;
        use system_configuration::core_foundation::array::CFArray;
        use std::{net::IpAddr, str::FromStr};

        struct MockSCDynamicStore {
            pairs: Vec<(CFString, CFType)>,
        }

        impl MockSCDynamicStore {
            fn new() -> Self {
                let mut keys = Vec::new();
                let mut values = Vec::new();

                // HTTP proxy enabled
                keys.push(CFString::new("HTTPEnable"));
                values.push(CFNumber::from(1).as_CFType());

                // HTTP proxy host and port
                keys.push(CFString::new("HTTPProxy"));
                values.push(CFString::new("test-proxy.example.com").as_CFType());
                keys.push(CFString::new("HTTPPort"));
                values.push(CFNumber::from(8080).as_CFType());

                // HTTPS proxy enabled
                keys.push(CFString::new("HTTPSEnable"));
                values.push(CFNumber::from(1).as_CFType());
                // HTTPS proxy host and port
                keys.push(CFString::new("HTTPSProxy"));
                values.push(CFString::new("secure-proxy.example.com").as_CFType());
                keys.push(CFString::new("HTTPSPort"));
                values.push(CFNumber::from(8443).as_CFType());

                // Exception list
                keys.push(CFString::new("ExceptionsList"));
                let exceptions = vec![
                    CFString::new("localhost").as_CFType(),
                    CFString::new("127.0.0.1").as_CFType(),
                    CFString::new("*.local").as_CFType(),
                ];
                values.push(CFArray::from_CFTypes(&exceptions).as_CFType());

                let pairs = keys
                    .iter()
                    .map(|k| k.clone())
                    .zip(values.iter().map(|v| v.as_CFType()))
                    .collect::<Vec<_>>();

                MockSCDynamicStore { pairs }
            }

            fn get_proxies(&self) -> Option<CFDictionary<CFString, CFType>> {
                let proxies = CFDictionary::from_CFType_pairs(&self.pairs.clone());
                Some(proxies)
            }
        }

        #[test]
        fn test_mac_os_proxy_mocked() {
            let mock_store = MockSCDynamicStore::new();
            let proxies = mock_store.get_proxies().unwrap();
            let (http, https, ns) = Matcher::builder().extract_system_proxy(proxies);

            assert!(http.is_some());
            assert!(https.is_some());
            assert!(ns.is_some());
        }

        #[ignore]
        #[test]
        fn test_mac_os_proxy() {
            let matcher = Matcher::builder().from_system_proxy().build();
            assert!(matcher
                .http
                .unwrap()
                .uri
                .eq("http://proxy.example.com:8080"));
            assert!(matcher
                .https
                .unwrap()
                .uri
                .eq("https://proxy.example.com:8080"));

            assert!(matcher.no.domains.contains("ebay.com"));
            assert!(matcher.no.domains.contains("amazon.com"));

            let ip = IpAddr::from_str("54.239.28.85").unwrap();
            assert!(matcher.no.ips.contains(ip));
        }
    }
}

// ===== Windows Builder System Proxies =====
#[cfg(feature = "system-proxies")]
#[cfg(target_os = "win")]
mod win_proxies {
    impl Builder {
        pub fn from_system_proxy(mut self) -> Self {
            todo!("Load Win system proxy settings");
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
    }
}
