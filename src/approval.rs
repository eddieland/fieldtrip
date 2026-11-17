//! Asking the human before anything is sent.
//!
//! Approval comes from the person at the MCP client, through elicitation. The calling agent is the party being
//! gated, so nothing it can put in a tool call counts as approval.

use std::collections::{BTreeMap, HashSet};
use std::fmt::Write as _;
use std::hash::{BuildHasher as _, RandomState};
use std::sync::Mutex;

use rmcp::model::{
    ElicitRequest, ElicitRequestParams, ElicitationAction, ElicitationSchema, Implementation, InputRequest,
    InputRequiredResult, InputResponses, ProtocolVersion,
};
use rmcp::service::{ElicitationMode, RequestContext};
use rmcp::{Peer, RoleServer};

use crate::audit::Verdict;
use crate::config::{Approval, Endpoint, Mutations};
use crate::plan::{Kind, Plan, Variables};

/// What came of asking.
#[derive(Debug)]
enum Decision {
    /// The human accepted this one request.
    Approved,
    /// The human accepted, and chose to let the same query run for the rest of the session.
    ApprovedForSession,
    /// The human declined.
    Declined,
    /// The human dismissed the prompt without answering.
    Cancelled,
    /// The client cannot show a prompt.
    Unavailable,
    /// The prompt could not be delivered or answered.
    Failed(String),
}

impl Decision {
    /// The word the audit log records for this decision.
    pub(crate) const fn label(&self) -> &'static str {
        match self {
            Self::Approved | Self::ApprovedForSession => "accepted",
            Self::Declined => "declined",
            Self::Cancelled => "cancelled",
            Self::Unavailable => "unavailable",
            Self::Failed(_) => "failed",
        }
    }

    /// Why nothing was sent, in a few words, for the audit log. `None` for a plain approval.
    pub(crate) fn reason(&self) -> Option<String> {
        match self {
            Self::Approved => None,
            Self::ApprovedForSession => Some("the user approved this query for the rest of the session".to_owned()),
            Self::Declined => Some("the user declined".to_owned()),
            Self::Cancelled => Some("the user dismissed the prompt without answering".to_owned()),
            Self::Unavailable => Some("the MCP client cannot show approval prompts".to_owned()),
            Self::Failed(reason) => Some(format!("the approval prompt failed: {reason}")),
        }
    }

    /// `Ok` for an approval. Anything else becomes the error the calling agent sees, which says that nothing was
    /// sent to `endpoint` and what to do next.
    pub(crate) fn into_result(self, endpoint: &str) -> Result<(), String> {
        match self {
            Self::Approved | Self::ApprovedForSession => Ok(()),
            Self::Declined => Err(format!(
                "The user declined. Nothing was sent to `{endpoint}`. Do not send this operation again unless the \
                 user asks for it."
            )),
            Self::Cancelled => Err(format!(
                "The user dismissed the approval prompt without answering. Nothing was sent to `{endpoint}`. Ask the \
                 user whether they want this operation to run."
            )),
            Self::Unavailable => Err(format!(
                "Nothing was sent to `{endpoint}`. This MCP client cannot show approval prompts, because it does not \
                 support elicitation, so `execute` has no way to ask the user. The user can either use a client \
                 that supports elicitation, or set `approval = \"client\"` for this endpoint in the fieldtrip \
                 config to rely on the client's own tool approval."
            )),
            Self::Failed(reason) => Err(format!(
                "Nothing was sent to `{endpoint}`. The approval prompt failed: {reason}"
            )),
        }
    }
}

/// Refuses `operation` when it holds a mutation and `endpoint` denies them. `name` is the endpoint's name.
///
/// This runs on the parsed text alone, before validation, so a mutation is refused before the calling agent can
/// read any validation error as something to fix and try again. A document that does not parse may still show a
/// mutation, and is refused all the same.
///
/// # Errors
///
/// Returns the refusal, which names the setting and says what the calling agent can still do.
pub(crate) fn refuse_denied_mutation(name: &str, endpoint: &Endpoint, operation: &str) -> Result<(), String> {
    if endpoint.mutations != Mutations::Deny {
        return Ok(());
    }
    let document =
        apollo_compiler::ast::Document::parse(operation, "operation.graphql").unwrap_or_else(|invalid| invalid.partial);
    let has_mutation = document.definitions.iter().any(|definition| {
        matches!(
            definition,
            apollo_compiler::ast::Definition::OperationDefinition(operation)
                if operation.operation_type == apollo_compiler::ast::OperationType::Mutation
        )
    });
    if has_mutation {
        return Err(format!(
            "Nothing was sent to `{name}`. The user's fieldtrip config turns mutations off for this endpoint with \
             `mutations = \"deny\"`, so `execute` refuses every mutation here, whoever approves it. Do not retry it \
             on this endpoint. `estimate_cost` still validates and costs a mutation, so you can give the user a valid \
             one to run themselves, or ask whether another endpoint in `list_endpoints` is meant for writes."
        ));
    }
    Ok(())
}

/// The MCP clients known to accept a prompt with no fields without showing it to anyone, by the `clientInfo.name`
/// they give, in lowercase, and the name to show. A mutation's prompt has no fields and a query's one field is
/// optional, so asking one of these would approve the operation unseen.
const AUTO_ACCEPTING: [(&str, &str); 1] = [("opencode", "OpenCode")];

/// The name of the client, when `client` is one that accepts fieldtrip's prompts without showing them.
pub(crate) fn auto_accepting(client: Option<&Implementation>) -> Option<&'static str> {
    let name = client?.name.to_lowercase();
    AUTO_ACCEPTING
        .iter()
        .find(|(known, _)| *known == name)
        .map(|(_, shown)| *shown)
}

/// The refusal for an operation on the endpoint called `name` that would have to be approved by asking `client`,
/// which accepts without showing the prompt.
fn unseen(name: &str, client: &str) -> String {
    format!(
        "Nothing was sent to `{name}`. This MCP client, {client}, accepts fieldtrip's approval prompts without showing \
         them to the user, so asking it would approve the operation unseen, and fieldtrip does not ask it. Tell the \
         user. To run operations here from {client}, the user sets `approval = \"client\"` for this endpoint in the \
         fieldtrip config and adds a permission rule in {client} that asks before `fieldtrip_execute`, so \
         that {client}'s own prompt approves each one. fieldtrip's guide for {client} has the steps. Do not try to get \
         around this."
    )
}

/// Why an operation may be sent without asking.
#[derive(Debug)]
struct Waiver {
    /// The word the audit log records: `client` or `auto`.
    pub(crate) label: &'static str,
    /// The reason, as a clause: `the cost is within ...`.
    pub(crate) reason: String,
}

/// Why an operation of `kind` and estimated `cost` may be sent to `endpoint` without asking, when the user's config
/// says so, or when the user `approved` this very query for the session. `None` means ask.
///
/// The config's two ways out are set by the user in the config file, and the third by the user in a prompt.
/// Mutations are only ever waived by `approval = "client"`.
fn waiver(endpoint: &Endpoint, kind: Kind, cost: u64, approved: bool) -> Option<Waiver> {
    if endpoint.approval == Approval::Client {
        return Some(Waiver {
            label: "client",
            reason: "approval is left to the MCP client for this endpoint".to_owned(),
        });
    }
    if kind != Kind::Query {
        return None;
    }
    if let Some(threshold) = endpoint.auto_approve_cost.filter(|threshold| cost <= *threshold) {
        return Some(Waiver {
            label: "auto",
            reason: format!("the cost is within `auto_approve_cost` ({threshold})"),
        });
    }
    approved.then(|| Waiver {
        label: "session",
        reason: "they approved this query, with any variables, for the rest of the session".to_owned(),
    })
}

/// The queries the user has approved for the rest of the session, from the prompt's one field.
///
/// An approval covers one endpoint and one canonical text, whatever the variables. It is kept by the SHA-256 the
/// audit log records, lives in memory only, and so ends when the server does. Mutations are never in here.
#[derive(Debug, Default)]
struct Standing {
    /// Endpoint names with the hash of an approved text.
    approved: Mutex<HashSet<(String, String)>>,
}

impl Standing {
    /// Whether the user approved the query `text` on `endpoint` for the session.
    fn covers(&self, endpoint: &str, text: &str) -> bool {
        self.approved
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains(&(endpoint.to_owned(), crate::audit::hash(text)))
    }

    /// Records that the user approved the query `text` on `endpoint` for the session.
    fn grant(&self, endpoint: &str, text: &str) {
        self.approved
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert((endpoint.to_owned(), crate::audit::hash(text)));
    }
}

/// The name of the prompt's one field, which a query's prompt has and a mutation's does not.
const SCOPE: &str = "approve";
/// The field's default: the accept covers the request shown and nothing else.
const ONCE: &str = "this request only";
/// The field's other value: the accept also covers later runs of the same query.
const FOR_SESSION: &str = "this query with any variables, until fieldtrip restarts";

/// The form of an approval prompt. With `offer_session` it has one optional field, which lets the human approve the
/// query for the rest of the session. Without, it has none.
///
/// The field is optional and defaults to one request, so an accept that fills nothing in, as from a client that
/// accepts without showing the prompt, approves no more than the request it was asked about.
fn form(offer_session: bool) -> ElicitationSchema {
    let plain = || ElicitationSchema::new(BTreeMap::new());
    if !offer_session {
        return plain();
    }
    serde_json::from_value(serde_json::json!({
        "type": "object",
        "properties": {
            SCOPE: {
                "type": "string",
                "title": "Approve",
                "description": "Choose the second to stop being asked about this query: it then runs without a \
                                prompt, whatever its variables, until fieldtrip restarts.",
                "enum": [ONCE, FOR_SESSION],
                "default": ONCE,
            },
        },
    }))
    .unwrap_or_else(|_error| plain())
}

/// What an accept approves, from the `content` that came with it. Only an explicit choice of the session, on a
/// prompt that offered it, is more than the one request.
fn accepted(offered_session: bool, content: Option<&serde_json::Value>) -> Decision {
    let chosen = content
        .and_then(|content| content.get(SCOPE))
        .and_then(serde_json::Value::as_str);
    if offered_session && chosen == Some(FOR_SESSION) {
        Decision::ApprovedForSession
    } else {
        Decision::Approved
    }
}

/// One line saying who approves operations for `endpoint`, for `list_endpoints`. `unseen` is the name of a client
/// that would accept fieldtrip's prompts without showing them, when the caller is one.
pub(crate) fn policy_for(endpoint: &Endpoint, unseen: Option<&str>) -> String {
    let policy = policy(endpoint);
    match unseen {
        Some(client) if endpoint.approval == Approval::Elicit => format!(
            "{policy}, but this client, {client}, accepts those prompts unseen, so `execute` refuses every operation \
             that would need one"
        ),
        _ => policy,
    }
}

/// One line saying who approves operations for `endpoint`, for `fieldtrip check` and `list_endpoints`.
pub(crate) fn policy(endpoint: &Endpoint) -> String {
    match (endpoint.approval, endpoint.auto_approve_cost) {
        (Approval::Client, _) => "left to the MCP client".to_owned(),
        (Approval::Elicit, None) => "the user is asked before every operation".to_owned(),
        (Approval::Elicit, Some(threshold)) => {
            format!("the user is asked, except for queries with an estimated cost of {threshold} or less")
        }
    }
}

/// The text of the approval prompt for `plan`.
///
/// Everything in it comes from the parsed operation, the variables and the config. It is what will be sent, and
/// nothing the calling agent wrote about it.
pub(crate) fn prompt(endpoint: &str, url: &str, plan: &Plan, variables: &Variables) -> String {
    let kind = plan.kind.keyword();
    let article = if kind.starts_with(['a', 'e', 'i', 'o', 'u']) {
        "an"
    } else {
        "a"
    };
    let mut out = format!(
        "fieldtrip wants to send {article} {kind} to `{endpoint}` ({url}).\n\n{}\n\n",
        plan.text
    );
    if variables.is_empty() {
        out.push_str("Variables: none\n");
    } else {
        let _ = writeln!(out, "Variables: {}", serde_json::Value::Object(variables.clone()));
    }
    let _ = write!(
        out,
        "Estimated cost: {}\n\nAccept to send it. Decline to stop.",
        plan.cost
    );
    for assumption in &plan.assumptions {
        let _ = write!(out, "\nThe estimate assumes: {assumption}");
    }
    out
}

/// The text of the approval prompt for following pages: `plan` sent up to `follow.max_pages` times, each page
/// costing what `plan` does, with `variables` on the first.
///
/// Like [`prompt`], it holds nothing the calling agent wrote but the operation, the variables, and here the path and
/// the variable name, which are checked to be plain names.
pub(crate) fn pages_prompt(
    endpoint: &str,
    url: &str,
    plan: &Plan,
    variables: &Variables,
    follow: &crate::paginate::Follow,
) -> String {
    let pages = follow.max_pages;
    let mut out = format!(
        "fieldtrip wants to send a {} to `{endpoint}` ({url}) up to {pages} times, to follow the pages of `{}`.\n\n\
         {}\n\n",
        plan.kind.keyword(),
        follow.path,
        plan.text
    );
    if variables.is_empty() {
        out.push_str("Variables: none\n");
    } else {
        let _ = writeln!(out, "Variables: {}", serde_json::Value::Object(variables.clone()));
    }
    let worst = plan.cost.saturating_mul(u64::try_from(pages).unwrap_or(u64::MAX));
    let _ = write!(
        out,
        "After the first page, `${}` is the `{}` of the page before.\nEstimated cost: {worst} at most, {} for each \
         page\n\nAccept to send up to {pages} pages. Decline to stop.",
        follow.variable,
        follow.cursor_name(),
        plan.cost
    );
    for assumption in &plan.assumptions {
        let _ = write!(out, "\nThe estimate assumes: {assumption}");
    }
    out
}

/// How many sets of variables the prompt for a batch shows. The rest are counted.
const BATCH_SAMPLE: usize = 5;

/// The text of the approval prompt for a batch: `plan` sent once for each of `sets`, at a `total` estimated cost of
/// which `costliest` is the most one run costs.
///
/// Like [`prompt`], it holds nothing the calling agent wrote but the operation and the variables. The sets are
/// numbered as the result numbers them, from 0, and past the first few they are only counted.
pub(crate) fn batch_prompt(
    endpoint: &str,
    url: &str,
    plan: &Plan,
    sets: &[Variables],
    total: u64,
    costliest: u64,
) -> String {
    let runs = sets.len();
    let mut out = format!(
        "fieldtrip wants to send a {} to `{endpoint}` ({url}) {runs} times, once for each set of variables.\n\n{}\n\n\
         Variables, {runs} sets:\n",
        plan.kind.keyword(),
        plan.text
    );
    for (index, variables) in sets.iter().enumerate().take(BATCH_SAMPLE) {
        let _ = writeln!(out, "[{index}] {}", serde_json::Value::Object(variables.clone()));
    }
    if runs > BATCH_SAMPLE {
        let _ = writeln!(out, "… and {} more sets, which are not shown.", runs - BATCH_SAMPLE);
    }
    let _ = write!(
        out,
        "Estimated cost: {total} in total, {costliest} for the costliest run\n\nAccept to send all {runs}. Decline to \
         stop."
    );
    for assumption in &plan.assumptions {
        let _ = write!(out, "\nThe estimate assumes: {assumption}");
    }
    out
}

/// Shows `prompt` to the human and waits for the answer. Only an approval allows sending. With `offer_session` the
/// prompt has the field that approves a query for the session.
async fn ask(peer: &Peer<RoleServer>, prompt: String, offer_session: bool) -> Decision {
    if !peer.supported_elicitation_modes().contains(&ElicitationMode::Form) {
        return Decision::Unavailable;
    }
    let request = ElicitRequestParams::FormElicitationParams {
        meta: None,
        message: prompt,
        requested_schema: form(offer_session),
    };
    match peer.create_elicitation(request).await {
        Ok(result) => match result.action {
            ElicitationAction::Accept => accepted(offer_session, result.content.as_ref()),
            ElicitationAction::Decline => Decision::Declined,
            ElicitationAction::Cancel => Decision::Cancelled,
            // The action list can grow. Anything that is not a plain accept is treated as a refusal.
            _ => Decision::Failed("the client answered with an action fieldtrip does not know".to_owned()),
        },
        Err(error) => Decision::Failed(error.to_string()),
    }
}

/// The key the approval request and its answer travel under in an "input required" round trip.
const APPROVAL_KEY: &str = "approval";
/// The most prompts that can be waiting for an answer. Past it, the oldest ones are forgotten and must be asked
/// again.
const MAX_OUTSTANDING: usize = 1024;

/// Whether the request came in on a protocol revision where the server cannot send a request to the client.
///
/// From revision 2026-07-28 on, a server asks for input by answering the tool call with an "input required"
/// result. The client fulfils it and calls again.
fn asks_by_round_trip(context: &RequestContext<RoleServer>) -> bool {
    context
        .protocol_version()
        .is_some_and(|version| version >= ProtocolVersion::V_2026_07_28)
}

/// What `execute` does next with an operation that is valid and ready to send.
#[derive(Debug)]
pub(crate) enum Gate {
    /// Send it. `unasked` is the sentence that tells the calling agent the human was not asked, or is empty.
    Open {
        /// What the audit log records about the approval.
        verdict: Verdict,
        /// Why no prompt was shown, as a sentence for the tool result. Empty when the human accepted a prompt.
        unasked: String,
    },
    /// On a round-trip revision, the human has not been asked yet. The call ends with this result, which asks.
    Ask(InputRequiredResult),
    /// Nothing is sent.
    Shut {
        /// What the audit log records about the refusal.
        verdict: Verdict,
        /// The error the calling agent sees.
        refusal: String,
    },
}

/// What came back with a call of `execute` on a round-trip revision: nothing on the first call, and the ticket and
/// the human's answer on the retry.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Returned<'call> {
    /// The request state, which is the ticket of the prompt being answered.
    pub(crate) state: Option<&'call str>,
    /// The answers to the input requests of the first call.
    pub(crate) responses: Option<&'call InputResponses>,
}

/// What a tool asks to send, as the approval step sees it.
pub(crate) struct Request<'call> {
    /// The tool that asks.
    pub(crate) tool: &'static str,
    /// The endpoint's name.
    pub(crate) name: &'call str,
    /// The endpoint.
    pub(crate) endpoint: &'call Endpoint,
    /// Whether the operation is a query or a mutation.
    pub(crate) kind: Kind,
    /// The operation's canonical text.
    pub(crate) text: &'call str,
    /// The estimated cost of everything the call would send.
    pub(crate) cost: u64,
    /// The prompt to show when the human has to be asked.
    pub(crate) prompt: String,
    /// The variables, and whatever else about the call decides what is sent. An approval covers exactly this.
    pub(crate) covered: &'call serde_json::Value,
    /// Whether the prompt lets the human approve the query for the rest of the session.
    pub(crate) offer_session: bool,
}

/// What the server remembers about approvals between calls.
#[derive(Debug, Default)]
pub(crate) struct Approvals {
    /// Approval prompts that are waiting for an answer, on protocol revisions that ask by round trip.
    outstanding: Outstanding,
    /// The queries the user approved for the rest of the session.
    standing: Standing,
}

/// Decides whether `request` may be sent, asking the human when the config calls for it.
///
/// This is the only way to a [`Gate::Open`]: the user's config waives the prompt, the user approved this very query
/// for the session, or the human accepted the prompt. Every other path ends in a refusal or, on a round-trip
/// revision, in the question itself.
pub(crate) async fn gate(
    approvals: &Approvals,
    context: &RequestContext<RoleServer>,
    request: Request<'_>,
    returned: Returned<'_>,
) -> Gate {
    let Request { name, endpoint, .. } = request;
    let is_query = request.kind == Kind::Query;
    let standing = is_query && approvals.standing.covers(name, request.text);
    if let Some(waiver) = waiver(endpoint, request.kind, request.cost, standing) {
        return Gate::Open {
            unasked: format!(" The user was not asked: {}.", waiver.reason),
            verdict: Verdict {
                approval: waiver.label,
                reason: Some(waiver.reason),
            },
        };
    }
    if let Some(client) = auto_accepting(context.client_info().as_ref()) {
        tracing::warn!("refused an operation on `{name}`: {client} would accept the prompt unseen");
        return Gate::Shut {
            verdict: Verdict {
                approval: "unavailable",
                reason: Some(format!("{client} accepts approval prompts without showing them")),
            },
            refusal: unseen(name, client),
        };
    }
    let offer_session = request.offer_session && is_query;
    let decision = if asks_by_round_trip(context) {
        let operation = Operation {
            tool: request.tool,
            endpoint: name,
            text: request.text,
            variables: request.covered,
        };
        let round = approvals.outstanding.round(
            context,
            &operation,
            request.prompt,
            offer_session,
            returned.state,
            returned.responses,
        );
        match round {
            Round::Ask(question) => return Gate::Ask(question),
            Round::Decided(decision) => decision,
        }
    } else {
        ask(&context.peer, request.prompt, offer_session).await
    };
    let verdict = Verdict {
        approval: decision.label(),
        reason: decision.reason(),
    };
    let for_session = matches!(decision, Decision::ApprovedForSession);
    match decision.into_result(name) {
        Ok(()) if for_session => {
            approvals.standing.grant(name, request.text);
            Gate::Open {
                verdict,
                unasked: " The user approved this query for the rest of the session: until fieldtrip restarts it \
                          runs without a prompt, whatever its variables."
                    .to_owned(),
            }
        }
        Ok(()) => Gate::Open {
            verdict,
            unasked: String::new(),
        },
        Err(refusal) => Gate::Shut { verdict, refusal },
    }
}

/// What one call of `execute` does about approval on a round-trip revision.
#[derive(Debug)]
enum Round {
    /// The human has not been asked yet. This result asks them, and the call ends here.
    Ask(InputRequiredResult),
    /// The call came back with an answer.
    Decided(Decision),
}

/// The prompts that have been sent out and not answered yet.
///
/// A prompt and its answer arrive in two separate calls, so the answer has to be tied to the operation the human
/// was shown. Each prompt gets a ticket: a number used once, and a keyed fingerprint of that number with the
/// operation. The ticket goes out as the request state and must come back with the answer. An accept counts only
/// when its ticket is outstanding and its fingerprint matches the operation in the call that carries it.
#[derive(Debug, Default)]
struct Outstanding {
    /// Keys the fingerprints. It is random per process, so a ticket means nothing to another server.
    key: RandomState,
    /// The numbers of the prompts that are waiting for an answer.
    numbers: Mutex<HashSet<u64>>,
}

impl Outstanding {
    /// Handles one call of `execute` on a round-trip revision.
    ///
    /// With no `responses` this is the first call, and the result asks the human. With `responses` it is the
    /// retry, and the decision is whatever the human answered, provided `state` is the ticket for this operation.
    fn round(
        &self,
        context: &RequestContext<RoleServer>,
        operation: &Operation<'_>,
        prompt: String,
        offer_session: bool,
        state: Option<&str>,
        responses: Option<&InputResponses>,
    ) -> Round {
        let can_prompt = context
            .client_capabilities()
            .and_then(|capabilities| capabilities.elicitation)
            .is_some_and(|elicitation| elicitation.form.is_some() || elicitation.url.is_none());
        if !can_prompt {
            return Round::Decided(Decision::Unavailable);
        }
        let Some(responses) = responses else {
            return Round::Ask(self.ask(operation, prompt, offer_session));
        };
        if !self.redeem(state, operation) {
            return Round::Decided(Decision::Failed(
                "the approval that came back was not given for this operation, or was already used".to_owned(),
            ));
        }
        let response = responses.get(APPROVAL_KEY);
        let action = response.and_then(|response| response.get("action"));
        Round::Decided(match action.and_then(|action| action.as_str()) {
            Some("accept") => accepted(offer_session, response.and_then(|response| response.get("content"))),
            Some("decline") => Decision::Declined,
            Some("cancel") => Decision::Cancelled,
            _ => Decision::Failed("the client did not answer the approval prompt".to_owned()),
        })
    }

    /// Issues a ticket for `operation` and builds the result that asks the human.
    fn ask(&self, operation: &Operation<'_>, prompt: String, offer_session: bool) -> InputRequiredResult {
        // A fresh `RandomState` is keyed differently every time, which makes this a number nobody can predict.
        let number = RandomState::new().hash_one(0_u8);
        {
            let mut numbers = self.numbers.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            if numbers.len() >= MAX_OUTSTANDING {
                numbers.clear();
            }
            numbers.insert(number);
        }
        let ticket = format!("{number:016x}{:016x}", self.fingerprint(number, operation));
        let request = ElicitRequest::new(ElicitRequestParams::FormElicitationParams {
            meta: None,
            message: prompt,
            requested_schema: form(offer_session),
        });
        let requests = BTreeMap::from([(APPROVAL_KEY.to_owned(), InputRequest::Elicitation(request))]);
        InputRequiredResult::new(Some(requests), Some(ticket))
    }

    /// Checks that `ticket` was issued for `operation` and has not been used, and uses it up.
    fn redeem(&self, ticket: Option<&str>, operation: &Operation<'_>) -> bool {
        let Some(ticket) = ticket.filter(|ticket| ticket.len() == 32 && ticket.is_ascii()) else {
            return false;
        };
        let (number, fingerprint) = ticket.split_at(16);
        let (Ok(number), Ok(fingerprint)) = (u64::from_str_radix(number, 16), u64::from_str_radix(fingerprint, 16))
        else {
            return false;
        };
        if fingerprint != self.fingerprint(number, operation) {
            return false;
        }
        self.numbers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&number)
    }

    /// A keyed hash of a ticket number and everything that makes up the operation.
    fn fingerprint(&self, number: u64, operation: &Operation<'_>) -> u64 {
        self.key.hash_one((
            number,
            operation.tool,
            operation.endpoint,
            operation.text,
            operation.variables.to_string(),
        ))
    }
}

/// What an approval covers: the tool that asked, where the operation goes, its canonical text, and its variables.
#[derive(Debug, Clone, Copy)]
struct Operation<'call> {
    /// The tool that asked, so that an approval given to one tool is no use to another.
    pub(crate) tool: &'static str,
    /// The endpoint's name.
    pub(crate) endpoint: &'call str,
    /// The operation in canonical form, as it is shown and sent.
    pub(crate) text: &'call str,
    /// The variables that are sent with it, and whatever else about the call decides what is sent.
    pub(crate) variables: &'call serde_json::Value,
}
