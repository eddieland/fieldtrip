//! A mock GraphQL endpoint: a small HTTP server that records every request it receives.

use std::fmt::Write as _;
use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

/// One HTTP request the mock received.
#[derive(Debug, Clone)]
pub(crate) struct Recorded {
    /// The request method, such as `POST`.
    pub(crate) method: String,
    /// Header names, lowercased, with their values.
    #[expect(dead_code, reason = "first read by the header slices")]
    pub(crate) headers: Vec<(String, String)>,
    /// The request body.
    pub(crate) body: String,
}

/// What the mock answers with.
#[derive(Debug, Clone)]
pub(crate) struct Response {
    /// The HTTP status code.
    pub(crate) status: u16,
    /// Extra response headers.
    pub(crate) headers: Vec<(String, String)>,
    /// The response body.
    pub(crate) body: String,
}

impl Response {
    /// A response with `status` and a plain-text `body`.
    pub(crate) fn new(status: u16, body: impl Into<String>) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: body.into(),
        }
    }
}

/// A running mock endpoint.
#[derive(Debug)]
pub(crate) struct MockServer {
    port: u16,
    requests: Arc<Mutex<Vec<Recorded>>>,
}

impl MockServer {
    /// Starts a server on a free local port that answers each request with `respond`.
    pub(crate) fn start(respond: impl Fn(&Recorded) -> Response + Send + Sync + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind a local port");
        let port = listener.local_addr().expect("local address").port();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::clone(&requests);
        let respond = Arc::new(respond);
        // The accept loop lives until the test process exits.
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let log = Arc::clone(&log);
                let respond = Arc::clone(&respond);
                std::thread::spawn(move || serve(stream, &log, respond.as_ref()));
            }
        });
        Self { port, requests }
    }

    /// Starts a server that fails every request. For tests where no request should arrive at all.
    pub(crate) fn unreachable() -> Self {
        Self::start(|_| Response::new(500, "this endpoint should not have been called"))
    }

    /// Starts a server that answers as `api` does.
    pub(crate) fn serving(api: super::FixtureApi) -> Self {
        Self::start(move |request| api.respond(request))
    }

    /// The URL to put in a config file.
    pub(crate) fn url(&self) -> String {
        format!("http://127.0.0.1:{}/graphql", self.port)
    }

    /// Every request received so far, oldest first.
    pub(crate) fn requests(&self) -> Vec<Recorded> {
        self.requests.lock().expect("request log").clone()
    }
}

/// Reads one request from `stream`, records it, and writes the response.
fn serve(stream: TcpStream, log: &Mutex<Vec<Recorded>>, respond: &dyn Fn(&Recorded) -> Response) {
    let mut reader = BufReader::new(stream);
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).unwrap_or(0) == 0 {
        return;
    }
    let method = request_line.split_whitespace().next().unwrap_or_default().to_owned();

    let mut headers = Vec::new();
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 || line.trim().is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            headers.push((name.trim().to_lowercase(), value.trim().to_owned()));
        }
    }
    let length = headers
        .iter()
        .find(|(name, _)| name == "content-length")
        .and_then(|(_, value)| value.parse::<usize>().ok())
        .unwrap_or(0);
    let mut body = vec![0_u8; length];
    if reader.read_exact(&mut body).is_err() {
        return;
    }

    let recorded = Recorded {
        method,
        headers,
        body: String::from_utf8_lossy(&body).into_owned(),
    };
    log.lock().expect("request log").push(recorded.clone());
    let response = respond(&recorded);

    let mut head = format!(
        "HTTP/1.1 {} Mock\r\ncontent-length: {}\r\nconnection: close\r\n",
        response.status,
        response.body.len()
    );
    if !response
        .headers
        .iter()
        .any(|(name, _)| name.eq_ignore_ascii_case("content-type"))
    {
        head.push_str("content-type: application/json\r\n");
    }
    for (name, value) in &response.headers {
        let _ = write!(head, "{name}: {value}\r\n");
    }
    head.push_str("\r\n");
    let mut stream = reader.into_inner();
    let _ignored = stream.write_all(head.as_bytes());
    let _ignored = stream.write_all(response.body.as_bytes());
    let _ignored = stream.flush();
}
