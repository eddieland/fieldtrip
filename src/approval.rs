//! Asking the human before anything is sent.
//!
//! Approval comes from the person at the MCP client, through elicitation. The calling agent is the party being
//! gated, so nothing it can put in a tool call counts as approval.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use rmcp::model::{ElicitRequestParams, ElicitationAction, ElicitationSchema};
use rmcp::service::ElicitationMode;
use rmcp::{Peer, RoleServer};

use crate::plan::{Plan, Variables};

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
