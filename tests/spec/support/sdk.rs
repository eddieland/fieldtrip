//! A second test client, built on the MCP SDK's own client, that speaks protocol revision 2026-07-28.
//!
//! That revision has no `initialize` handshake, and a server asks for input by answering a tool call with an
//! "input required" result that the client fulfils and retries. The hand-written client in this module's parent
//! speaks the older handshake, so the two together cover both ways an approval prompt can travel.

use std::process::Stdio;
use std::sync::{Arc, Mutex};

use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ClientCapabilities, ClientConfig, ElicitRequestParams,
    ElicitResult, ElicitationAction, ErrorData, Implementation, InputResponses, ProtocolVersion,
};
use rmcp::service::{RequestContext, RoleClient, RunningService};
use rmcp::transport::TokioChildProcess;
use rmcp::{ClientHandler, ClientLifecycleMode, ClientServiceExt as _};
use serde_json::Value;
use tempfile::TempDir;

use super::{Human, Launch, Reply, server_command};

/// The client side of the session: it answers approval prompts as a [`Human`] does and records them.
#[derive(Debug, Clone)]
struct Handler {
    human: Human,
    client: String,
    prompts: Arc<Mutex<Vec<String>>>,
}

impl ClientHandler for Handler {
    fn create_elicitation(
        &self,
        request: ElicitRequestParams,
        _context: RequestContext<RoleClient>,
    ) -> impl Future<Output = Result<ElicitResult, ErrorData>> + Send + '_ {
        if let ElicitRequestParams::FormElicitationParams { message, .. } = &request {
            self.prompts.lock().expect("prompt log").push(message.clone());
        }
        std::future::ready(match self.human {
            Human::Accepts => Ok(ElicitResult::new(ElicitationAction::Accept).with_content(serde_json::json!({}))),
            Human::Declines => Ok(ElicitResult::new(ElicitationAction::Decline)),
            Human::Cancels => Ok(ElicitResult::new(ElicitationAction::Cancel)),
            Human::Broken => Err(ErrorData::internal_error("the prompt could not be shown", None)),
            Human::Absent => panic!("the server sent a prompt to a client with no elicitation support"),
        })
    }

    fn get_info(&self) -> ClientConfig {
        let capabilities = if self.human == Human::Absent {
            ClientCapabilities::default()
        } else {
            ClientCapabilities::builder().enable_elicitation().build()
        };
        ClientConfig::new(capabilities, Implementation::new(self.client.clone(), "0"))
    }
}

/// A running `fieldtrip` process with a 2026-07-28 session on it.
#[derive(Debug)]
pub(crate) struct SdkClient {
    runtime: tokio::runtime::Runtime,
    service: RunningService<RoleClient, Handler>,
    prompts: Arc<Mutex<Vec<String>>>,
    // Held so that the directory with the config and fixtures outlives the server.
    _dir: TempDir,
}

impl SdkClient {
    /// Starts the server as `launch` describes and opens a session on revision 2026-07-28.
    pub(crate) fn start(launch: &Launch) -> Self {
        let (dir, arguments, env) = launch.prepare();
        let arguments: Vec<&std::ffi::OsStr> = arguments.iter().map(AsRef::as_ref).collect();
        let command = tokio::process::Command::from(server_command(&arguments, &env, dir.path()));
        let prompts = Arc::new(Mutex::new(Vec::new()));
        let handler = Handler {
            human: launch.human,
            client: launch.client.clone(),
            prompts: Arc::clone(&prompts),
        };

        let runtime = tokio::runtime::Runtime::new().expect("create a runtime");
        let service = runtime.block_on(async {
            let (transport, _stderr) = TokioChildProcess::builder(command)
                .stderr(Stdio::null())
                .spawn()
                .expect("spawn the fieldtrip binary");
            let lifecycle = ClientLifecycleMode::Discover {
                preferred_versions: vec![ProtocolVersion::V_2026_07_28],
            };
            handler
                .serve_with_lifecycle(transport, lifecycle)
                .await
                .expect("open a 2026-07-28 session")
        });
        Self {
            runtime,
            service,
            prompts,
            _dir: dir,
        }
    }

    /// Calls `tool`, fulfilling any input the server asks for, and returns the final result.
    pub(crate) fn call(&self, tool: &'static str, arguments: &Value) -> Reply {
        let result = self
            .runtime
            .block_on(self.service.call_tool(params(tool, arguments)))
            .expect("the tool call");
        reply(&result)
    }

    /// Sends one `tools/call` without fulfilling anything, and returns whatever the server answered.
    pub(crate) fn call_once(&self, tool: &'static str, arguments: &Value, retry: Option<Retry>) -> CallToolResponse {
        let mut params = params(tool, arguments);
        if let Some(retry) = retry {
            params = params.with_input_responses(retry.responses);
            if let Some(state) = retry.state {
                params = params.with_request_state(state);
            }
        }
        self.runtime
            .block_on(self.service.call_tool_once(params))
            .expect("the tool call")
    }

    /// The text of every approval prompt the client has been shown.
    pub(crate) fn prompts(&self) -> Vec<String> {
        self.prompts.lock().expect("prompt log").clone()
    }
}

/// What a client sends back on the retry of a call that asked for input.
#[derive(Debug)]
pub(crate) struct Retry {
    /// The answers, by the keys the server asked under.
    pub(crate) responses: InputResponses,
    /// The opaque state the server handed out, echoed back.
    pub(crate) state: Option<String>,
}

/// The text and error flag of a finished tool call.
pub(crate) fn reply(result: &CallToolResult) -> Reply {
    let text: Vec<String> = result
        .content
        .iter()
        .filter_map(|block| serde_json::to_value(block).ok())
        .filter_map(|block| block.get("text").and_then(Value::as_str).map(str::to_owned))
        .collect();
    Reply {
        text: text.join("\n"),
        is_error: result.is_error.unwrap_or(false),
    }
}

/// The parameters of a `tools/call` for `tool` with `arguments`, which must be a JSON object.
fn params(tool: &'static str, arguments: &Value) -> CallToolRequestParams {
    let arguments = arguments
        .as_object()
        .cloned()
        .expect("tool arguments are a JSON object");
    CallToolRequestParams::new(tool).with_arguments(arguments)
}
