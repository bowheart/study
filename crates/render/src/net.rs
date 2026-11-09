//! HTTP GETs for fonts, icons and images. Native builds use a thread per request. The web build
//! has no threads or sockets: requests queue up for the page, which fetches them and hands the
//! bodies back through `fulfill` (see `crates/web`).

/// A response body, or the HTTP status of a failed request (`None` for network errors).
pub type Reply = Result<Vec<u8>, Option<u16>>;

/// Starts a GET; `done` runs on another thread (native) or from the page's callback (web).
pub fn get(url: String, limit: u64, done: impl FnOnce(Reply) + Send + 'static) {
    imp::get(url, limit, Box::new(done));
}

#[cfg(not(target_os = "emscripten"))]
mod imp {
    pub fn get(url: String, limit: u64, done: Box<dyn FnOnce(super::Reply) + Send>) {
        std::thread::spawn(move || {
            done(match ureq::get(&url).call() {
                Ok(mut r) => r.body_mut().with_config().limit(limit).read_to_vec().map_err(|_| None),
                Err(ureq::Error::StatusCode(s)) => Err(Some(s)),
                Err(_) => Err(None),
            })
        });
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
