//! The MCP server. Each tool is a thin wrapper over the rest of the crate.

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::sync::Arc;

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::tool::{InputResponses, RequestState};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResponse, CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerConfig};
use rmcp::service::{NotificationContext, RequestContext};
use rmcp::{RoleServer, ServerHandler, schemars, tool, tool_handler, tool_router};
use serde::Deserialize;
use serde_json::value::RawValue;
use tokio::sync::Mutex;

use crate::approval::{self, Approvals, Gate, Request, Returned};
use crate::audit::{self, Attempt, Audit, Outcome, Series, Verdict};
use crate::config::{Approval, Config, Endpoint, Mutations, show_duration};
use crate::http::Client;
use crate::library::Saved;
use crate::listing::{self, Filters};
use crate::plan::{Kind, Plan, Variables};
use crate::results::Results;
use crate::schema::{Loaded, introspection};
use crate::suggest::backticked;
use crate::text::{capped, excerpt};
use crate::{cache, response, results, select, shape};

/// What the server tells a client about itself and how to use it.
const INSTRUCTIONS: &str = "fieldtrip lets you use GraphQL APIs the user has configured, reading the real schema \
     instead of guessing it. Work in this order:\n\
     1. `list_endpoints` gives the endpoint names. Every other tool takes one.\n\
     2. `list_operations` shows the operations the user has saved and trusts for an endpoint. When one does what you \
     need, run it by its name instead of writing your own, and skip to step 6.\n\
     3. `introspect` once per endpoint shows its query and mutation fields, the API's entry points.\n\
     4. `search_schema` finds a type, field or saved operation by a word. `describe` shows a field's exact \
     arguments and return type, and for a type, what uses it and the shortest paths to it from `Query`.\n\
     5. Write one operation and check it with `estimate_cost` until it is valid and cheap. Nothing is sent.\n\
     6. `execute` sends it, or a saved operation named by `operation_name`. The user approves each request unless \
     their config says otherwise.\n\
     7. `execute_batch` sends one query once for each of many sets of variables, with a single approval. Use it \
     instead of calling `execute` in a loop.\n\
     8. A response too long to show is written to a file, and `execute` returns its path, a preview and its shape. \
     `read_result` then returns any part of it by a path, without sending anything: use it rather than running the \
     operation again.\n\
     GraphQL habits that matter here: take every field and argument name from the schema, never from memory. \
     Select only the fields you need, since there is no wildcard. Name the operation and pass inputs as \
     variables, not as values pasted into the text: the user reads both when approving. Give lists a page size, \
     such as `first` or `limit`, because each list multiplies the cost of everything under it. Avoid fields \
     marked deprecated, whose reason names the replacement.";

/// Added to [`INSTRUCTIONS`] when the user keeps an audit log, which `history` reads.
const HISTORY_INSTRUCTIONS: &str = "\nThe user keeps an audit log of `execute` attempts. At the start of a task, or \
     after an interruption, `history` shows what was already sent or refused, in this session and earlier ones. Do \
     not send again what the user declined unless they ask for it.";

/// What a tool hands back: text for the calling agent, or an error message that says what to do next.
type ToolResult = Result<String, String>;

/// The fieldtrip MCP server.
#[derive(Debug, Clone)]
pub(crate) struct Fieldtrip {
    config: Arc<Config>,
    /// Schemas loaded so far, by endpoint name. The lock is held while loading, so each schema loads once.
    schemas: Arc<Mutex<HashMap<String, Arc<Held>>>>,
    http: Client,
    /// What is remembered about approvals: prompts waiting for an answer, and queries approved for the session.
    approvals: Arc<Approvals>,
    /// Where each `execute` attempt is recorded.
    audit: Arc<Audit>,
    /// Where responses too long to show are written.
    results: Arc<Results>,
    /// The tools this server offers. `history` is left out when there is no audit log to read.
    tools: Arc<ToolRouter<Self>>,
}

/// A loaded schema and where it came from.
#[derive(Debug)]
struct Held {
    /// The schema.
    schema: Loaded,
    /// Where it came from.
    origin: Origin,
}

impl std::ops::Deref for Held {
    type Target = Loaded;

    fn deref(&self) -> &Loaded {
        &self.schema
    }
}

/// Where a schema came from.
#[derive(Debug)]
enum Origin {
    /// The endpoint's `schema` file.
    File,
    /// An introspection query, just now.
    Introspected,
    /// fieldtrip's cache, where it was written this long ago, within the endpoint's `schema_ttl`.
    Kept(std::time::Duration),
    /// fieldtrip's cache, written this long ago, because introspecting failed with the error given.
    Fallback(std::time::Duration, String),
}

impl Held {
    /// A note to add to every result that reads a schema that may be out of date. Empty for any other schema.
    fn staleness(&self) -> String {
        match &self.origin {
            Origin::Fallback(age, _) => format!(
                "\n[This schema is fieldtrip's cached copy from {} ago, because the endpoint could not be \
                 introspected. It may be out of date. `introspect` with `refresh: true` tries again.]",
                show_duration(*age)
            ),
            _ => String::new(),
        }
    }
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
                       match. Case is ignored. Use the user's own word for a thing when they gave one: their glossary may \
                       know it."
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
                       operations whose name or description holds every word are listed. A word from the user's \
                       glossary also matches operations that use what it stands for. Case is ignored. Leave it \
                       out to list them all."
    )]
    query: Option<String>,
    /// Patterns the operations must use.
    #[schemars(
        description = "Optional. Patterns that every listed operation must match, all of them. A type name, such as \
                       `Order`, matches an operation that selects a field of that type or a field that returns it. A \
                       type followed by field names, as in a cost map, matches an operation that selects that chain: \
                       `User.orders` is `orders` selected on a `User`, `Query.order.lineItems` is that exact path \
                       from the root, and `*` stands for any one name, as in `*.payment`. Fragments are followed."
    )]
    #[serde(default)]
    uses: Vec<String>,
    /// Query or mutation.
    #[schemars(description = "Optional. `query` or `mutation`, to list only operations of that kind.")]
    kind: Option<String>,
    /// One operation to show in full.
    #[schemars(
        description = "Optional. The exact name of one saved operation, to show it in full with its text. Give it \
                       alone, without the other filters."
    )]
    name: Option<String>,
}

/// How many saved operations `search_schema` lists before the schema's own matches.
const SEARCH_OPERATIONS: usize = 10;

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

/// Arguments of `execute_batch`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct BatchArguments {
    /// The endpoint name, as listed by `list_endpoints`.
    endpoint: String,
    /// The GraphQL document.
    #[schemars(description = OPERATION)]
    operation: Option<String>,
    /// The name of a saved operation.
    #[schemars(description = OPERATION_NAME)]
    operation_name: Option<String>,
    /// One set of variables for each run.
    #[schemars(description = "A JSON list with one object of variables for each run, such as \
                       `[{\"id\": \"1\"}, {\"id\": \"2\"}]`. The query is sent once for each, in this order, and the \
                       result numbers the runs by their position in this list, from 0. Every set is checked against \
                       the declared types before the user is asked or anything is sent.")]
    variables: Vec<Variables>,
    /// A path that picks part of each response.
    #[schemars(
        description = "Optional. A path that picks part of each response once it arrives, as `select` does for \
                       `execute`, such as `data.user.name`. With many runs it keeps the result short. `errors` is \
                       still shown in full for each run."
    )]
    select: Option<String>,
}

/// One run of a batch: the variables it is sent with and what it is estimated to cost.
struct Run {
    variables: Variables,
    cost: u64,
}

/// The fewest characters a run of a batch gets of the result, however many runs there are.
const MIN_SHARE: usize = 300;

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
        description = "Optional. Set to `true` to write the whole response to a new file that fieldtrip names, \
                       whatever its size, and get back its path and size with the response or a short preview of it. \
                       A long response is written to a file without this. Use it when you want a small one on disk \
                       too. It changes neither what is sent nor what the user approves."
    )]
    #[serde(default)]
    save: bool,
}

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
        None => error.into(),
    })
}

/// The variables to send with an operation, and a note about any that were left out. Only a `saved` operation leaves
/// any out: the ones it lost along with its client directives.
fn sendable(saved: Option<&Saved>, variables: &Variables) -> (Variables, Option<String>) {
    saved.map_or_else(|| (variables.clone(), None), |saved| saved.sendable(variables))
}

/// How many attempts `history` returns when it is not told.
const DEFAULT_HISTORY: usize = 20;

/// The most attempts `history` returns.
const MAX_HISTORY: usize = 100;

/// Arguments of `history`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct HistoryArguments {
    /// The endpoint to show the attempts on.
    #[schemars(
        description = "Optional. An endpoint name, as listed by `list_endpoints`, to see only the attempts on it. \
                       Leave it out to see the attempts on every configured endpoint."
    )]
    endpoint: Option<String>,
    /// How many attempts to show.
    #[schemars(
        description = "Optional. How many of the most recent attempts to return, from 1 to 100. Defaults to 20."
    )]
    limit: Option<usize>,
}

/// Arguments of `read_result`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ReadResultArguments {
    /// The result to read.
    #[schemars(
        description = "The ID of a response that `execute` wrote to a file, such as `acme-prod-1b4e28ba9f3c`, or \
                       the file's path. Both are in `execute`'s result, where it says the response was saved. No \
                       other file can be read."
    )]
    result: String,
    /// A path that picks part of the response.
    #[schemars(description = "A path that picks part of the saved response, such as \
                       `data.repository.issues.nodes[*].number`, written as for `execute`: it starts at `data`, \
                       `errors` or `extensions`, names are joined by `.`, `[0]` takes one item of a list and `[*]` \
                       every item. Take the names from the shape `execute` showed. Leave it out to see the preview \
                       and the shape again.")]
    select: Option<String>,
}

/// Arguments of `introspect`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct IntrospectArguments {
    /// The endpoint name, as listed by `list_endpoints`.
    endpoint: String,
    /// Whether to load the schema again.
    #[schemars(
        description = "Set to `true` to load the schema again from its source, after the API has changed. \
                       Otherwise the copy already loaded is used, or the one fieldtrip keeps on disk while it is \
                       newer than the endpoint's `schema_ttl`."
    )]
    #[serde(default)]
    refresh: bool,
}

#[tool_router]
impl Fieldtrip {
    /// Creates a server for `config` that reaches endpoints through `http` and records attempts in `audit`.
    pub(crate) fn new(config: Config, http: Client, audit: Audit) -> Self {
        let mut tools = Self::tool_router();
        if audit.path().is_none() {
            tools.remove_route("history");
        }
        let results = Results::new(audit.session(), config.result_ttl);
        // Earlier sessions leave their files behind, so the ones that are too old are cleared out now.
        results.sweep();
        Self {
            config: Arc::new(config),
            schemas: Arc::default(),
            http,
            approvals: Arc::default(),
            results: Arc::new(results),
            audit: Arc::new(audit),
            tools: Arc::new(tools),
        }
    }

    #[tool(
        title = "List GraphQL endpoints",
        description = "List the GraphQL endpoints the user has configured. Call this first: every other tool takes \
                       one of these endpoint names, and fieldtrip talks to no other host. For each endpoint it \
                       returns the user's description of it when there is one, the URL, where the schema comes \
                       from (a file or introspection), who approves `execute` requests, `mutations: denied` when \
                       `execute` refuses every mutation there, the cost map and the glossary of the user's own words if there are any, how many operations \
                       the user has saved for `list_operations`, and the names of the headers sent with each request, so you can tell whether \
                       authentication is set up. For a header produced by a command, such as a login helper, it says \
                       how the command's last run went: ok, failed with its exit status, or not run yet. It runs \
                       nothing to find out. A failed command usually means the user's login expired: tell them to \
                       sign in again rather than retrying. Header values are never shown. When several endpoints are \
                       instances of one service, such as dev and prod, prefer the one the description or the user \
                       points to, and ask when it is unclear. Endpoints and headers come only from the user's \
                       config: if one is missing, ask the user to add it.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    #[expect(
        clippy::needless_pass_by_value,
        reason = "rmcp's `tool` macro hands the request context over by value"
    )]
    fn list_endpoints(&self, context: RequestContext<RoleServer>) -> String {
        let unseen = approval::auto_accepting(context.client_info().as_ref());
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
                None if endpoint.schema_ttl.is_zero() => {
                    let _ = writeln!(out, "  schema: introspection, not cached");
                }
                None => {
                    let _ = writeln!(
                        out,
                        "  schema: introspection, cached for {}",
                        show_duration(endpoint.schema_ttl)
                    );
                }
            }
            let _ = writeln!(out, "  approval: {}", approval::policy_for(endpoint, unseen));
            if endpoint.mutations == Mutations::Deny {
                let _ = writeln!(out, "  mutations: denied");
            }
            if let Some(source) = &endpoint.costs.source {
                let rules = endpoint.costs.len();
                let plural = if rules == 1 { "" } else { "s" };
                let _ = writeln!(out, "  cost map: {} ({rules} rule{plural})", source.display());
            }
            if let Some(source) = &endpoint.glossary.source {
                let terms = endpoint.glossary.len();
                let plural = if terms == 1 { "" } else { "s" };
                let _ = writeln!(out, "  glossary: {} ({terms} term{plural})", source.display());
            }
            if let Some(source) = &endpoint.operations.source {
                let count = endpoint.operations.operations.len();
                let left_out = match endpoint.operations.operation_files_left_out() {
                    0 => String::new(),
                    1 => ", 1 file left out".to_owned(),
                    many => format!(", {many} files left out"),
                };
                let _ = writeln!(
                    out,
                    "  saved operations: {count} in {}{left_out} (see `list_operations`)",
                    source.display()
                );
            }
            if !endpoint.headers.is_empty() {
                let names: Vec<&str> = endpoint.headers.iter().map(|header| header.name.as_str()).collect();
                let _ = writeln!(out, "  headers: {}", names.join(", "));
            }
            for header in &endpoint.headers {
                if let Some(status) = header.status() {
                    let _ = writeln!(out, "  header {}: {status}", header.name);
                }
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
                       marked BROKEN with what is wrong, and `execute` will not run it. Files that could not be \
                       loaded at all, such as two with one name, are listed at the end as left out. A library can \
                       hold hundreds, \
                       so narrow it: `query` matches words in the name or description, and a word from the user's \
                       glossary also matches the operations that use what it stands for, listed first and marked \
                       `glossary: term → name`. `uses` matches operations \
                       that select a type or a chain of fields such as `Order.payment`, and `kind` keeps queries or \
                       mutations. When more than five match, each gets one line without its text, and `name` shows \
                       one in full.",
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
        if let Some(wanted) = &arguments.name {
            if arguments.query.is_some() || !arguments.uses.is_empty() || arguments.kind.is_some() {
                return Err(
                    "Give `name` alone: it shows one operation, and `query`, `uses` and `kind` narrow a list."
                        .to_owned(),
                );
            }
            let saved = library.get(name, wanted)?;
            let mut out = format!(
                "Saved operation `{wanted}` for `{name}`, from {}. Run it with `execute` and `operation_name`, \
                 passing the variables it declares.\n\n",
                saved.file.display()
            );
            listing::one(&mut out, &schema, name, endpoint, saved, &[]);
            return Ok(capped(
                out,
                endpoint.max_output_chars,
                "The operation is too long to show whole.",
            ));
        }
        let filters = Filters::new(
            arguments.query.as_deref(),
            &arguments.uses,
            arguments.kind.as_deref(),
            &schema,
        )?;
        Ok(capped(
            listing::list(&schema, name, endpoint, &filters),
            endpoint.max_output_chars,
            "Narrow the list with `query`, `uses` or `kind`.",
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
                       lists its possible types: select their own fields with `... on Type`. A type ends with what \
                       uses it: the unions it belongs to, then every field that returns it, argument that takes it \
                       (written `Type.field(argument:)`) and input field that holds it, so you can work back from a \
                       type you found to where it appears. Then come up to five shortest paths from `Query`, one per \
                       root field that leads there, such as `Query.search{... on Product}.reviews`: each is a \
                       selection to write, with `{... on Type}` for an inline fragment. Avoid anything marked \
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
        ) + &schema.staleness())
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
                       every word are listed first: prefer one of those when it does what you need. \
                       When the user keeps a glossary of their own words for this API, such as `PR` for \
                       `PullRequest`, a word from it is looked up there before anything else, and each line reads \
                       `term → name`. So search for the user's word as they said it before trying synonyms.",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    async fn search_schema(&self, Parameters(arguments): Parameters<SearchArguments>) -> ToolResult {
        let name = &arguments.endpoint;
        let endpoint = self.endpoint(name)?;
        let schema = self.schema(name, false).await?;
        let found = schema.search(name, &arguments.query, &endpoint.glossary)?;
        let saved = endpoint
            .operations
            .matching_with(&arguments.query, &endpoint.glossary, &schema);
        let mut operations = String::new();
        if !saved.is_empty() {
            operations.push_str(
                "Saved operations that match, which `execute` runs by `operation_name` (`list_operations` shows them \
                 in full):\n",
            );
        }
        for (saved, through) in saved.iter().take(SEARCH_OPERATIONS) {
            let _ = writeln!(
                operations,
                "{}",
                listing::summary(&schema, name, endpoint, saved, through)
            );
        }
        if saved.len() > SEARCH_OPERATIONS {
            let _ = writeln!(
                operations,
                "And {} more. `list_operations` with the same `query` lists them all.",
                saved.len() - SEARCH_OPERATIONS
            );
        }
        // The user's own words first, then what they saved, then the schema's matches, with a blank line between.
        let parts = [found.glossary, operations, found.schema];
        let parts: Vec<&str> = parts
            .iter()
            .map(String::as_str)
            .filter(|part| !part.is_empty())
            .collect();
        Ok(parts.join("\n") + &schema.staleness())
    }

    #[tool(
        title = "Validate and cost a GraphQL operation",
        description = "Validate one query or mutation against the schema and estimate its cost, without sending \
                       anything to the endpoint. Pass the GraphQL text as `operation`, or the name of a saved \
                       operation as `operation_name`. Run it after each change while you write an operation, and before \
                       `execute`. An invalid operation returns every error with its line and column, the nearest \
                       field names for a misspelled field, and the valid names for an unknown input field, \
                       argument or enum value. A valid operation returns the estimated cost, the depth, the \
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
        let (variables, left_out) = sendable(saved, &arguments.variables);
        let plan = planned(&schema, name, endpoint, text, saved, &variables)?;
        let mut out = format!("Valid {} for `{name}`.\n{}", plan.title(), plan.estimate());
        if let Some(note) = left_out {
            let _ = writeln!(out, "{note}");
        }
        if plan.kind == Kind::Mutation && endpoint.mutations == Mutations::Deny {
            out.push_str(
                "`execute` will refuse it: mutations are turned off for this endpoint by `mutations = \"deny\"` in \
                 the user's config. The user can run it themselves.\n",
            );
        }
        out.push_str(&schema.staleness());
        Ok(out)
    }

    #[tool(
        title = "Execute a GraphQL operation (the user approves)",
        description = "Send one query or mutation to the endpoint and return the response. Pass the GraphQL text \
                       as `operation`, or the name of an operation the user saved as `operation_name`. Make it valid \
                       with `estimate_cost` first, and use `describe` rather than this tool to explore. The user \
                       approves each request in a prompt that shows the operation exactly as it will be sent, its \
                       variables and its cost, unless their config says otherwise (`list_endpoints` shows who \
                       approves). In that prompt the user can approve a query for the rest of the session: it then \
                       runs without a prompt whatever its variables, and the result says so. So when you run one \
                       query many times, keep its text the same and change only the variables. Mutations are asked \
                       about every time. Write for that reader: name the operation, pass inputs as variables, and select \
                       only the fields you need. The result starts with what was sent and its estimated cost, then \
                       a line with the HTTP status, how long the request took, any ID fieldtrip generated for the \
                       request (`sent X-Correlation-Id ...`), and any response headers the user chose to show, such \
                       as a request or trace ID. Give those IDs to the user when something needs looking up in the \
                       server's logs; a generated one is shown even when no answer came. Then come `errors`, \
                       in full, then `data`, then any `extensions`, each as the server wrote it. A response too \
                       long to show is written whole to a file instead: the result gives the file's path, `errors` \
                       in full, a preview that is still valid JSON, with long lists and strings cut and each cut \
                       marked with what it dropped, and the shape of the response, one line per path with its type \
                       and size. Nothing was lost, so do not send the operation again to see more: `read_result` returns \
                       any part of the file by a path. To get one part in this result, pass `select`. Pass `save` to have a short response written to a file too. A \
                       connection with \
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
        self.refuse_denied(name, endpoint, text, &arguments.variables).await?;
        let select = arguments.select.as_deref().map(select::Path::parse).transpose()?;
        let schema = self.schema(name, false).await?;
        // From here on the variables are the ones that will be sent, which is what the prompt and the log show.
        let (variables, left_out) = sendable(saved, &arguments.variables);
        let plan = planned(&schema, name, endpoint, text, saved, &variables)?;
        plan.check_variables(&schema, &variables)?;
        let attempt = Attempt {
            endpoint: name,
            kind: plan.kind,
            operation: plan.name.as_deref(),
            text: &plan.text,
            variables: &variables,
            cost: Some(plan.cost),
            series: Series::Single,
        };

        // Nothing below this point runs unless the human accepted, or the user's config waives the prompt.
        let covered = serde_json::Value::Object(variables.clone());
        let request = Request {
            tool: "execute",
            name,
            endpoint,
            kind: plan.kind,
            text: &plan.text,
            cost: plan.cost,
            prompt: approval::prompt(name, &endpoint.shown_url, &plan, &variables),
            covered: &covered,
            offer_session: plan.kind == Kind::Query,
        };
        let returned = Returned {
            state: state.as_deref(),
            responses: responses.as_ref(),
        };
        let (verdict, unasked) = match approval::gate(&self.approvals, &context, request, returned).await {
            Gate::Open { verdict, unasked } => (verdict, unasked),
            Gate::Ask(question) => return Ok(question.into()),
            Gate::Shut { verdict, refusal } => {
                self.audit.record(endpoint, &attempt, &verdict, Outcome::NotSent).await;
                return Err(refusal);
            }
        };

        let sent = self
            .http
            .post(endpoint, &plan.payload(&variables), endpoint.max_response_bytes)
            .await;
        // A response too long to show is written to a file whole, so nothing has to be sent again to see the rest.
        // It is written before the attempt is recorded, so the audit log can name the file.
        let inline = endpoint.inline_limit();
        let written = sent
            .as_ref()
            .ok()
            .filter(|reply| !reply.cut_short)
            .map(|reply| (reply, reply.body.chars().count()))
            .filter(|(_, length)| arguments.save || *length > inline)
            .map(|(reply, length)| (length, self.results.write(name, &reply.body)));
        let result = written
            .as_ref()
            .and_then(|(_, stored)| stored.as_ref().ok())
            .map(|stored| stored.id.as_str());
        let outcome = sent
            .as_ref()
            .map_or_else(Outcome::Failed, |reply| Outcome::Answered(reply, result));
        self.audit.record(endpoint, &attempt, &verdict, outcome).await;
        let reply = sent.map_err(String::from)?;
        let mut summary = format!(
            "Sent {} to `{name}`. Estimated cost: {}.{unasked}\n{}",
            plan.title(),
            plan.cost,
            reply.metadata()
        );
        if let Some(note) = &left_out {
            let _ = write!(summary, "\n{note}");
        }
        if let Some(note) = &reply.refused_twice {
            let _ = write!(summary, "\n{note}");
        }
        response::render(
            summary,
            &reply,
            endpoint,
            written,
            inline,
            select.as_ref(),
            arguments.save,
        )
        .map(finished)
    }

    #[tool(
        title = "Execute a GraphQL query many times (the user approves once)",
        description = "Send one query to the endpoint once for each set of variables in a list, and return every \
                       response. Use it instead of calling `execute` in a loop when you need the same lookup for \
                       many IDs or inputs: the user is asked once, in a prompt that shows the query, how many times \
                       it will run, the first few sets of variables and the total estimated cost, and the result \
                       says so when they were not asked. Pass the GraphQL text as `operation`, or the name of a \
                       saved operation as `operation_name`, and the sets as `variables`. Check the query with \
                       `estimate_cost` first. Queries only: a mutation is refused, because the user approves each \
                       mutation on its own with `execute`. The user's config caps the number of runs, 100 unless \
                       they changed it. The runs are sent one after another, in order. The result has one section \
                       for each run, starting with `[0]` for the first set, with the HTTP status and time, then \
                       that run's `errors` in full, then its `data`, cut to fit its share of the result: pass \
                       `select` to keep only the part you need. One run's `errors` do not stop the others. A run \
                       that gets no answer at all stops the batch, and the result says which runs were not sent. \
                       If the user declines, nothing was sent: do not send it again, ask what they want instead.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = true
        )
    )]
    async fn execute_batch(
        &self,
        context: RequestContext<RoleServer>,
        RequestState(state): RequestState,
        InputResponses(responses): InputResponses,
        Parameters(arguments): Parameters<BatchArguments>,
    ) -> Result<CallToolResponse, String> {
        let name = &arguments.endpoint;
        let endpoint = self.endpoint(name)?;
        let (text, saved) = chosen(
            name,
            endpoint,
            arguments.operation.as_deref(),
            arguments.operation_name.as_deref(),
        )?;
        let no_variables = Variables::new();
        let first = arguments.variables.first().unwrap_or(&no_variables);
        self.refuse_denied(name, endpoint, text, first).await?;
        let select = arguments.select.as_deref().map(select::Path::parse).transpose()?;
        let count = arguments.variables.len();
        if count == 0 {
            return Err(
                "`variables` is empty. Give a list with one object of variables for each run, such as \
                 `[{\"id\": \"1\"}, {\"id\": \"2\"}]`. Nothing was sent."
                    .to_owned(),
            );
        }
        if count > endpoint.max_batch {
            return Err(format!(
                "`variables` has {count} sets, and one call to `execute_batch` on `{name}` sends at most {} \
                 (`max_batch` in the user's config). Nothing was sent. Split the list into smaller batches.",
                endpoint.max_batch
            ));
        }
        let schema = self.schema(name, false).await?;

        // Every set is planned and checked before anyone is asked, so a batch never stops halfway on a typo.
        let mut runs = Vec::with_capacity(count);
        let mut notes: Vec<String> = Vec::new();
        let mut shown_plan = None;
        for (index, given) in arguments.variables.iter().enumerate() {
            let (variables, left_out) = sendable(saved, given);
            let plan = planned(&schema, name, endpoint, text, saved, &variables)?;
            if plan.kind == Kind::Mutation {
                return Err(format!(
                    "Nothing was sent to `{name}`. `execute_batch` runs queries only, and this is a mutation. The \
                     user approves each mutation on its own: send them one at a time with `execute`."
                ));
            }
            plan.check_variables(&schema, &variables)
                .map_err(|error| format!("`variables[{index}]`: {error} Nothing was sent."))?;
            notes.extend(left_out.filter(|note| !notes.contains(note)));
            runs.push(Run {
                variables,
                cost: plan.cost,
            });
            // The costliest run's plan is the one shown, since its assumptions are the ones that matter most.
            if shown_plan.as_ref().is_none_or(|shown: &Plan| plan.cost > shown.cost) {
                shown_plan = Some(plan);
            }
        }
        let Some(plan) = shown_plan else {
            return Err("`variables` is empty. Nothing was sent.".to_owned());
        };
        let total = runs.iter().fold(0_u64, |total, run| total.saturating_add(run.cost));
        let sets: Vec<Variables> = runs.iter().map(|run| run.variables.clone()).collect();

        // Nothing below this point runs unless the human accepted, or the user's config waives the prompt.
        let covered: serde_json::Value = sets.iter().cloned().map(serde_json::Value::Object).collect();
        let whole = Attempt {
            endpoint: name,
            kind: plan.kind,
            operation: plan.name.as_deref(),
            text: &plan.text,
            variables: &no_variables,
            cost: Some(total),
            series: Series::Batch(&sets),
        };
        let request = Request {
            tool: "execute_batch",
            name,
            endpoint,
            kind: plan.kind,
            text: &plan.text,
            cost: total,
            prompt: approval::batch_prompt(name, &endpoint.shown_url, &plan, &sets, total, plan.cost),
            covered: &covered,
            offer_session: false,
        };
        let returned = Returned {
            state: state.as_deref(),
            responses: responses.as_ref(),
        };
        let (verdict, unasked) = match approval::gate(&self.approvals, &context, request, returned).await {
            Gate::Open { verdict, unasked } => (verdict, unasked),
            Gate::Ask(question) => return Ok(question.into()),
            Gate::Shut { verdict, refusal } => {
                self.audit.record(endpoint, &whole, &verdict, Outcome::NotSent).await;
                return Err(refusal);
            }
        };

        let mut out = format!(
            "Sent {} to `{name}` {count} times, once for each set of variables. Estimated cost: {total} in \
             total.{unasked}\nEach run below starts with the position of its set in `variables`, from 0.",
            plan.title()
        );
        for note in &notes {
            let _ = write!(out, "\n{note}");
        }
        let limit = endpoint.max_output_chars;
        let share = (limit.saturating_sub(out.chars().count() + response::NOTE_ROOM) / count).max(MIN_SHARE);
        let mut answered = 0;
        for (index, run) in runs.iter().enumerate() {
            let attempt = Attempt {
                variables: &run.variables,
                cost: Some(run.cost),
                series: Series::Run {
                    run: index + 1,
                    of: count,
                },
                ..whole
            };
            let sent = self
                .http
                .post(endpoint, &plan.payload(&run.variables), endpoint.max_response_bytes)
                .await;
            let outcome = sent
                .as_ref()
                .map_or_else(Outcome::Failed, |reply| Outcome::Answered(reply, None));
            self.audit.record(endpoint, &attempt, &verdict, outcome).await;
            let reply = match sent {
                Ok(reply) => reply,
                Err(failed) => {
                    let _ = write!(out, "\n\n[{index}] No answer: {failed}");
                    let unsent = count - index - 1;
                    if unsent > 0 {
                        let _ = write!(
                            out,
                            "\n\n[The {unsent} run{} after `[{index}]` {} not sent: a batch stops at the first \
                             request that gets no answer, since the rest would likely fail the same way.]",
                            if unsent == 1 { "" } else { "s" },
                            if unsent == 1 { "was" } else { "were" }
                        );
                    }
                    break;
                }
            };
            answered += 1;
            let section = response::section(&format!("[{index}]"), &reply, endpoint, select.as_ref(), share);
            let _ = write!(out, "\n\n{section}");
        }
        let out = capped(
            out,
            limit.max(count.saturating_mul(MIN_SHARE)),
            "Pass `select`, or send fewer sets at once.",
        );
        if answered == 0 { Err(out) } else { Ok(finished(out)) }
    }

    #[tool(
        title = "Read part of a saved GraphQL response",
        description = "Return part of a response that `execute` wrote to a file because it was too long to show, or \
                       because you passed `save`. Use it instead of running the operation again when you want to \
                       look at another part: it reads the file and sends nothing to the endpoint, so it needs no \
                       approval and costs the API nothing. Pass the result ID or the path from `execute`'s result \
                       as `result`, and a `select` path such as `data.repository.issues.nodes[*].number`, using the \
                       names in the shape `execute` showed. It returns `errors` in full, then the selected part as \
                       JSON. A selection too long for one result is cut by structure, with each cut marked: narrow \
                       the path, for instance to one item with `[0]`, to get a part whole. Without `select` it \
                       returns the preview and the shape again. A path that matches nothing says what is there. The \
                       file holds the response as it was when it arrived: to see current data, run the operation \
                       again with `execute`.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    fn read_result(&self, Parameters(arguments): Parameters<ReadResultArguments>) -> ToolResult {
        let select = arguments
            .select
            .as_deref()
            .map(select::Path::parse)
            .transpose()
            .map_err(|error| error.replace(" Nothing was sent.", ""))?;
        let stored = self.results.find(&arguments.result)?;
        let body = std::fs::read_to_string(&stored.path).map_err(|error| {
            format!(
                "Could not read the result `{}` at {}: {error}",
                stored.id,
                stored.path.display()
            )
        })?;
        let summary = format!(
            "Result `{}`: {} bytes, in {}. Nothing was sent to the endpoint.",
            stored.id,
            shape::grouped(body.len()),
            stored.path.display()
        );
        let Ok(parts) = serde_json::from_str::<BTreeMap<String, Box<RawValue>>>(&body) else {
            let excerpt = excerpt(&body, response::EXCERPT);
            return Err(format!(
                "{summary}\nThe file does not hold a GraphQL response: it is not JSON, or not a JSON object, so \
                 `select` has nothing to pick from. It begins: {excerpt}"
            ));
        };
        let (output, inline) = self.limits_of(&stored.id);
        let room = if select.is_some() {
            response::Room::Read(output)
        } else {
            response::Room::Preview(inline.min(response::SAVED_PREVIEW))
        };
        Ok(response::laid_out(summary, "", &body, parts, select.as_ref(), room))
    }

    #[tool(
        title = "Show earlier GraphQL executions",
        description = "List the `execute` attempts recorded in the user's audit log, in this session and earlier \
                       ones, so you can see what was already sent or refused before you repeat it. Use it at the \
                       start of a task that may have been started before, after an interruption, or when the user \
                       asks what was run. Returns the most recent attempts, oldest first, one JSON object per line \
                       with the time, the `session`, the endpoint, the operation's name and text, its variables \
                       unless the user keeps them out, and `approval`: `accepted`, `auto`, `session` or `client` for one that \
                       was sent, with its HTTP `status`, time in `ms` and number of `errors`, or `declined`, \
                       `cancelled`, `unavailable`, `failed` or `denied` with a `reason` for one that was not. The \
                       first line names this session. A response is not in the log, but a line with a `result` \
                       is an attempt whose response was written to a file: the files that still exist are listed \
                       after the lines, and `read_result` reads them without sending anything. For any other \
                       attempt, run the operation again to see data. Do not send again what the user declined \
                       unless they ask for it.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    fn history(&self, Parameters(arguments): Parameters<HistoryArguments>) -> ToolResult {
        // With no endpoint named, the result is held to the smallest limit of any of them.
        let (budget, scope) = if let Some(name) = &arguments.endpoint {
            (self.endpoint(name)?.max_output_chars, format!(" on `{name}`"))
        } else {
            let smallest = self
                .config
                .endpoints
                .values()
                .map(|endpoint| endpoint.max_output_chars)
                .min();
            (smallest.unwrap_or(usize::MAX), String::new())
        };
        let wanted = arguments.endpoint.as_deref();
        let history = self.audit.history(|endpoint| {
            wanted.map_or_else(
                || self.config.endpoints.contains_key(endpoint),
                |wanted| wanted == endpoint,
            )
        })?;
        let session = self.audit.session();
        let mut notes = String::new();
        if history.partial {
            notes.push_str("\n[Only the end of the audit log was read, so older attempts are not counted.]");
        }
        if history.unreadable > 0 {
            let lines = if history.unreadable == 1 {
                "1 line".to_owned()
            } else {
                format!("{} lines", history.unreadable)
            };
            let _ = write!(
                notes,
                "\n[{lines} of the audit log could not be read and were skipped.]"
            );
        }
        let total = history.lines.len();
        if total == 0 {
            return Ok(format!(
                "The audit log has no `execute` attempts{scope} yet. This session is `{session}`.{notes}"
            ));
        }
        let limit = arguments.limit.unwrap_or(DEFAULT_HISTORY).clamp(1, MAX_HISTORY);
        let recent = history.lines.get(total.saturating_sub(limit)..).unwrap_or_default();

        // The newest attempts matter most, so when they do not all fit, the oldest are left out.
        let head_room = 400 + notes.chars().count();
        let mut room = budget.saturating_sub(head_room + response::NOTE_ROOM);
        let mut kept = 0;
        for line in recent.iter().rev() {
            let length = line.chars().count() + 1;
            if kept > 0 && length > room {
                break;
            }
            room = room.saturating_sub(length);
            kept += 1;
        }
        let shown = recent.get(recent.len() - kept..).unwrap_or_default();
        let mut out = format!(
            "The last {kept} of {total} `execute` attempts{scope} in the audit log, oldest first, one JSON object per \
             line. This session is `{session}`: a line with another `session` is from an earlier run of fieldtrip."
        );
        for line in shown {
            let _ = write!(out, "\n{line}");
        }
        out.push_str(&self.result_files(shown));
        let dropped = recent.len() - kept;
        if dropped > 0 {
            let attempts = if dropped == 1 { "attempt" } else { "attempts" };
            let _ = write!(
                out,
                "\n[{dropped} older {attempts} did not fit in this result. Pass a smaller `limit`, or an `endpoint`.]"
            );
        }
        out.push_str(&notes);
        Ok(capped(out, budget, "Pass a smaller `limit`, or an `endpoint`."))
    }

    #[tool(
        title = "Load a GraphQL schema overview",
        description = "Load an endpoint's schema and summarize it: its description, how many types of each kind it \
                       has, and the name of every query and mutation field, which are the API's entry points. Call \
                       it once per endpoint to see what the API offers, then use `search_schema` and `describe` to \
                       read the fields you need. The schema stays loaded for every tool, so there is no need to send \
                       an introspection query with `execute`. It also reports cost-map rules that match nothing in \
                       the schema, and targets of the user's glossary that the schema no longer has. An introspected schema is kept on disk, a day unless the user's config says \
                       otherwise, and the result says when it came from there. When the endpoint cannot be \
                       introspected, an older copy stands in and every tool that reads it says it may be out of \
                       date. Pass `refresh` only when the API has changed since it was loaded.",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    async fn introspect(&self, Parameters(arguments): Parameters<IntrospectArguments>) -> ToolResult {
        let name = &arguments.endpoint;
        let schema = self.schema(name, arguments.refresh).await?;
        let endpoint = self.endpoint(name)?;
        let url = &endpoint.shown_url;
        let source = match (&schema.origin, &endpoint.schema) {
            (Origin::File, Some(path)) => format!("read from the file {}", path.display()),
            (Origin::File | Origin::Introspected, _) => format!("loaded by introspection from {url}"),
            (Origin::Kept(age), _) => format!(
                "loaded by introspection from {url} {} ago and read from fieldtrip's cache, which keeps it for \
                 {} (`schema_ttl`). If the API has changed since, call again with `refresh: true`",
                show_duration(*age),
                show_duration(endpoint.schema_ttl)
            ),
            (Origin::Fallback(age, error), _) => format!(
                "read from fieldtrip's cache, where it was kept {} ago. It may be out of date: {url} could not be \
                 introspected just now, so this copy stands in, and `refresh: true` tries again.\nIntrospection \
                 failed with: {}",
                show_duration(*age),
                error.trim_end_matches('.')
            ),
        };
        Ok(format!(
            "Schema for `{name}`, {source}.\n{}{}{}",
            schema.overview(endpoint.mutations),
            schema.cost_map_report(&endpoint.costs),
            schema.glossary_report(&endpoint.glossary)
        ))
    }
}

impl Fieldtrip {
    /// Refuses `text` when it holds a mutation and `endpoint` denies them, and records the refusal in the audit log
    /// with the `variables` given.
    async fn refuse_denied(
        &self,
        name: &str,
        endpoint: &Endpoint,
        text: &str,
        variables: &Variables,
    ) -> Result<(), String> {
        let Err(refusal) = approval::refuse_denied_mutation(name, endpoint, text) else {
            return Ok(());
        };
        let operation = audit::mutation_name(text);
        let attempt = Attempt {
            endpoint: name,
            kind: Kind::Mutation,
            operation: operation.as_deref(),
            text,
            variables,
            cost: None,
            series: Series::Single,
        };
        let verdict = Verdict {
            approval: "denied",
            reason: Some("mutations are turned off for this endpoint by `mutations = \"deny\"`".to_owned()),
        };
        self.audit.record(endpoint, &attempt, &verdict, Outcome::NotSent).await;
        Err(refusal)
    }

    /// The server's instructions, which mention `history` only when it is offered.
    fn instructions(&self) -> String {
        if self.audit.path().is_some() {
            format!("{INSTRUCTIONS}{HISTORY_INSTRUCTIONS}")
        } else {
            INSTRUCTIONS.to_owned()
        }
    }

    /// The `max_output_chars` and the inline limit that apply to the result called `id`: those of the endpoint it
    /// is named after, or the smallest of any endpoint when that one is no longer configured.
    fn limits_of(&self, id: &str) -> (usize, usize) {
        let endpoints = &self.config.endpoints;
        let named = endpoints
            .iter()
            .filter(|(name, _)| id.strip_prefix(name.as_str()).is_some_and(|rest| rest.starts_with('-')))
            .max_by_key(|(name, _)| name.len());
        named.map_or_else(
            || {
                let smallest =
                    |limit: fn(&Endpoint) -> usize| endpoints.values().map(limit).min().unwrap_or(usize::MAX);
                (
                    smallest(|endpoint| endpoint.max_output_chars),
                    smallest(Endpoint::inline_limit),
                )
            },
            |(_, endpoint)| (endpoint.max_output_chars, endpoint.inline_limit()),
        )
    }

    /// A note for `history` about the attempts in `lines` whose response was written to a file: which files are
    /// still there, and how many are gone. Empty when no line names a result.
    fn result_files(&self, lines: &[String]) -> String {
        self.results.sweep();
        let mut there = Vec::new();
        let mut gone = 0_usize;
        for line in lines {
            let Ok(entry) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };
            let text = |key: &str| entry.get(key).and_then(serde_json::Value::as_str);
            let (Some(session), Some(id)) = (text("session"), text("result")) else {
                continue;
            };
            match results::existing(session, id) {
                Some(path) => there.push(format!("`{id}` in {}", path.display())),
                None => gone += 1,
            }
        }
        let deleted = match gone {
            0 => String::new(),
            1 => " 1 other has been deleted.".to_owned(),
            many => format!(" {many} others have been deleted."),
        };
        let kept = show_duration(self.config.result_ttl);
        if there.is_empty() && gone == 0 {
            String::new()
        } else if there.is_empty() {
            format!(
                "\n[The responses these attempts name by `result` were in files that have been deleted, which \
                 happens {kept} after one is written (`result_ttl`). Run an operation again to see its data.]"
            )
        } else {
            format!(
                "\n[Responses still in files, named by `result` above. `read_result` returns any part of one \
                 without sending anything: {}.{deleted} A file is deleted {kept} after it is written (`result_ttl`).]",
                there.join(", ")
            )
        }
    }

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
    async fn schema(&self, name: &str, refresh: bool) -> Result<Arc<Held>, String> {
        let endpoint = self.endpoint(name)?;
        let mut schemas = self.schemas.lock().await;
        if let Some(loaded) = schemas.get(name).filter(|_| !refresh) {
            return Ok(Arc::clone(loaded));
        }
        let loaded = Arc::new(self.load(name, endpoint, refresh).await?);
        schemas.insert(name.to_owned(), Arc::clone(&loaded));
        drop(schemas);
        Ok(loaded)
    }

    /// Loads the schema for `endpoint`, and warns about each saved operation that no longer validates against it.
    async fn load(&self, name: &str, endpoint: &Endpoint, refresh: bool) -> Result<Held, String> {
        let held = self.read_schema(name, endpoint, refresh).await?;
        for saved in &endpoint.operations.operations {
            if let Err(error) = saved.plan(&held, name, endpoint) {
                tracing::warn!(
                    "the saved operation `{}` for endpoint `{name}` ({}{}) no longer validates against the schema: {}",
                    saved.name,
                    saved.file.display(),
                    saved.with_fragments(),
                    error.problems().join("; ")
                );
            }
        }
        Ok(held)
    }

    /// Logs each file left out of a library, and checks the saved operations of every endpoint whose schema is a
    /// file, so that one broken by a change to the schema is reported at startup. Endpoints that load their schema by
    /// introspection are checked on first use, because startup sends nothing.
    pub(crate) async fn check_saved_operations(&self) {
        for (name, endpoint) in &self.config.endpoints {
            let library = &endpoint.operations;
            for left_out in library.rejected.iter().chain(library.left_out_fragments()) {
                tracing::warn!(
                    "left out of the saved operations for endpoint `{name}`: {}",
                    left_out.problem
                );
            }
            if endpoint.schema.is_none() || endpoint.operations.is_empty() {
                continue;
            }
            if let Err(error) = self.schema(name, false).await {
                tracing::warn!("the saved operations for endpoint `{name}` could not be checked: {error}");
            }
        }
    }

    /// Reads the schema for `endpoint` from its file, or by introspection when it has none.
    ///
    /// An introspected schema is kept in fieldtrip's cache for `schema_ttl`, and read from there until then unless
    /// `refresh` is set. When introspection fails, a copy of any age stands in, unless `refresh` asked for a new one.
    async fn read_schema(&self, name: &str, endpoint: &Endpoint, refresh: bool) -> Result<Held, String> {
        if let Some(path) = &endpoint.schema {
            let text = std::fs::read_to_string(path).map_err(|error| {
                format!(
                    "Could not read the schema file {} for endpoint `{name}`: {error}",
                    path.display()
                )
            })?;
            let schema = Loaded::from_sdl(&text, &format!("The schema file {}", path.display()))?;
            return Ok(Held {
                schema,
                origin: Origin::File,
            });
        }
        let caching = !endpoint.schema_ttl.is_zero();
        let kept = if caching {
            cache::read_schema(&endpoint.url)
        } else {
            None
        };
        // A copy that does not parse, written by an older fieldtrip perhaps, is as good as none.
        let kept = kept.and_then(|kept| Some((Loaded::from_sdl(&kept.sdl, "The cached schema").ok()?, kept.age)));
        let (kept, age) = match kept {
            Some((schema, age)) if !refresh && age < endpoint.schema_ttl => {
                return Ok(Held {
                    schema,
                    origin: Origin::Kept(age),
                });
            }
            Some((schema, age)) if !refresh => (Some(schema), age),
            _ => (None, std::time::Duration::ZERO),
        };
        let introspected = match introspection::fetch(&self.http, endpoint).await {
            Ok(sdl) => sdl,
            Err(error) => {
                return match kept {
                    Some(schema) => {
                        tracing::warn!("`{name}` could not be introspected, so its cached schema stands in: {error}");
                        Ok(Held {
                            schema,
                            origin: Origin::Fallback(age, error),
                        })
                    }
                    None => Err(error),
                };
            }
        };
        let schema = Loaded::from_sdl(
            &introspected,
            &format!("The introspection result from {}", endpoint.shown_url),
        )?;
        if caching && let Err(error) = cache::write_schema(&endpoint.url, &endpoint.shown_url, &introspected) {
            tracing::warn!("the schema of `{name}` could not be kept in the cache: {error}");
        }
        Ok(Held {
            schema,
            origin: Origin::Introspected,
        })
    }
}

#[expect(
    clippy::unused_async_trait_impl,
    reason = "the async fn is generated by rmcp's `tool_handler` macro"
)]
#[tool_handler(router = self.tools)]
impl ServerHandler for Fieldtrip {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new(env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION")))
            .with_instructions(self.instructions())
    }

    async fn on_initialized(&self, context: NotificationContext<RoleServer>) {
        let client = context.peer.peer_info().map(|info| info.client_info.clone());
        let Some(client) = approval::auto_accepting(client.as_ref()) else {
            return;
        };
        let asking: Vec<&str> = self
            .config
            .endpoints
            .iter()
            .filter(|(_, endpoint)| endpoint.approval == Approval::Elicit)
            .map(|(name, _)| name.as_str())
            .collect();
        if !asking.is_empty() {
            tracing::warn!(
                "{client} accepts fieldtrip's approval prompts without showing them, so `execute` will refuse every \
                 operation that needs one on {}. Set `approval = \"client\"` on those endpoints and add a permission \
                 rule in {client} that asks before `fieldtrip_execute`.",
                backticked(asking.into_iter())
            );
        }
    }
}
