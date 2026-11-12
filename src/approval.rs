//! Asking the human before anything is sent.
//!
//! Approval comes from the person at the MCP client, through elicitation. The calling agent is the party being
//! gated, so nothing it can put in a tool call counts as approval.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use rmcp::model::{ElicitRequestParams, ElicitationAction, ElicitationSchema};
use rmcp::service::ElicitationMode;
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
