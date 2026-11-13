//! Asking the human before anything is sent.
//!
//! Approval comes from the person at the MCP client, through elicitation. The calling agent is the party being
//! gated, so nothing it can put in a tool call counts as approval.

use std::collections::{BTreeMap, HashSet};
use std::fmt::Write as _;
use std::hash::{BuildHasher as _, RandomState};
use std::sync::Mutex;

use rmcp::model::{
    ElicitRequest, ElicitRequestParams, ElicitationAction, ElicitationSchema, InputRequest, InputRequiredResult,
    InputResponses, ProtocolVersion,
};
use rmcp::service::{ElicitationMode, RequestContext};
use rmcp::{Peer, RoleServer};

use crate::config::{Approval, Endpoint};
use crate::plan::{Kind, Plan, Variables};

/// What came of asking.
#[derive(Debug)]
pub(crate) enum Decision {
    /// The human accepted.
    Approved,
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
    /// `Ok` for an approval. Anything else becomes the error the calling agent sees, which says that nothing was
    /// sent to `endpoint` and what to do next.
    pub(crate) fn into_result(self, endpoint: &str) -> Result<(), String> {
        match self {
            Self::Approved => Ok(()),
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

/// Why `plan` may be sent to `endpoint` without asking, when the user's config says so. `None` means ask.
///
/// Both ways out are set by the user in the config file. Mutations are only ever waived by `approval = "client"`.
pub(crate) fn waiver(endpoint: &Endpoint, plan: &Plan) -> Option<String> {
    if endpoint.approval == Approval::Client {
        return Some("approval is left to the MCP client for this endpoint".to_owned());
    }
    let threshold = endpoint
        .auto_approve_cost
        .filter(|threshold| plan.kind == Kind::Query && plan.cost <= *threshold)?;
    Some(format!("the cost is within `auto_approve_cost` ({threshold})"))
}

/// One line saying who approves operations for `endpoint`, for `list_endpoints`.
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

/// Shows `prompt` to the human and waits for the answer. Only [`Decision::Approved`] allows sending.
pub(crate) async fn ask(peer: &Peer<RoleServer>, prompt: String) -> Decision {
    if !peer.supported_elicitation_modes().contains(&ElicitationMode::Form) {
        return Decision::Unavailable;
    }
    let request = ElicitRequestParams::FormElicitationParams {
        meta: None,
        message: prompt,
        // No fields to fill in: the prompt is a plain accept or decline.
        requested_schema: ElicitationSchema::new(BTreeMap::new()),
    };
    match peer.create_elicitation(request).await {
        Ok(result) => match result.action {
            ElicitationAction::Accept => Decision::Approved,
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
pub(crate) fn asks_by_round_trip(context: &RequestContext<RoleServer>) -> bool {
    context
        .protocol_version()
        .is_some_and(|version| version >= ProtocolVersion::V_2026_07_28)
}

/// What one call of `execute` does about approval on a round-trip revision.
#[derive(Debug)]
pub(crate) enum Round {
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
pub(crate) struct Outstanding {
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
    pub(crate) fn round(
        &self,
        context: &RequestContext<RoleServer>,
        operation: &Operation<'_>,
        prompt: String,
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
            return Round::Ask(self.ask(operation, prompt));
        };
        if !self.redeem(state, operation) {
            return Round::Decided(Decision::Failed(
                "the approval that came back was not given for this operation, or was already used".to_owned(),
            ));
        }
        let action = responses.get(APPROVAL_KEY).and_then(|response| response.get("action"));
        Round::Decided(match action.and_then(|action| action.as_str()) {
            Some("accept") => Decision::Approved,
            Some("decline") => Decision::Declined,
            Some("cancel") => Decision::Cancelled,
            _ => Decision::Failed("the client did not answer the approval prompt".to_owned()),
        })
    }

    /// Issues a ticket for `operation` and builds the result that asks the human.
    fn ask(&self, operation: &Operation<'_>, prompt: String) -> InputRequiredResult {
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
            requested_schema: ElicitationSchema::new(BTreeMap::new()),
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
        let variables = serde_json::Value::Object(operation.variables.clone()).to_string();
        self.key
            .hash_one((number, operation.endpoint, operation.text, variables))
    }
}

/// What an approval covers: where the operation goes, its canonical text, and its variables.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Operation<'call> {
    /// The endpoint's name.
    pub(crate) endpoint: &'call str,
    /// The operation in canonical form, as it is shown and sent.
    pub(crate) text: &'call str,
    /// The variables that are sent with it.
    pub(crate) variables: &'call Variables,
}
