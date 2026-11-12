//! The test harness: an MCP client that drives the `fieldtrip` binary over stdio, and a mock GraphQL endpoint.

mod api;
mod mock;

use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use tempfile::TempDir;

pub(crate) use self::api::FixtureApi;
pub(crate) use self::mock::{MockServer, Response};

/// How long to wait for the server to answer before failing the test.
const REPLY_TIMEOUT: Duration = Duration::from_secs(20);

/// How the test client answers an approval prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Human {
    /// The client does not declare elicitation support, so no prompt can be shown.
    Absent,
    /// The human accepts every prompt.
    Accepts,
    /// The human declines every prompt.
    Declines,
    /// The human dismisses every prompt.
    Cancels,
    /// The client says it can show prompts, but answers each one with an error.
    Broken,
}

/// The text and error flag of a tool result.
#[derive(Debug)]
pub(crate) struct Reply {
    /// The text content of the result, joined with newlines.
    pub(crate) text: String,
    /// Whether the tool reported an error.
    pub(crate) is_error: bool,
}

/// A running `fieldtrip` process with an initialized MCP session.
#[derive(Debug)]
pub(crate) struct Mcp {
    child: Child,
    stdin: Option<ChildStdin>,
    incoming: Receiver<Value>,
    stderr: Arc<Mutex<String>>,
    stderr_reader: Option<std::thread::JoinHandle<()>>,
    next_id: u64,
    human: Human,
    prompts: Vec<String>,
    dir: TempDir,
}

/// How to start the server for a test.
#[derive(Debug)]
pub(crate) struct Launch {
    config: String,
    human: Human,
    env: Vec<(String, String)>,
}

impl Launch {
    /// A launch with `config` as the config file, no extra environment, and a client that cannot show prompts.
    pub(crate) fn new(config: &str) -> Self {
        Self {
            config: config.to_owned(),
            human: Human::Absent,
            env: Vec::new(),
        }
    }

    /// Sets an environment variable for the server process.
    pub(crate) fn env(mut self, name: &str, value: &str) -> Self {
        self.env.push((name.to_owned(), value.to_owned()));
        self
    }

    /// Makes the client answer approval prompts as `human` does.
    pub(crate) fn human(mut self, human: Human) -> Self {
        self.human = human;
        self
    }

    /// Starts the server and completes the MCP handshake.
    pub(crate) fn start(self) -> Mcp {
        let (dir, config_path) = prepare(&self.config);
        let mut mcp = Mcp::spawn(
            &["--config".as_ref(), config_path.as_os_str()],
            &self.env,
            self.human,
            dir,
        );
        mcp.initialize();
        mcp
    }

    /// Starts the server expecting it to refuse the config, and returns what it wrote to stderr.
    pub(crate) fn fails_to_start(self) -> String {
        let (dir, config_path) = prepare(&self.config);
        let output = server_command(&["--config".as_ref(), config_path.as_os_str()], &self.env, dir.path())
            .stdin(Stdio::null())
            .output()
            .expect("run the fieldtrip binary");
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        assert!(
            !output.status.success(),
            "the server should have refused to start. stderr:\n{stderr}"
        );
        assert!(
            output.stdout.is_empty(),
            "nothing may be written to stdout on a failed start"
        );
        stderr
    }
}

/// Writes `config` into a fresh directory, next to a copy of everything in `tests/fixtures/`.
fn prepare(config: &str) -> (TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("create a temp dir");
    copy_fixtures(dir.path());
    let config_path = dir.path().join("fieldtrip.toml");
    std::fs::write(&config_path, config).expect("write the config file");
    (dir, config_path)
}

/// The command that runs the server in `dir` with `args` and `env`.
fn server_command(args: &[&std::ffi::OsStr], env: &[(String, String)], dir: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_fieldtrip"));
    command
        .args(args)
        .current_dir(dir)
        // The server honors the system proxy settings. Tests talk to a local mock, so they must not see any.
        .env_remove("HTTP_PROXY")
        .env_remove("HTTPS_PROXY")
        .env_remove("ALL_PROXY")
        .env_remove("http_proxy")
        .env_remove("https_proxy")
        .env_remove("all_proxy")
        .envs(env.iter().map(|(name, value)| (name, value)));
    command
}

impl Mcp {
    /// Starts the server with `config` as its config file, and a client that cannot show prompts.
    ///
    /// The config is written into a fresh directory next to a copy of everything in `tests/fixtures/`, so a config
    /// can name a fixture by its relative path.
    pub(crate) fn start(config: &str) -> Self {
        Launch::new(config).start()
    }

    /// Spawns the binary with `args` and `env`, and wires up its three streams.
    fn spawn(args: &[&std::ffi::OsStr], env: &[(String, String)], human: Human, dir: TempDir) -> Self {
        let mut child = server_command(args, env, dir.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn the fieldtrip binary");
        let stdin = child.stdin.take().expect("child stdin");
        let stdout = child.stdout.take().expect("child stdout");
        let mut child_stderr = child.stderr.take().expect("child stderr");

        let (sender, incoming) = channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                // Anything on stdout that is not JSON is a protocol violation, so hand it over as a string and
                // let the test fail on it.
                let message = serde_json::from_str(&line).unwrap_or(Value::String(line));
                if sender.send(message).is_err() {
                    break;
                }
            }
        });

        let stderr = Arc::new(Mutex::new(String::new()));
        let stderr_sink = Arc::clone(&stderr);
        let stderr_reader = std::thread::spawn(move || {
            let mut chunk = [0_u8; 4096];
            while let Ok(read) = child_stderr.read(&mut chunk) {
                if read == 0 {
                    break;
                }
                let text = String::from_utf8_lossy(chunk.get(..read).unwrap_or_default()).into_owned();
                stderr_sink.lock().expect("stderr buffer").push_str(&text);
            }
        });

        Self {
            child,
            stdin: Some(stdin),
            incoming,
            stderr,
            stderr_reader: Some(stderr_reader),
            next_id: 0,
            human,
            prompts: Vec::new(),
            dir,
        }
    }

    /// Performs the MCP handshake.
    fn initialize(&mut self) {
        let capabilities = if self.human == Human::Absent {
            json!({})
        } else {
            json!({ "elicitation": {} })
        };
        let result = self.request(
            "initialize",
            &json!({
                "protocolVersion": "2025-11-25",
                "capabilities": capabilities,
                "clientInfo": { "name": "fieldtrip-spec", "version": "0" },
            }),
        );
        assert!(
            result.get("serverInfo").is_some(),
            "initialize should return serverInfo, got {result}"
        );
        self.send(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));
    }

    /// The directory holding the config file and the fixture copies.
    pub(crate) fn dir(&self) -> &Path {
        self.dir.path()
    }

    /// The names of the tools the server offers.
    pub(crate) fn tools(&mut self) -> Vec<String> {
        let result = self.request("tools/list", &json!({}));
        let tools = result
            .get("tools")
            .and_then(Value::as_array)
            .expect("tools/list returns a tools array");
        tools
            .iter()
            .filter_map(|tool| tool.get("name").and_then(Value::as_str))
            .map(str::to_owned)
            .collect()
    }

    /// Calls `tool` with `arguments` and returns its result.
    pub(crate) fn call(&mut self, tool: &str, arguments: &Value) -> Reply {
        let result = self.request("tools/call", &json!({ "name": tool, "arguments": arguments }));
        let content = result
            .get("content")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let text: Vec<&str> = content
            .iter()
            .filter_map(|block| block.get("text").and_then(Value::as_str))
            .collect();
        Reply {
            text: text.join("\n"),
            is_error: result.get("isError").and_then(Value::as_bool).unwrap_or(false),
        }
    }

    /// The text of every approval prompt the server has shown so far.
    pub(crate) fn prompts(&self) -> &[String] {
        &self.prompts
    }

    /// Ends the session, waits for the server to exit, and returns everything it wrote to stderr.
    pub(crate) fn finish(mut self) -> String {
        drop(self.stdin.take());
        for _ in 0..100 {
            if self.child.try_wait().expect("poll the server process").is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        // A server that ignored the end of its input is killed, which also ends the stderr stream.
        let _ignored = self.child.kill();
        let _ignored = self.child.wait();
        if let Some(reader) = self.stderr_reader.take() {
            reader.join().expect("the stderr reader");
        }
        self.stderr()
    }

    /// Everything the server has written to stderr so far.
    pub(crate) fn stderr(&self) -> String {
        self.stderr.lock().expect("stderr buffer").clone()
    }

    /// Sends a request and returns its `result`, answering any requests the server makes in the meantime.
    fn request(&mut self, method: &str, params: &Value) -> Value {
        self.next_id += 1;
        let id = self.next_id;
        self.send(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
        loop {
            let message = match self.incoming.recv_timeout(REPLY_TIMEOUT) {
                Ok(message) => message,
                Err(RecvTimeoutError::Timeout) => panic!("no reply to `{method}` in time. stderr:\n{}", self.stderr()),
                Err(RecvTimeoutError::Disconnected) => {
                    panic!(
                        "the server exited before replying to `{method}`. stderr:\n{}",
                        self.stderr()
                    )
                }
            };
            assert!(
                message.is_object(),
                "the server wrote something that is not JSON-RPC to stdout: {message}"
            );
            if message.get("method").is_some() {
                self.answer_server(&message);
            } else if message.get("id").and_then(Value::as_u64) == Some(id) {
                if let Some(error) = message.get("error") {
                    panic!("`{method}` failed at the protocol level: {error}");
                }
                return message.get("result").cloned().unwrap_or(Value::Null);
            }
        }
    }

    /// Answers a request the server sent to the client. Notifications are ignored.
    fn answer_server(&mut self, message: &Value) {
        let Some(id) = message.get("id") else { return };
        let method = message.get("method").and_then(Value::as_str).unwrap_or_default();
        let reply = match method {
            "elicitation/create" => {
                let prompt = message
                    .pointer("/params/message")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                self.prompts.push(prompt.to_owned());
                match self.human {
                    Human::Accepts => {
                        json!({ "jsonrpc": "2.0", "id": id, "result": { "action": "accept", "content": {} } })
                    }
                    Human::Declines => json!({ "jsonrpc": "2.0", "id": id, "result": { "action": "decline" } }),
                    Human::Cancels => json!({ "jsonrpc": "2.0", "id": id, "result": { "action": "cancel" } }),
                    Human::Broken => json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "error": { "code": -32603, "message": "the prompt could not be shown" },
                    }),
                    Human::Absent => panic!("the server sent a prompt to a client with no elicitation support"),
                }
            }
            "ping" => json!({ "jsonrpc": "2.0", "id": id, "result": {} }),
            other => json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32601, "message": format!("the test client does not handle `{other}`") },
            }),
        };
        self.send(&reply);
    }

    /// Writes one JSON-RPC message to the server's stdin.
    fn send(&mut self, message: &Value) {
        let stdin = self.stdin.as_mut().expect("the session is open");
        writeln!(stdin, "{message}").expect("write to the server's stdin");
        stdin.flush().expect("flush the server's stdin");
    }
}

impl Drop for Mcp {
    fn drop(&mut self) {
        // Closing stdin ends the session. The kill covers a server that does not notice.
        drop(self.stdin.take());
        let _ignored = self.child.kill();
        let _ignored = self.child.wait();
    }
}

/// Copies every file in `tests/fixtures/` into `dir`.
fn copy_fixtures(dir: &Path) {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("fixtures");
    let Ok(entries) = std::fs::read_dir(&fixtures) else {
        return;
    };
    for entry in entries {
        let entry = entry.expect("read a fixture entry");
        std::fs::copy(entry.path(), dir.join(entry.file_name())).expect("copy a fixture");
    }
}
