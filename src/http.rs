//! The HTTP client that talks to GraphQL endpoints.

use std::error::Error as _;

use crate::config::Endpoint;

/// The `User-Agent` sent with every request.
const USER_AGENT: &str = concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION"));

/// An HTTP client for GraphQL requests.
#[derive(Debug, Clone)]
pub(crate) struct Client {
    http: reqwest::Client,
}

/// What an endpoint answered.
#[derive(Debug)]
pub(crate) struct Reply {
    /// The HTTP status code.
    pub(crate) status: u16,
    /// The response body as text.
    pub(crate) body: String,
}

impl Client {
    /// Builds the client.
    ///
    /// Redirects are refused: following one would carry the endpoint's headers to a host the user never named.
    ///
    /// # Errors
    ///
    /// Returns a message when the TLS backend cannot be set up.
    pub(crate) fn new() -> Result<Self, String> {
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(USER_AGENT)
            .build()
            .map_err(|error| format!("could not set up the HTTP client: {}", explain(&error)))?;
        Ok(Self { http })
    }

    /// Sends `payload` to `endpoint` as a GraphQL-over-HTTP POST and returns whatever came back.
    ///
    /// # Errors
    ///
    /// Returns a message when no response arrives. An HTTP error status is not an error here: GraphQL servers put
    /// useful detail in the body, so the caller decides what the status means.
    pub(crate) async fn post(&self, endpoint: &Endpoint, payload: &serde_json::Value) -> Result<Reply, String> {
        let response = self
            .http
            .post(&endpoint.url)
            .header("content-type", "application/json")
            .header("accept", "application/graphql-response+json, application/json")
            .body(payload.to_string())
            .send()
            .await
            .map_err(|error| format!("The request to {} failed: {}", endpoint.url, explain(&error)))?;
        let status = response.status().as_u16();
        let body = response.text().await.map_err(|error| {
            format!(
                "The response from {} could not be read: {}",
                endpoint.url,
                explain(&error)
            )
        })?;
        Ok(Reply { status, body })
    }
}

/// An error and its causes on one line, without the request URL, which the caller adds where it is useful.
fn explain(error: &reqwest::Error) -> String {
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        let cause_text = cause.to_string();
        if !text.contains(&cause_text) {
            text.push_str(": ");
            text.push_str(&cause_text);
        }
        source = cause.source();
    }
    text
}
