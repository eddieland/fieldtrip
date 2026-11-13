//! The HTTP client that talks to GraphQL endpoints.

use std::error::Error as _;

use crate::config::Endpoint;
use crate::headers::Header;

/// The `User-Agent` sent with every request.
const USER_AGENT: &str = concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION"));

/// The status an endpoint answers with when it does not accept the credentials.
const UNAUTHORIZED: u16 = 401;

/// An HTTP client for GraphQL requests.
#[derive(Debug, Clone)]
pub(crate) struct Client {
    http: reqwest::Client,
}

/// The most of a response body that is read. The rest is dropped, so a huge response cannot exhaust memory.
const MAX_BODY_BYTES: usize = 1024 * 1024;

/// What an endpoint answered.
#[derive(Debug)]
pub(crate) struct Reply {
    /// The HTTP status code.
    pub(crate) status: u16,
    /// The response body as text, up to [`MAX_BODY_BYTES`].
    pub(crate) body: String,
    /// Whether the body was longer than what was read.
    pub(crate) cut_short: bool,
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
    /// When the endpoint answers 401 and one of its headers comes from a command, the command runs again and the
    /// request is retried once: the usual cause is a token that expired while its output was being reused.
    ///
    /// # Errors
    ///
    /// Returns a message when a header cannot be resolved or no response arrives. An HTTP error status is not an
    /// error here: GraphQL servers put useful detail in the body, so the caller decides what the status means.
    pub(crate) async fn post(&self, endpoint: &Endpoint, payload: &serde_json::Value) -> Result<Reply, String> {
        let reply = self.send(endpoint, payload, false).await?;
        if reply.status == UNAUTHORIZED && endpoint.headers.iter().any(Header::is_refreshable) {
            return self.send(endpoint, payload, true).await;
        }
        Ok(reply)
    }

    /// Sends one request. With `fresh_headers`, header commands run again instead of reusing their last output.
    async fn send(
        &self,
        endpoint: &Endpoint,
        payload: &serde_json::Value,
        fresh_headers: bool,
    ) -> Result<Reply, String> {
        // Resolve every header before anything is sent, so a missing value never produces a half-authenticated
        // request.
        let mut headers = Vec::with_capacity(endpoint.headers.len());
        for header in &endpoint.headers {
            headers.push(header.resolve(fresh_headers).await?);
        }
        let mut request = self
            .http
            .post(&endpoint.url)
            .header("content-type", "application/json")
            .header("accept", "application/graphql-response+json, application/json");
        let mut secrets = Vec::new();
        for header in headers {
            request = request.header(header.name, header.value);
            secrets.extend(header.secret);
        }
        let mut response = request
            .body(payload.to_string())
            .timeout(endpoint.timeout)
            .send()
            .await
            .map_err(|error| {
                if error.is_timeout() {
                    timed_out(endpoint)
                } else {
                    format!("The request to {} failed: {}", endpoint.url, explain(&error))
                }
            })?;
        let status = response.status().as_u16();
        if response.status().is_redirection() {
            let target = response
                .headers()
                .get("location")
                .and_then(|location| location.to_str().ok())
                .unwrap_or("another URL");
            return Err(format!(
                "{} answered HTTP {status}, a redirect to {target}. fieldtrip does not follow redirects, because the \
                 headers configured for this endpoint would go to a host the user did not name. The user has to put \
                 the final URL in the fieldtrip config.",
                endpoint.url
            ));
        }

        let mut bytes = Vec::new();
        let mut cut_short = false;
        loop {
            let chunk = response.chunk().await.map_err(|error| {
                if error.is_timeout() {
                    timed_out(endpoint)
                } else {
                    format!(
                        "The response from {} could not be read: {}",
                        endpoint.url,
                        explain(&error)
                    )
                }
            })?;
            let Some(chunk) = chunk else { break };
            let room = MAX_BODY_BYTES - bytes.len();
            if chunk.len() > room {
                bytes.extend_from_slice(chunk.get(..room).unwrap_or_default());
                cut_short = true;
                break;
            }
            bytes.extend_from_slice(&chunk);
        }
        let body = String::from_utf8_lossy(&bytes).into_owned();
        // A server can echo a credential back, in an error message for instance. It must not reach the caller.
        let body = secrets
            .iter()
            .fold(body, |body, secret| secret.scrub(&body).unwrap_or(body));
        Ok(Reply {
            status,
            body,
            cut_short,
        })
    }
}

/// The error for an endpoint that took longer than its `timeout`.
fn timed_out(endpoint: &Endpoint) -> String {
    format!(
        "{} did not answer within {} seconds, the `timeout` for this endpoint. Nothing is known about whether the \
         operation ran. For a query, ask for less. For a mutation, check its effect before sending it again.",
        endpoint.url,
        endpoint.timeout.as_secs()
    )
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
