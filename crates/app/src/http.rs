//! Tiny blocking HTTP abstraction used by `updater.rs`.
//!
//! A trait (instead of calling `ureq` directly) lets unit tests run the whole
//! client logic against canned responses, with zero real network traffic.
//!
//! IMPORTANT: everything here blocks. Never call it from the input hot path;
//! use a background thread or an on-demand task.

use std::io::{Read, Write};
use std::path::Path;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct HttpResponse {
    pub status: u16,
    pub body: String,
}

impl HttpResponse {
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

/// `Err(String)` = transport failure (DNS, connect, timeout, ...).
/// Any HTTP status (also 4xx/5xx) comes back as `Ok(HttpResponse)`.
pub trait Transport: Send + Sync {
    fn get(&self, url: &str) -> Result<HttpResponse, String>;
    /// Stream a 2xx response body to `dest`; returns bytes written.
    /// Fails (and removes the partial file) above `max_bytes` or on non-2xx.
    fn download(&self, url: &str, dest: &Path, max_bytes: u64) -> Result<u64, String>;
}

pub struct UreqTransport {
    agent: ureq::Agent,
}

impl UreqTransport {
    pub fn new(timeout: Duration) -> Self {
        Self {
            agent: ureq::AgentBuilder::new()
                .timeout_connect(timeout)
                .timeout(timeout)
                .user_agent(concat!("INTmouse/", env!("CARGO_PKG_VERSION")))
                .build(),
        }
    }

    /// Short timeouts for API calls.
    pub fn api() -> Self {
        Self::new(Duration::from_secs(5))
    }

    /// Longer overall timeout for downloading an installer.
    pub fn download_agent() -> Self {
        Self::new(Duration::from_secs(300))
    }
}

fn into_response(r: Result<ureq::Response, ureq::Error>) -> Result<HttpResponse, String> {
    match r {
        Ok(resp) => {
            let status = resp.status();
            let body = resp.into_string().map_err(|e| e.to_string())?;
            Ok(HttpResponse { status, body })
        }
        Err(ureq::Error::Status(status, resp)) => Ok(HttpResponse {
            status,
            body: resp.into_string().unwrap_or_default(),
        }),
        Err(e) => Err(e.to_string()),
    }
}

impl Transport for UreqTransport {
    fn get(&self, url: &str) -> Result<HttpResponse, String> {
        into_response(self.agent.get(url).call())
    }

    fn download(&self, url: &str, dest: &Path, max_bytes: u64) -> Result<u64, String> {
        let resp = match self.agent.get(url).call() {
            Ok(r) => r,
            Err(ureq::Error::Status(code, _)) => return Err(format!("HTTP {code}")),
            Err(e) => return Err(e.to_string()),
        };
        let mut reader = resp.into_reader().take(max_bytes.saturating_add(1));
        let mut file = std::fs::File::create(dest).map_err(|e| e.to_string())?;
        let result = std::io::copy(&mut reader, &mut file)
            .and_then(|n| file.flush().map(|_| n))
            .map_err(|e| e.to_string());
        drop(file);
        match result {
            Ok(n) if n > max_bytes => {
                let _ = std::fs::remove_file(dest);
                Err(format!("download exceeds {max_bytes} bytes"))
            }
            Ok(n) => Ok(n),
            Err(e) => {
                let _ = std::fs::remove_file(dest);
                Err(e)
            }
        }
    }
}

/// Canned-response transport for unit tests (no sockets at all).
#[cfg(test)]
pub mod mock {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    pub struct MockTransport {
        replies: Mutex<VecDeque<Result<HttpResponse, String>>>,
        /// One entry per call: `"GET url"` or `"POST url body"` or `"DOWNLOAD url"`.
        pub calls: Mutex<Vec<String>>,
        pub download_bytes: Mutex<Vec<u8>>,
    }

    impl MockTransport {
        pub fn new() -> Arc<Self> {
            Arc::new(Self::default())
        }
        pub fn push_ok(&self, status: u16, body: &str) {
            self.replies
                .lock()
                .unwrap()
                .push_back(Ok(HttpResponse { status, body: body.into() }));
        }
        pub fn push_err(&self, msg: &str) {
            self.replies.lock().unwrap().push_back(Err(msg.into()));
        }
        pub fn call_count(&self) -> usize {
            self.calls.lock().unwrap().len()
        }
        fn next(&self, what: String) -> Result<HttpResponse, String> {
            self.calls.lock().unwrap().push(what);
            self.replies
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| Err("mock: no reply queued".into()))
        }
    }

    impl Transport for MockTransport {
        fn get(&self, url: &str) -> Result<HttpResponse, String> {
            self.next(format!("GET {url}"))
        }
        fn download(&self, url: &str, dest: &Path, _max: u64) -> Result<u64, String> {
            self.calls.lock().unwrap().push(format!("DOWNLOAD {url}"));
            let bytes = self.download_bytes.lock().unwrap().clone();
            std::fs::write(dest, &bytes).map_err(|e| e.to_string())?;
            Ok(bytes.len() as u64)
        }
    }

    impl Transport for Arc<MockTransport> {
        fn get(&self, url: &str) -> Result<HttpResponse, String> {
            (**self).get(url)
        }
        fn download(&self, url: &str, dest: &Path, max: u64) -> Result<u64, String> {
            (**self).download(url, dest, max)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::net::TcpListener;

    /// Loopback-only server (127.0.0.1) answering one request: verifies that the
    /// real `ureq` path maps 4xx to `Ok(HttpResponse)` and sends a plain GET.
    #[test]
    fn ureq_transport_maps_status_and_body_on_loopback() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(s.try_clone().unwrap());
            let mut request_line = String::new();
            reader.read_line(&mut request_line).unwrap();
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" || line.is_empty() {
                    break;
                }
            }
            let reply = r#"{"version":""}"#;
            write!(
                s,
                "HTTP/1.1 404 Not Found\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                reply.len(),
                reply
            )
            .unwrap();
            request_line
        });
        let t = UreqTransport::new(Duration::from_secs(5));
        let r = t.get(&format!("http://127.0.0.1:{port}/version.json")).unwrap();
        assert_eq!(r.status, 404);
        assert!(r.body.contains("version"));
        assert_eq!(server.join().unwrap(), "GET /version.json HTTP/1.1\r\n");
    }

    #[test]
    fn connection_refused_is_transport_error() {
        // Bind then drop to obtain a port nothing listens on.
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let t = UreqTransport::new(Duration::from_secs(2));
        assert!(t.get(&format!("http://127.0.0.1:{port}/x")).is_err());
    }
}
