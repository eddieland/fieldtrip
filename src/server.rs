//! The MCP server. Each tool is a thin wrapper over the rest of the crate.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::Arc;

use rmcp::handler::server::tool::{InputResponses, RequestState};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResponse, CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerConfig};
use rmcp::service::RequestContext;
use rmcp::{RoleServer, ServerHandler, schemars, tool, tool_handler, tool_router};
use serde::Deserialize;
use tokio::sync::Mutex;

use crate::approval::{self, Operation, Outstanding, Round};
use crate::config::{Config, Endpoint};
use crate::http::Client;
use crate::plan::{Plan, Variables};
use crate::schema::{Loaded, introspection};
use crate::suggest::backticked;

/// What the server tells a client about itself and how to use it.
const INSTRUCTIONS: &str = "fieldtrip lets you work with GraphQL APIs the user has configured. Start with \
     `list_endpoints`. Every other tool takes one of those endpoint names.";

/// What a tool hands back: text for the calling agent, or an error message that says what to do next.
type ToolResult = Result<String, String>;

/// The fieldtrip MCP server.
#[derive(Debug, Clone)]
pub(crate) struct Fieldtrip {
    config: Arc<Config>,
    /// Schemas loaded so far, by endpoint name. The lock is held while loading, so each schema loads once.
    schemas: Arc<Mutex<HashMap<String, Arc<Loaded>>>>,
    http: Client,
    /// Approval prompts that are waiting for an answer, on protocol revisions that ask by round trip.
    outstanding: Arc<Outstanding>,
}

/// Arguments of `describe`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct DescribeArguments {
    /// The endpoint name, as listed by `list_endpoints`.
    endpoint: String,
    /// A type name such as `User`, or a field written as `Type.field`, such as `Query.user`.
    name: String,
}

/// Arguments of `search_schema`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SearchArguments {
    /// The endpoint name, as listed by `list_endpoints`.
    endpoint: String,
    /// One or more words. Each must appear in a name or a description. Case is ignored.
    query: String,
}

/// Arguments of `estimate_cost`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct EstimateArguments {
    /// The endpoint name, as listed by `list_endpoints`.
    endpoint: String,
    /// One GraphQL query or mutation, with any fragments it uses.
    operation: String,
    /// The operation's variables, as a JSON object. They are used to resolve list sizes such as `first: $n`.
    #[serde(default)]
    variables: Variables,
}

/// Arguments of `execute`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ExecuteArguments {
    /// The endpoint name, as listed by `list_endpoints`.
    endpoint: String,
    /// One GraphQL query or mutation, with any fragments it uses.
    operation: String,
    /// The operation's variables, as a JSON object.
    #[serde(default)]
    variables: Variables,
}

/// The most characters a tool result carries. Schemas and responses can be far larger than a caller can use.
const MAX_OUTPUT: usize = 40_000;

/// Cuts `text` to [`MAX_OUTPUT`] characters and says so, with `advice` on how to get a smaller result.
fn capped(text: String, advice: &str) -> String {
    let length = text.chars().count();
    if length <= MAX_OUTPUT {
        return text;
    }
    let mut kept: String = text.chars().take(MAX_OUTPUT).collect();
    let _ = write!(
        kept,
        "\n[fieldtrip cut this off at {MAX_OUTPUT} characters. The full result had at least {length}. {advice}]"
    );
    kept
}

/// A finished tool call with `text` as its result.
fn finished(text: String) -> CallToolResponse {
    CallToolResult::success(vec![ContentBlock::text(text)]).into()
}

/// How much of a response that is not JSON to quote in an error.
const EXCERPT: usize = 300;

/// Arguments of `introspect`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct IntrospectArguments {
    /// The endpoint name, as listed by `list_endpoints`.
    endpoint: String,
    /// Load the schema again from its source, instead of using the copy already loaded.
    #[serde(default)]
    refresh: bool,
}

#[tool_router]
impl Fieldtrip {
    /// Creates a server for `config` that reaches endpoints through `http`.
    pub(crate) fn new(config: Config, http: Client) -> Self {
        Self {
            config: Arc::new(config),
            schemas: Arc::default(),
            http,
            outstanding: Arc::default(),
        }
    }

    #[tool(
        description = "List the GraphQL endpoints the user has configured. Call this first: every other tool takes \
                       one of these endpoint names. Returns each endpoint's name, its URL, where its schema comes \
                       from, and the names of the headers sent with its requests. Header values are never shown."
    )]
    fn list_endpoints(&self) -> String {
        let endpoints = &self.config.endpoints;
        if endpoints.is_empty() {
            return "No endpoints are configured. The user adds them to the fieldtrip config file.".to_owned();
        }
        let mut out = String::new();
        for (name, endpoint) in endpoints {
            let _ = writeln!(out, "{name}");
            let _ = writeln!(out, "  url: {}", endpoint.url);
            match &endpoint.schema {
                Some(path) => {
                    let _ = writeln!(out, "  schema: file {}", path.display());
                }
                None => {
                    let _ = writeln!(out, "  schema: introspection");
                }
            }
            let _ = writeln!(out, "  approval: {}", approval::policy(endpoint));
            if let Some(source) = &endpoint.costs.source {
                let rules = endpoint.costs.len();
                let plural = if rules == 1 { "" } else { "s" };
                let _ = writeln!(out, "  cost map: {} ({rules} rule{plural})", source.display());
            }
            if !endpoint.headers.is_empty() {
                let names: Vec<&str> = endpoint.headers.iter().map(|header| header.name.as_str()).collect();
                let _ = writeln!(out, "  headers: {}", names.join(", "));
            }
        }
        out
    }

    #[tool(
        description = "Show the exact definition of one type or one field, as compact GraphQL SDL. Use it before \
                       writing an operation, to get field names, argument names and types right. `name` is a type \
                       (`User`) or a field (`Query.user`, `Mutation.createOrder`). For a field, the result also \
                       includes its argument types and its return type."
    )]
    async fn describe(&self, Parameters(arguments): Parameters<DescribeArguments>) -> ToolResult {
        let schema = self.schema(&arguments.endpoint, false).await?;
        let definition = schema.describe(&arguments.name)?;
        Ok(capped(
            definition,
            "Describe one field at a time, as `Type.field`, or use `search_schema`.",
        ))
    }

    #[tool(
        description = "Search an endpoint's schema for queries, mutations, types, fields, arguments and enum values. \
                       Matches names and descriptions, ignoring case, and returns the best matches as one-line \
                       signatures. Use it to find the real name of something before you `describe` it. Use short \
                       queries: one word, or a type name and a field name."
    )]
    async fn search_schema(&self, Parameters(arguments): Parameters<SearchArguments>) -> ToolResult {
        let schema = self.schema(&arguments.endpoint, false).await?;
        schema.search(&arguments.endpoint, &arguments.query)
    }

    #[tool(
        description = "Check a query or mutation against the schema and estimate its cost, without sending it. Use \
                       it while writing an operation: it reports validation errors with positions, or the estimated \
                       cost, the depth, the cost of each top-level field, and the assumptions behind the estimate. \
                       Pass exactly one operation. The estimate is a heuristic, not the server's own accounting."
    )]
    async fn estimate_cost(&self, Parameters(arguments): Parameters<EstimateArguments>) -> ToolResult {
        let name = &arguments.endpoint;
        let endpoint = self.endpoint(name)?;
        let schema = self.schema(name, false).await?;
        let plan = Plan::new(&schema, name, endpoint, &arguments.operation, &arguments.variables)?;
        Ok(format!("Valid {} for `{name}`.\n{}", plan.title(), plan.estimate()))
    }

    #[tool(
        description = "Send a query or mutation to the endpoint and return the GraphQL response. The user is shown \
                       the exact operation, its variables and its estimated cost, and it is sent only if they \
                       approve. Check the operation with `estimate_cost` first. If the user declines, do not send \
                       the same operation again: ask them what they want. Pass exactly one operation."
    )]
    async fn execute(
        &self,
        context: RequestContext<RoleServer>,
        RequestState(state): RequestState,
        InputResponses(responses): InputResponses,
        Parameters(arguments): Parameters<ExecuteArguments>,
    ) -> Result<CallToolResponse, String> {
        let name = &arguments.endpoint;
        let endpoint = self.endpoint(name)?;
        let schema = self.schema(name, false).await?;
        let plan = Plan::new(&schema, name, endpoint, &arguments.operation, &arguments.variables)?;
        plan.check_variables(&schema, &arguments.variables)?;

        // Nothing below this point runs unless the human accepted, or the user's config waives the prompt.
        let unasked = if let Some(reason) = approval::waiver(endpoint, &plan) {
            format!(" The user was not asked: {reason}.")
        } else {
            let prompt = approval::prompt(name, &endpoint.url, &plan, &arguments.variables);
            let decision = if approval::asks_by_round_trip(&context) {
                let operation = Operation {
                    endpoint: name,
                    text: &plan.text,
                    variables: &arguments.variables,
                };
                match self
                    .outstanding
                    .round(&context, &operation, prompt, state.as_deref(), responses.as_ref())
                {
                    Round::Ask(question) => return Ok(question.into()),
                    Round::Decided(decision) => decision,
                }
            } else {
                approval::ask(&context.peer, prompt).await
            };
            decision.into_result(name)?;
            String::new()
        };

        let reply = self.http.post(endpoint, &plan.payload(&arguments.variables)).await?;
        let summary = format!(
            "Sent {} to `{name}`. HTTP {}. Estimated cost: {}.{unasked}",
            plan.title(),
            reply.status,
            plan.cost
        );
        let text = capped(
            format!("{summary}\n{}", reply.body.trim()),
            "Ask for less: fewer fields, or a smaller page with `first` or `limit`.",
        );
        if reply.cut_short {
            // Too long to read in full, so it cannot be parsed. The status says whether it is a result.
            return if reply.status < 400 {
                Ok(finished(text))
            } else {
                Err(text)
            };
        }
        let Ok(serde_json::Value::Object(response)) = serde_json::from_str::<serde_json::Value>(&reply.body) else {
            let excerpt: String = reply.body.chars().take(EXCERPT).collect();
            return Err(format!("{summary}\nThe response was not a GraphQL response: {excerpt}"));
        };
        let has_data = response.get("data").is_some_and(|data| !data.is_null());
        if has_data { Ok(finished(text)) } else { Err(text) }
    }

    #[tool(
        description = "Load an endpoint's schema and get an overview of it: how many types it has and the names of \
                       its queries and mutations. Use it once per endpoint to see what the API offers, then \
                       `describe` the fields you need. The schema stays loaded. Pass `refresh` only when the API has \
                       changed."
    )]
    async fn introspect(&self, Parameters(arguments): Parameters<IntrospectArguments>) -> ToolResult {
        let name = &arguments.endpoint;
        let schema = self.schema(name, arguments.refresh).await?;
        let endpoint = self.endpoint(name)?;
        let source = endpoint.schema.as_ref().map_or_else(
            || format!("loaded by introspection from {}", endpoint.url),
            |path| format!("read from the file {}", path.display()),
        );
        Ok(format!("Schema for `{name}`, {source}.\n{}", schema.overview()))
    }
}

impl Fieldtrip {
    /// Looks up an endpoint by name.
    fn endpoint(&self, name: &str) -> Result<&Endpoint, String> {
        self.config.endpoints.get(name).ok_or_else(|| {
            let names = self.config.endpoints.keys().map(String::as_str);
            format!(
                "There is no endpoint named `{name}`. Configured endpoints: {}.",
                backticked(names)
            )
        })
    }

    /// Returns the schema for the endpoint called `name`, loading it on first use or when `refresh` is set.
    async fn schema(&self, name: &str, refresh: bool) -> Result<Arc<Loaded>, String> {
        let endpoint = self.endpoint(name)?;
        let mut schemas = self.schemas.lock().await;
        if let Some(loaded) = schemas.get(name).filter(|_| !refresh) {
            return Ok(Arc::clone(loaded));
        }
        let loaded = Arc::new(self.load(name, endpoint).await?);
        schemas.insert(name.to_owned(), Arc::clone(&loaded));
        drop(schemas);
        Ok(loaded)
    }

    /// Loads the schema for `endpoint` from its file, or by introspection when it has none.
    async fn load(&self, name: &str, endpoint: &Endpoint) -> Result<Loaded, String> {
        let Some(path) = &endpoint.schema else {
            let sdl = introspection::fetch(&self.http, endpoint).await?;
            return Loaded::from_sdl(&sdl, &format!("The introspection result from {}", endpoint.url));
        };
        let text = std::fs::read_to_string(path).map_err(|error| {
            format!(
                "Could not read the schema file {} for endpoint `{name}`: {error}",
                path.display()
            )
        })?;
        Loaded::from_sdl(&text, &format!("The schema file {}", path.display()))
    }
}

#[expect(
    clippy::unused_async_trait_impl,
    reason = "the async fn is generated by rmcp's `tool_handler` macro"
)]
#[tool_handler]
impl ServerHandler for Fieldtrip {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new(env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION")))
            .with_instructions(INSTRUCTIONS)
    }
}
