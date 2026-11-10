//! HTTP GETs for fonts, icons and images. Native builds use a thread per request. The web build
//! has no threads or sockets: requests queue up for the page, which fetches them and hands the
//! bodies back through `fulfill` (see `crates/web`).

/// A response body, or the HTTP status of a failed request (`None` for network errors).
pub type Reply = Result<Vec<u8>, Option<u16>>;

/// Starts a GET; `done` runs on another thread (native) or from the page's callback (web).
pub fn get(url: String, limit: u64, done: impl FnOnce(Reply) + Send + 'static) {
    imp::get(url, limit, Box::new(done));
}

/// Native builds honor `STUDY_FETCH_HOSTS`: a comma-separated list of hosts study may contact
/// (`*.example.com` for subdomains too). When it's set, anything else, redirects included, fails
/// without a request, and an empty value blocks all network access. `HTTPS_PROXY`, `HTTP_PROXY`,
/// `ALL_PROXY` and `NO_PROXY` are honored either way.
#[cfg(not(target_os = "emscripten"))]
mod imp {
    use std::sync::OnceLock;

    fn allowed_hosts() -> Option<&'static [String]> {
        static HOSTS: OnceLock<Option<Vec<String>>> = OnceLock::new();
        HOSTS
            .get_or_init(|| {
                let v = std::env::var("STUDY_FETCH_HOSTS").ok()?;
                Some(v.split(',').map(|h| h.trim().to_ascii_lowercase()).filter(|h| !h.is_empty()).collect())
            })
            .as_deref()
    }

    pub(super) fn host_allowed(url: &str, hosts: &[String]) -> bool {
        let Ok(uri) = url.parse::<ureq::http::Uri>() else { return false };
        if !matches!(uri.scheme_str(), Some("http" | "https")) {
            return false;
        }
        let Some(host) = uri.host().map(|h| h.to_ascii_lowercase()) else { return false };
        hosts.iter().any(|a| match a.strip_prefix("*.") {
            Some(domain) => host == domain || host.ends_with(&format!(".{domain}")),
            None => *a == host,
        })
    }

    fn fetch(url: &str, limit: u64) -> super::Reply {
        let Some(hosts) = allowed_hosts() else {
            return read(ureq::get(url).call(), limit);
        };
        // Follow redirects ourselves, so each hop is checked against the list.
        let agent: ureq::Agent = ureq::Agent::config_builder().max_redirects(0).build().into();
        let mut url = url.to_string();
        for _ in 0..10 {
            if !host_allowed(&url, hosts) {
                return Err(None);
            }
            let r = agent.get(&url).call();
            let next = match &r {
                Ok(resp) if resp.status().is_redirection() => resp.headers().get("location").and_then(|l| l.to_str().ok()).map(|l| join(&url, l)),
                _ => None,
            };
            match next {
                Some(n) => url = n,
                None => return read(r, limit),
            }
        }
        Err(None)
    }

    /// A redirect target, which may be relative to the URL that sent it.
    fn join(base: &str, location: &str) -> String {
        if location.contains("://") {
            return location.to_string();
        }
        let Ok(uri) = base.parse::<ureq::http::Uri>() else { return location.to_string() };
        let origin = format!("{}://{}", uri.scheme_str().unwrap_or("https"), uri.authority().map(|a| a.as_str()).unwrap_or(""));
        if location.starts_with("//") {
            format!("{}:{location}", uri.scheme_str().unwrap_or("https"))
        } else if location.starts_with('/') {
            format!("{origin}{location}")
        } else {
            let dir = uri.path().rsplit_once('/').map_or("", |(d, _)| d);
            format!("{origin}{dir}/{location}")
        }
    }

    fn read(r: Result<ureq::http::Response<ureq::Body>, ureq::Error>, limit: u64) -> super::Reply {
        match r {
            Ok(mut r) if r.status().is_success() => r.body_mut().with_config().limit(limit).read_to_vec().map_err(|_| None),
            Ok(r) => Err(Some(r.status().as_u16())),
            Err(ureq::Error::StatusCode(s)) => Err(Some(s)),
            Err(_) => Err(None),
        }
    }

    pub fn get(url: String, limit: u64, done: Box<dyn FnOnce(super::Reply) + Send>) {
        std::thread::spawn(move || done(fetch(&url, limit)));
    }

    #[cfg(test)]
    mod tests {
        #[test]
        fn hosts() {
            let list = vec!["fonts.gstatic.com".to_string(), "*.example.com".to_string()];
            assert!(super::host_allowed("https://fonts.gstatic.com/s/inter/v1.woff2", &list));
            assert!(super::host_allowed("https://cdn.example.com/a.png", &list));
            assert!(super::host_allowed("https://example.com/a.png", &list));
            assert!(!super::host_allowed("https://evilexample.com/a.png", &list));
            assert!(!super::host_allowed("http://169.254.169.254/latest/meta-data", &list));
            assert!(!super::host_allowed("file:///etc/passwd", &list));
            assert_eq!(super::join("https://a.com/x/y.png", "/z.png"), "https://a.com/z.png");
            assert_eq!(super::join("https://a.com/x/y.png", "z.png"), "https://a.com/x/z.png");
            assert_eq!(super::join("https://a.com/x", "https://b.com/"), "https://b.com/");
        }
    }
}

#[cfg(target_os = "emscripten")]
mod imp {
    use std::cell::RefCell;
    use std::collections::HashMap;

    type Done = Box<dyn FnOnce(super::Reply) + Send>;

    #[derive(Default)]
    struct Queue {
        next: u32,
        requests: Vec<(u32, String)>,
        waiting: HashMap<u32, Done>,
    }

    thread_local! {
        static QUEUE: RefCell<Queue> = RefCell::new(Queue::default());
    }

    pub fn get(url: String, _limit: u64, done: Done) {
        QUEUE.with_borrow_mut(|q| {
            q.next += 1;
            let id = q.next;
            q.requests.push((id, url));
            q.waiting.insert(id, done);
        });
    }

    /// Requests the page hasn't started yet.
    pub fn take_requests() -> Vec<(u32, String)> {
        QUEUE.with_borrow_mut(|q| std::mem::take(&mut q.requests))
    }

    /// Delivers the page's response for request `id`.
    pub fn fulfill(id: u32, reply: super::Reply) {
        // Taken out first: the callback may queue a follow-up request.
        if let Some(done) = QUEUE.with_borrow_mut(|q| q.waiting.remove(&id)) {
            done(reply);
        }
    }
}

#[cfg(target_os = "emscripten")]
pub use imp::{fulfill, take_requests};
