use std::collections::HashMap;
use std::io::Read;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use tiny_http::{Header, Method, Response, Server};

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct RequestCounts {
    pub playlists: usize,
    pub video_fragments: usize,
    pub audio_fragments: usize,
}

#[derive(Default)]
struct GateState {
    /// Fragment requests for the gate's video seen since arming, counted
    /// before any hold; `ordinal` selects which one to hold.
    seen: usize,
    engaged: bool,
    released: bool,
}

struct Gate {
    video_n: usize,
    /// Which fragment request (1-based, video and audio counted together) to
    /// hold, counted from arm time — `1` holds the next one, whatever traffic
    /// came before arming.
    ordinal: usize,
    state: Mutex<GateState>,
    cv: Condvar,
}

struct State {
    root: PathBuf,
    counts: Mutex<HashMap<usize, RequestCounts>>,
    gate: Mutex<Option<Arc<Gate>>>,
}

pub struct TestServer {
    addr: SocketAddr,
    state: Arc<State>,
    server: Arc<Server>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl TestServer {
    pub fn start(root: &Path) -> TestServer {
        let server = Arc::new(Server::http("127.0.0.1:0").expect("bind test server"));
        let addr = server.server_addr().to_ip().expect("ip addr");
        let state = Arc::new(State {
            root: root.to_path_buf(),
            counts: Mutex::new(HashMap::new()),
            gate: Mutex::new(None),
        });
        let handle = {
            let listener = Arc::clone(&server);
            let state = Arc::clone(&state);
            std::thread::spawn(move || {
                for request in listener.incoming_requests() {
                    // One thread per request: a held gate must not stall the accept loop.
                    let state = Arc::clone(&state);
                    std::thread::spawn(move || serve(state, request));
                }
            })
        };
        TestServer {
            addr,
            state,
            server,
            handle: Some(handle),
        }
    }

    pub fn url(&self, n: usize, rel: &str) -> String {
        format!("http://{}/v/{n}/{rel}", self.addr)
    }

    pub fn counts(&self, n: usize) -> RequestCounts {
        self.state
            .counts
            .lock()
            .unwrap()
            .get(&n)
            .copied()
            .unwrap_or_default()
    }

    /// Hold the `ordinal`-th fragment request (1-based, video and audio
    /// counted together) for video `n`; the counting happens before the hold.
    /// One-shot, 15s bounded — see `serve`.
    pub fn arm_gate_on_nth_fragment(&self, n: usize, ordinal: usize) {
        let gate = Arc::new(Gate {
            video_n: n,
            ordinal,
            state: Mutex::new(GateState::default()),
            cv: Condvar::new(),
        });
        *self.state.gate.lock().unwrap() = Some(gate);
    }

    /// Thin wrapper over `arm_gate_on_nth_fragment(n, 1)`: hold the next
    /// fragment request for video `n`.
    pub fn arm_gate_on_next_fragment(&self, n: usize) {
        self.arm_gate_on_nth_fragment(n, 1);
    }

    pub async fn wait_gate_engaged(&self, timeout: Duration) -> bool {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if let Some(g) = self.state.gate.lock().unwrap().clone()
                && g.state.lock().unwrap().engaged
            {
                return true;
            }
            if tokio::time::Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    pub fn release_gate(&self) {
        if let Some(g) = self.state.gate.lock().unwrap().take() {
            g.state.lock().unwrap().released = true;
            g.cv.notify_all();
        }
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        // The accept loop parks inside `incoming_requests`; `unblock` wakes it so
        // the iterator can finish and the thread can join. Without this the drop
        // would join a thread that never returns and hang the suite.
        self.release_gate();
        self.server.unblock();
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

fn classify(rel: &str) -> Option<&'static str> {
    if rel.ends_with(".m3u8") {
        return Some("playlist");
    }
    let stem = rel.trim_end_matches(".ts");
    if stem.len() > 1 && stem[1..].bytes().all(|b| b.is_ascii_digit()) {
        match &stem[..1] {
            "v" => return Some("video"),
            "a" => return Some("audio"),
            _ => {}
        }
    }
    None
}

fn serve(state: Arc<State>, request: tiny_http::Request) {
    let path = request.url().to_string();
    // /v/<n>/<rel>
    let mut parts = path.trim_start_matches('/').splitn(3, '/');
    let (Some("v"), Some(n), Some(rel)) = (parts.next(), parts.next(), parts.next()) else {
        let _ = request.respond(Response::empty(404));
        return;
    };
    let Ok(n) = n.parse::<usize>() else {
        let _ = request.respond(Response::empty(404));
        return;
    };

    if let Some(kind) = classify(rel) {
        let mut counts = state.counts.lock().unwrap();
        let entry = counts.entry(n).or_default();
        match kind {
            "playlist" => entry.playlists += 1,
            "video" => entry.video_fragments += 1,
            _ => entry.audio_fragments += 1,
        }
        drop(counts);
        // Gate: hold the armed fragment request for the armed video, bounded so
        // a stuck test can never hang CI forever (well under yt-dlp's socket timeout).
        if kind != "playlist" {
            let gate = state.gate.lock().unwrap().clone();
            if let Some(g) = gate
                && g.video_n == n
            {
                let mut st = g.state.lock().unwrap();
                // Count requests as they arrive (before any hold): the ordinal
                // is relative to arm time, so an arm placed after earlier
                // traffic still holds exactly the n-th request from then on.
                st.seen += 1;
                if !st.engaged && st.seen == g.ordinal {
                    st.engaged = true;
                    g.cv.notify_all();
                    while !st.released {
                        let (guard, timeout) =
                            g.cv.wait_timeout(st, Duration::from_secs(15)).unwrap();
                        st = guard;
                        if timeout.timed_out() {
                            break;
                        }
                    }
                }
            }
        }
    }

    let file = state.root.join(rel);
    if !file.is_file() {
        let _ = request.respond(Response::empty(404));
        return;
    }

    if request.method() == &Method::Head {
        let len = std::fs::metadata(&file).map(|m| m.len()).unwrap_or(0);
        let mut response = Response::empty(200);
        response.add_header(Header::from_bytes("Content-Length", len.to_string()).unwrap());
        let _ = request.respond(response);
        return;
    }

    // Range support is out of scope here: answer 200 with the full body (HTTP allows ignoring Range; yt-dlp tolerates it).
    let mut body = Vec::new();
    std::fs::File::open(&file)
        .unwrap()
        .read_to_end(&mut body)
        .unwrap();
    let _ = request.respond(Response::from_data(body));
}

// --- minimal test client ---

pub fn get_text(url: &str) -> String {
    let raw = request(url, "GET", &[]);
    raw.split("\r\n\r\n").nth(1).unwrap_or_default().to_string()
}

pub fn request(url: &str, method: &str, headers: &[(&str, &str)]) -> String {
    use std::io::Write;
    let rest = url.strip_prefix("http://").expect("http url");
    let (host, path) = rest.split_once('/').unwrap();
    let mut stream = std::net::TcpStream::connect(host).expect("connect");
    let mut req = format!("{method} /{path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n");
    for (k, v) in headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    req.push_str("\r\n");
    stream.write_all(req.as_bytes()).unwrap();
    // Bodies include binary .ts payloads, and `read_to_string` blanks the buffer
    // on invalid UTF-8: read raw bytes and convert lossily instead.
    let mut bytes = Vec::new();
    let _ = stream.read_to_end(&mut bytes);
    String::from_utf8_lossy(&bytes).into_owned()
}
