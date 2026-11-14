//! The MCP server. Each tool is a thin wrapper over the rest of the crate.

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::sync::Arc;

use rmcp::handler::server::tool::{InputResponses, RequestState};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResponse, CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerConfig};
use rmcp::service::RequestContext;
use rmcp::{RoleServer, ServerHandler, schemars, tool, tool_handler, tool_router};
use serde::Deserialize;
use serde_json::value::RawValue;
use tokio::sync::Mutex;

use crate::approval::{self, Operation, Outstanding, Round};
use crate::config::{Config, Endpoint, Mutations, show_bytes};
use crate::http::Client;
use crate::library::{self, Saved};
use crate::plan::{Kind, Plan, Variables};
use crate::schema::{Loaded, introspection};
use crate::suggest::backticked;
use crate::{response, saved, select};

/// What the server tells a client about itself and how to use it.
const INSTRUCTIONS: &str = "fieldtrip lets you use GraphQL APIs the user has configured, reading the real schema \
     instead of guessing it. Work in this order:\n\
     1. `list_endpoints` gives the endpoint names. Every other tool takes one.\n\
     2. `list_operations` shows the operations the user has saved and trusts for an endpoint. When one does what you \
     need, run it by its name instead of writing your own, and skip to step 6.\n\
     3. `introspect` once per endpoint shows its query and mutation fields, the API's entry points.\n\
     4. `search_schema` finds a type, field or saved operation by a word. `describe` shows a field's exact \
     arguments and return type.\n\
     5. Write one operation and check it with `estimate_cost` until it is valid and cheap. Nothing is sent.\n\
     6. `execute` sends it, or a saved operation named by `operation_name`. The user approves each request unless their config says otherwise.\n\
     GraphQL habits that matter here: take every field and argument name from the schema, never from memory. \
     Select only the fields you need, since there is no wildcard. Name the operation and pass inputs as \
     variables, not as values pasted into the text: the user reads both when approving. Give lists a page size, \
     such as `first` or `limit`, because each list multiplies the cost of everything under it. Avoid fields \
     marked deprecated, whose reason names the replacement.";

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
    /// The type or field to describe.
    #[schemars(
        description = "A type name such as `User`, or a field written as `Type.field`, such as `Query.user` or \
                       `User.orders`. Names are case-sensitive, as in the schema."
    )]
    name: String,
}

/// Arguments of `search_schema`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SearchArguments {
    /// The endpoint name, as listed by `list_endpoints`.
    endpoint: String,
    /// The words to search for.
    #[schemars(
        description = "One or more words separated by spaces, such as `refund` or `user email`. Every word must \
                       match. Case is ignored."
    )]
    query: String,
}

/// How `estimate_cost` and `execute` describe their `operation` argument to the calling agent.
const OPERATION: &str = "A GraphQL document holding exactly one query or mutation, plus any fragments it uses. \
     Name the operation and declare its inputs as variables, such as \
     `query UserName($id: ID!) { user(id: $id) { name } }`. Subscriptions are not supported. Leave it out when you \
     give `operation_name`.";

/// How `estimate_cost` and `execute` describe their `operation_name` argument to the calling agent.
const OPERATION_NAME: &str = "The name of an operation the user saved for this endpoint, as `list_operations` shows \
     it, such as `OrderById`. Use it instead of `operation`, and pass the variables it declares. Its saved text is \
     what is costed, shown and sent.";

/// Arguments of `list_operations`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ListOperationsArguments {
    /// The endpoint name, as listed by `list_endpoints`.
    endpoint: String,
    /// Words to filter by.
    #[schemars(
        description = "Optional. One or more words separated by spaces, such as `order` or `refund payment`. Only \
                       operations whose name or description holds every word are listed. Case is ignored. Leave it \
                       out to list them all."
    )]
    query: Option<String>,
}

/// Arguments of `estimate_cost`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct EstimateArguments {
    /// The endpoint name, as listed by `list_endpoints`.
    endpoint: String,
    /// The GraphQL document.
    #[schemars(description = OPERATION)]
    operation: Option<String>,
    /// The name of a saved operation.
    #[schemars(description = OPERATION_NAME)]
    operation_name: Option<String>,
    /// The variables, used to size lists.
    #[schemars(
        description = "Values for the variables the operation declares, as a JSON object keyed by name without \
                       the `$`. Here they size lists given as `first: $n`, so pass the same values you will pass \
                       to `execute`."
    )]
    #[serde(default)]
    variables: Variables,
}

/// Arguments of `execute`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ExecuteArguments {
    /// The endpoint name, as listed by `list_endpoints`.
    endpoint: String,
    /// The GraphQL document.
    #[schemars(description = OPERATION)]
    operation: Option<String>,
    /// The name of a saved operation.
    #[schemars(description = OPERATION_NAME)]
    operation_name: Option<String>,
    /// The variables sent with the operation.
    #[schemars(
        description = "Values for the variables the operation declares, as a JSON object keyed by name without \
                       the `$`, such as `{\"id\": \"42\", \"first\": 10}`. Write an enum value as a JSON string and \
                       an input object as a JSON object. They are checked against the declared types before the \
                       user is asked."
    )]
    #[serde(default)]
    variables: Variables,
    /// A path that picks part of the response.
    #[schemars(
        description = "Optional. A path that picks part of the response once it arrives, such as \
                       `data.repository.pullRequests.nodes[*].number`. It starts at the top of the response: \
                       `data`, `errors` or `extensions`. Names are joined by `.`, and an alias in the operation is \
                       the name in the response. `[0]` takes one item of a list, and `[*]` takes every item, which \
                       makes the result a list of every match. `errors` is still shown in full. It changes \
                       neither what is sent nor what the user approves: to fetch less, change the operation."
    )]
    select: Option<String>,
    /// Whether to save the whole response to a file.
    #[schemars(
        description = "Optional. Set to `true` to write the whole response to a new file that fieldtrip names, and \
                       get back its path and size with a short preview instead of the response. Use it when the \
                       response may be large and you can read files, or need all of it but not in your context. It \
                       changes neither what is sent nor what the user approves."
    )]
    #[serde(default)]
    save: bool,
}

/// Cuts `text` to `limit` characters and says so, with `advice` on how to get a smaller result.
fn capped(text: String, limit: usize, advice: &str) -> String {
    let length = text.chars().count();
    if length <= limit {
        return text;
    }
    let mut kept: String = text.chars().take(limit).collect();
    let _ = write!(
        kept,
        "\n[fieldtrip cut this off at {limit} characters. The full result had at least {length}. {advice}]"
    );
    kept
}

/// Like [`capped`], but `head` is always kept whole, and only `rest`, which follows it, is cut.
fn capped_after(head: String, rest: &str, limit: usize, advice: &str) -> String {
    let head_length = head.chars().count();
    let length = head_length + rest.chars().count();
    if length <= limit {
        return head + rest;
    }
    let mut kept = head;
    kept.extend(rest.chars().take(limit.saturating_sub(head_length)));
    let _ = write!(
        kept,
        "\n[fieldtrip cut this off at {limit} characters. The full result had at least {length}. Errors are never \
         cut. {advice}]"
    );
    kept
}

/// The most characters shown of a response that was saved to a file.
const SAVED_PREVIEW: usize = 4_000;

/// Room kept in a result for the note that follows a summary.
const NOTE_ROOM: usize = 500;

/// The result of `execute`: `head`, which ends with the errors, then each of the `rest` of the response's parts as
/// the server wrote it when they fit in the `room`, or a summary of them that is still valid JSON when not.
fn shown(head: String, rest: &[(String, Box<RawValue>)], room: response::Room) -> String {
    let limit = room.limit();
    let mut written = String::new();
    for (name, value) in rest {
        let _ = write!(written, "\n{name}: {}", value.get().trim());
    }
    let length = head.chars().count() + written.chars().count();
    if length <= limit {
        return head + &written;
    }
    let parts: serde_json::Map<String, serde_json::Value> = rest
        .iter()
        .filter_map(|(name, value)| Some((name.clone(), serde_json::from_str(value.get()).ok()?)))
        .collect();
    let space = limit.saturating_sub(head.chars().count() + NOTE_ROOM);
    match response::summarize(&parts, space) {
        Some(summary) => {
            let note = response::note(&summary, length, room);
            format!("{head}{}\n{note}", summary.lines)
        }
        None => capped_after(head, &written, limit, ASK_FOR_LESS),
    }
}

/// How `execute` says to get a smaller result.
const ASK_FOR_LESS: &str = "Ask for less: fewer fields, or a smaller page with `first` or `limit`.";

/// A finished tool call with `text` as its result.
fn finished(text: String) -> CallToolResponse {
    CallToolResult::success(vec![ContentBlock::text(text)]).into()
}

/// The text of the operation a call names: `operation` as given, or the saved operation called `operation_name`,
/// which the second item is then.
fn chosen<'call>(
    name: &str,
    endpoint: &'call Endpoint,
    operation: Option<&'call str>,
    operation_name: Option<&str>,
) -> Result<(&'call str, Option<&'call Saved>), String> {
    match (operation, operation_name) {
        (Some(_), Some(_)) => Err(
            "Give either `operation`, the GraphQL text, or `operation_name`, the name of a saved operation, not both."
                .to_owned(),
        ),
        (Some(text), None) => Ok((text, None)),
        (None, Some(wanted)) => {
            let saved = endpoint.operations.get(name, wanted)?;
            Ok((&saved.text, Some(saved)))
        }
        (None, None) => Err(
            "Give `operation`, the GraphQL text, or `operation_name`, the name of an operation from \
             `list_operations`."
                .to_owned(),
        ),
    }
}

/// Plans `text` for the endpoint called `name`. When it is the `saved` operation, an invalid one is reported as
/// broken, since the calling agent did not write it.
fn planned(
    schema: &Loaded,
    name: &str,
    endpoint: &Endpoint,
    text: &str,
    saved: Option<&Saved>,
    variables: &Variables,
) -> Result<Plan, String> {
    Plan::new(schema, name, endpoint, text, variables).map_err(|error| match saved {
        Some(saved) => saved.broken(name, &error),
        None => error,
    })
}

/// Writes one saved operation for `list_operations`.
fn listing(out: &mut String, schema: &Loaded, name: &str, endpoint: &Endpoint, saved: &Saved) {
    let _ = writeln!(out, "{} ({})", saved.name, saved.kind.keyword());
    if let Some(description) = &saved.description {
        let _ = writeln!(out, "  description: {description}");
    }
    let variables = if saved.variables.is_empty() {
        "none".to_owned()
    } else {
        saved.variables.join(", ")
    };
    let _ = writeln!(out, "  variables: {variables}");
    match saved.plan(schema, name, endpoint) {
        Ok(plan) => {
            let note = if plan.assumptions.is_empty() {
                ""
            } else {
                ", with sizes assumed: `estimate_cost` with `operation_name` and your variables gives a closer figure"
            };
            let _ = writeln!(out, "  estimated cost: {}{note}", plan.cost);
            if saved.kind == Kind::Mutation && endpoint.mutations == Mutations::Deny {
                out.push_str("  `execute` refuses it here: the user's config denies mutations for this endpoint.\n");
            }
            for line in plan.text.lines() {
                let _ = writeln!(out, "  {line}");
            }
        }
        Err(error) => {
            let _ = writeln!(
                out,
                "  BROKEN: it no longer validates against the schema, so `execute` will not run it. Tell the user, \
                 who can fix {}.",
                saved.file.display()
            );
            for problem in library::problems(&error) {
                let _ = writeln!(out, "  - {problem}");
            }
        }
    }
}

/// How much of a response that is not JSON to quote in an error.
const EXCERPT: usize = 300;

/// Arguments of `introspect`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct IntrospectArguments {
    /// The endpoint name, as listed by `list_endpoints`.
    endpoint: String,
    /// Whether to load the schema again.
    #[schemars(
        description = "Set to `true` to load the schema again from its source, after the API has changed. \
                       Otherwise the copy already loaded is used."
    )]
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
        title = "List GraphQL endpoints",
        description = "List the GraphQL endpoints the user has configured. Call this first: every other tool takes \
                       one of these endpoint names, and fieldtrip talks to no other host. For each endpoint it \
                       returns the user's description of it when there is one, the URL, where the schema comes \
                       from (a file or introspection), who approves `execute` requests, `mutations: denied` when \
                       `execute` refuses every mutation there, the cost map if there is one, how many operations \
                       the user has saved for `list_operations`, and the names of the headers sent with each request, so you can tell whether \
                       authentication is set up. Header values are never shown. When several endpoints are \
                       instances of one service, such as dev and prod, prefer the one the description or the user \
                       points to, and ask when it is unclear. Endpoints and headers come only from the user's \
                       config: if one is missing, ask the user to add it.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    fn list_endpoints(&self) -> String {
        let endpoints = &self.config.endpoints;
        if endpoints.is_empty() {
            return "No endpoints are configured. The user adds them to the fieldtrip config file.".to_owned();
        }
        let mut out = String::new();
        for (name, endpoint) in endpoints {
            let _ = writeln!(out, "{name}");
            if let Some(description) = &endpoint.description {
                let _ = writeln!(out, "  description: {description}");
            }
            let _ = writeln!(out, "  url: {}", endpoint.shown_url);
            match &endpoint.schema {
                Some(path) => {
                    let _ = writeln!(out, "  schema: file {}", path.display());
                }
                None => {
                    let _ = writeln!(out, "  schema: introspection");
                }
            }
            let _ = writeln!(out, "  approval: {}", approval::policy(endpoint));
            if endpoint.mutations == Mutations::Deny {
                let _ = writeln!(out, "  mutations: denied");
            }
            if let Some(source) = &endpoint.costs.source {
                let rules = endpoint.costs.len();
                let plural = if rules == 1 { "" } else { "s" };
                let _ = writeln!(out, "  cost map: {} ({rules} rule{plural})", source.display());
            }
            if let Some(source) = &endpoint.operations.source {
                let count = endpoint.operations.operations.len();
                let _ = writeln!(
                    out,
                    "  saved operations: {count} in {} (see `list_operations`)",
                    source.display()
                );
            }
            if !endpoint.headers.is_empty() {
                let names: Vec<&str> = endpoint.headers.iter().map(|header| header.name.as_str()).collect();
                let _ = writeln!(out, "  headers: {}", names.join(", "));
            }
            if !endpoint.response_headers.is_empty() {
                let _ = writeln!(
                    out,
                    "  response headers shown: {}",
                    endpoint.response_headers.join(", ")
                );
            }
        }
        out
    }

    #[tool(
        title = "List saved GraphQL operations",
        description = "List the operations the user has saved for an endpoint: named queries and mutations they \
                       already trust, kept as `.graphql` files. Call it after `list_endpoints` shows saved \
                       operations, before writing your own. When one does what you need, run it with `execute` and \
                       `operation_name` rather than writing it again: it is known to work, and its cost is known. \
                       For each operation it returns the name, whether it is a query or a mutation, the user's \
                       description, the variables it declares, its estimated cost and its text, which is also a \
                       good model for an operation of your own. One that no longer validates against the schema is \
                       marked BROKEN with what is wrong, and `execute` will not run it. Pass `query` to see only the \
                       operations whose name or description holds those words.",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    async fn list_operations(&self, Parameters(arguments): Parameters<ListOperationsArguments>) -> ToolResult {
        let name = &arguments.endpoint;
        let endpoint = self.endpoint(name)?;
        let library = &endpoint.operations;
        let Some(source) = &library.source else {
            return Ok(format!(
                "`{name}` has no saved operations. The user can add a directory of `.graphql` files, one named \
                 operation each, with `operations` in the fieldtrip config. Meanwhile write the operation yourself, \
                 starting from `introspect`.\n"
            ));
        };
        if library.is_empty() {
            return Ok(format!(
                "`{name}` has no saved operations: {} holds no `.graphql` files. Write the operation yourself, \
                 starting from `introspect`.\n",
                source.display()
            ));
        }
        let schema = self.schema(name, false).await?;
        let total = library.operations.len();
        let query = arguments.query.as_deref().unwrap_or_default().trim();
        let matching = library.matching(query);
        let mut out = if query.is_empty() {
            format!(
                "{total} saved operation{} for `{name}`, from {}.",
                if total == 1 { "" } else { "s" },
                source.display()
            )
        } else if matching.is_empty() {
            let names = library.operations.iter().map(|saved| saved.name.as_str());
            return Ok(format!(
                "No saved operation for `{name}` matches \"{query}\". Its saved operations: {}.\n",
                backticked(names)
            ));
        } else {
            format!(
                "{} of {total} saved operations for `{name}` match \"{query}\".",
                matching.len()
            )
        };
        out.push_str(" Run one with `execute` and `operation_name`, passing the variables it declares.\n");
        for saved in matching {
            out.push('\n');
            listing(&mut out, &schema, name, endpoint, saved);
        }
        Ok(capped(
            out,
            endpoint.max_output_chars,
            "Pass `query` to list fewer operations.",
        ))
    }

    #[tool(
        title = "Describe a GraphQL type or field",
        description = "Show the exact definition of one type or one field as GraphQL SDL, with its descriptions, \
                       default values and deprecations. Read it before you write a selection: GraphQL has no \
                       wildcard, and one wrong field or argument makes the whole operation invalid. `name` is a type \
                       (`User`) or a field (`Query.user`, `Mutation.createOrder`). For a field, the result also \
                       defines the types of its arguments and its return type, which shows the required arguments \
                       (marked `!`), what an input object needs, and what you can select next. An interface or union \
                       lists its possible types: select their own fields with `... on Type`. Avoid anything marked \
                       deprecated; its reason names the replacement.",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    async fn describe(&self, Parameters(arguments): Parameters<DescribeArguments>) -> ToolResult {
        let endpoint = self.endpoint(&arguments.endpoint)?;
        let schema = self.schema(&arguments.endpoint, false).await?;
        let definition = schema.describe(&arguments.name)?;
        Ok(capped(
            definition,
            endpoint.max_output_chars,
            "Describe one field at a time, as `Type.field`, or use `search_schema`.",
        ))
    }

    #[tool(
        title = "Search a GraphQL schema",
        description = "Find queries, mutations, types, fields, arguments and enum values by name or description, \
                       when you know what you want but not what this schema calls it. Every word in `query` must \
                       match, ignoring case. Names rank above descriptions, and queries and mutations come first. \
                       Returns the best 20 matches as one-line signatures, each starting with a name that \
                       `describe` accepts, and says how many more there were. Search for words from the domain \
                       (`invoice`, `refund`), not GraphQL words such as `query` or `list`. To look inside one type, \
                       add its name as a second word: `user orders`, not `User.orders`. When nothing matches, the \
                       nearest names are suggested. Operations the user saved whose name or description matches \
                       every word are listed first: prefer one of those when it does what you need.",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    async fn search_schema(&self, Parameters(arguments): Parameters<SearchArguments>) -> ToolResult {
        let name = &arguments.endpoint;
        let endpoint = self.endpoint(name)?;
        let schema = self.schema(name, false).await?;
        let found = schema.search(name, &arguments.query)?;
        let saved = endpoint.operations.matching(&arguments.query);
        if saved.is_empty() {
            return Ok(found);
        }
        let mut out = "Saved operations that match, which `execute` runs by `operation_name` (`list_operations` \
                       shows them in full):\n"
            .to_owned();
        for saved in saved {
            let _ = write!(out, "{} ({})", saved.name, saved.kind.keyword());
            if saved.plan(&schema, name, endpoint).is_err() {
                out.push_str(" BROKEN: no longer valid");
            }
            if let Some(description) = &saved.description {
                let _ = write!(out, "  \"{description}\"");
            }
            out.push('\n');
        }
        out.push('\n');
        Ok(out + &found)
    }

    #[tool(
        title = "Validate and cost a GraphQL operation",
        description = "Validate one query or mutation against the schema and estimate its cost, without sending \
                       anything to the endpoint. Pass the GraphQL text as `operation`, or the name of a saved \
                       operation as `operation_name`. Run it after each change while you write an operation, and before \
                       `execute`. An invalid operation returns every error with its line and column, and the nearest \
                       field names for a misspelled one. A valid operation returns the estimated cost, the depth, the \
                       cost of each top-level field, the cost-map rules used, and every assumption made. Lists drive \
                       the cost: a list multiplies everything selected under it by its size. An assumption that a \
                       list had no size usually means you should pass `first`, `last` or `limit`, or select less \
                       under it. The estimate is fieldtrip's heuristic, not the server's own rate limit.",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    async fn estimate_cost(&self, Parameters(arguments): Parameters<EstimateArguments>) -> ToolResult {
        let name = &arguments.endpoint;
        let endpoint = self.endpoint(name)?;
        let (text, saved) = chosen(
            name,
            endpoint,
            arguments.operation.as_deref(),
            arguments.operation_name.as_deref(),
        )?;
        let schema = self.schema(name, false).await?;
        let plan = planned(&schema, name, endpoint, text, saved, &arguments.variables)?;
        let mut out = format!("Valid {} for `{name}`.\n{}", plan.title(), plan.estimate());
        if plan.kind == Kind::Mutation && endpoint.mutations == Mutations::Deny {
            out.push_str(
                "`execute` will refuse it: mutations are turned off for this endpoint by `mutations = \"deny\"` in \
                 the user's config. The user can run it themselves.\n",
            );
        }
        Ok(out)
    }

    #[tool(
        title = "Execute a GraphQL operation (the user approves)",
        description = "Send one query or mutation to the endpoint and return the response. Pass the GraphQL text \
                       as `operation`, or the name of an operation the user saved as `operation_name`. Make it valid \
                       with `estimate_cost` first, and use `describe` rather than this tool to explore. The user \
                       approves each request in a prompt that shows the operation exactly as it will be sent, its \
                       variables and its cost, unless their config says otherwise (`list_endpoints` shows who \
                       approves). Write for that reader: name the operation, pass inputs as variables, and select \
                       only the fields you need. The result starts with what was sent and its estimated cost, then \
                       a line with the HTTP status, how long the request took, and any response headers the user \
                       chose to show, such as a request or trace ID to look up in their logs. Then come `errors`, \
                       in full, then `data`, then any `extensions`, each as the server wrote it. When those after \
                       `errors` are too long for one result, they come back as a summary that is still valid JSON, \
                       with long lists cut to their first items, each cut marked with what it dropped. To keep a \
                       large response out of your context, pass `select` to pick the part you need, or `save` to \
                       write all of it to a file and get its path and a preview. A connection with \
                       `hasNextPage: true` is pointed out with its cursor: page through it with `after` rather than \
                       raising `first`. A response \
                       can hold both `data` and `errors`: read both, because one field can fail while the rest \
                       succeed. Each error's `path` names the field that failed, and its `extensions` often carry a \
                       code. For a mutation, select the result fields that confirm what changed, and any error \
                       fields its payload type has. If the user declines, nothing was sent: do not send it again, \
                       ask what they want instead.",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = true
        )
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
        let (text, saved) = chosen(
            name,
            endpoint,
            arguments.operation.as_deref(),
            arguments.operation_name.as_deref(),
        )?;
        // Before the schema is loaded, so a denied mutation causes no request of any kind.
        approval::refuse_denied_mutation(name, endpoint, text)?;
        let select = arguments.select.as_deref().map(select::Path::parse).transpose()?;
        let schema = self.schema(name, false).await?;
        let plan = planned(&schema, name, endpoint, text, saved, &arguments.variables)?;
        plan.check_variables(&schema, &arguments.variables)?;

        // Nothing below this point runs unless the human accepted, or the user's config waives the prompt.
        let unasked = if let Some(reason) = approval::waiver(endpoint, &plan) {
            format!(" The user was not asked: {reason}.")
        } else {
            let prompt = approval::prompt(name, &endpoint.shown_url, &plan, &arguments.variables);
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

        let reply = self
            .http
            .post(
                endpoint,
                &plan.payload(&arguments.variables),
                endpoint.max_response_bytes,
            )
            .await?;
        let summary = format!(
            "Sent {} to `{name}`. Estimated cost: {}.{unasked}\n{}",
            plan.title(),
            plan.cost,
            reply.metadata()
        );
        if reply.cut_short {
            // Too long to read in full, so it cannot be parsed. The status says whether it is a result.
            let excerpt: String = reply.body.chars().take(EXCERPT).collect();
            let unsaved = if arguments.save {
                " Nothing was saved, because only part of the body was read."
            } else {
                ""
            };
            let text = format!(
                "{summary}\n[The response is larger than {}, the `max_response_bytes` limit for this endpoint, so \
                 fieldtrip read only that much. A cut-off body is not valid JSON, so `data` and `errors` are both \
                 lost. The operation did reach the server, so a mutation has run. Ask for less: fewer fields, or a \
                 smaller page with `first` or `limit`, and page through the rest. The user can raise \
                 `max_response_bytes`.{unsaved}]\n\
                 It began: {excerpt}",
                show_bytes(endpoint.max_response_bytes)
            );
            return if reply.status < 400 {
                Ok(finished(text))
            } else {
                Err(text)
            };
        }
        let (summary, room) = if arguments.save {
            match saved::save(name, &reply.body) {
                Ok(path) => (
                    format!(
                        "{summary}\nSaved the whole response, {} bytes, to {}",
                        reply.body.len(),
                        path.display()
                    ),
                    response::Room::Preview(endpoint.max_output_chars.min(SAVED_PREVIEW)),
                ),
                Err(error) => (
                    format!("{summary}\nThe response was not saved: {error}."),
                    response::Room::Result(endpoint.max_output_chars),
                ),
            }
        } else {
            (summary, response::Room::Result(endpoint.max_output_chars))
        };
        let Ok(mut parts) = serde_json::from_str::<BTreeMap<String, Box<RawValue>>>(&reply.body) else {
            let excerpt: String = reply.body.chars().take(EXCERPT).collect();
            return Err(format!("{summary}\nThe response was not a GraphQL response: {excerpt}"));
        };
        if !parts.contains_key("errors") && !parts.contains_key("data") {
            return Err(capped(
                format!(
                    "{summary}\nThe response has neither `data` nor `errors`, so it is not a GraphQL response:\n{}",
                    reply.body.trim()
                ),
                endpoint.max_output_chars,
                ASK_FOR_LESS,
            ));
        }

        // Errors come first and whole, because they say what went wrong. What follows them is cut to fit.
        let mut head = summary;
        let data = parts.get("data").and_then(|data| serde_json::from_str(data.get()).ok());
        for hint in data.as_ref().map_or_else(Vec::new, response::page_hints) {
            let _ = write!(head, "\n{hint}");
        }
        let errors = parts.remove("errors");
        let rest = if let Some(path) = &select {
            let picked = serde_json::from_str(&reply.body)
                .map_or_else(|_error| select::Selected::nothing(), |whole| path.select(&whole));
            let _ = write!(head, "\n{}", picked.line);
            if let Some(errors) = errors.filter(|_| !path.is_in_errors()) {
                let _ = write!(head, "\nerrors: {}", errors.get().trim());
            }
            picked
                .value
                .and_then(|value| serde_json::value::to_raw_value(&value).ok())
                .map(|value| ("selected".to_owned(), value))
                .into_iter()
                .collect()
        } else {
            if let Some(errors) = errors {
                let _ = write!(head, "\nerrors: {}", errors.get().trim());
            }
            let data = parts.remove("data").map(|data| ("data".to_owned(), data));
            data.into_iter().chain(parts).collect::<Vec<_>>()
        };
        Ok(finished(shown(head, &rest, room)))
    }

    #[tool(
        title = "Load a GraphQL schema overview",
        description = "Load an endpoint's schema and summarize it: its description, how many types of each kind it \
                       has, and the name of every query and mutation field, which are the API's entry points. Call \
                       it once per endpoint to see what the API offers, then use `search_schema` and `describe` to \
                       read the fields you need. The schema stays loaded for every tool, so there is no need to send \
                       an introspection query with `execute`. It also reports cost-map rules that match nothing in \
                       the schema. Pass `refresh` only when the API has changed since it was loaded.",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    async fn introspect(&self, Parameters(arguments): Parameters<IntrospectArguments>) -> ToolResult {
        let name = &arguments.endpoint;
        let schema = self.schema(name, arguments.refresh).await?;
        let endpoint = self.endpoint(name)?;
        let source = endpoint.schema.as_ref().map_or_else(
            || format!("loaded by introspection from {}", endpoint.shown_url),
            |path| format!("read from the file {}", path.display()),
        );
        Ok(format!(
            "Schema for `{name}`, {source}.\n{}{}",
            schema.overview(endpoint.mutations),
            schema.cost_map_report(&endpoint.costs)
        ))
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

    /// Loads the schema for `endpoint`, and warns about each saved operation that no longer validates against it.
    async fn load(&self, name: &str, endpoint: &Endpoint) -> Result<Loaded, String> {
        let loaded = self.read_schema(name, endpoint).await?;
        for saved in &endpoint.operations.operations {
            if let Err(error) = saved.plan(&loaded, name, endpoint) {
                tracing::warn!(
                    "the saved operation `{}` for endpoint `{name}` ({}) no longer validates against the schema: {}",
                    saved.name,
                    saved.file.display(),
                    library::problems(&error).join("; ")
                );
            }
        }
        Ok(loaded)
    }

    /// Checks the saved operations of every endpoint whose schema is a file, so that one broken by a change to the
    /// schema is reported at startup. Endpoints that load their schema by introspection are checked on first use,
    /// because startup sends nothing.
    pub(crate) async fn check_saved_operations(&self) {
        for (name, endpoint) in &self.config.endpoints {
            if endpoint.schema.is_none() || endpoint.operations.is_empty() {
                continue;
            }
            if let Err(error) = self.schema(name, false).await {
                tracing::warn!("the saved operations for endpoint `{name}` could not be checked: {error}");
            }
        }
    }

    /// Reads the schema for `endpoint` from its file, or by introspection when it has none.
    async fn read_schema(&self, name: &str, endpoint: &Endpoint) -> Result<Loaded, String> {
        let Some(path) = &endpoint.schema else {
            let sdl = introspection::fetch(&self.http, endpoint).await?;
            return Loaded::from_sdl(&sdl, &format!("The introspection result from {}", endpoint.shown_url));
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
