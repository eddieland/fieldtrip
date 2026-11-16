//! The HTTP client that talks to GraphQL endpoints.

use std::error::Error as _;
use std::fmt::{self, Write as _};
use std::time::{Duration, Instant};

use crate::config::Endpoint;
use crate::headers::{Generated, Header, Secret};

/// The `User-Agent` sent with every request.
const USER_AGENT: &str = concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION"));

/// The status an endpoint answers with when it does not accept the credentials.
const UNAUTHORIZED: u16 = 401;

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
    /// The response body as text, up to the limit the caller gave.
    pub(crate) body: String,
    /// Whether the body was longer than the limit, so that only its start was read.
    pub(crate) cut_short: bool,
    /// How long the request took, from sending it to reading the last of the body.
    pub(crate) elapsed: Duration,
    /// The endpoint's configured `response_headers` that came back, by their configured name, in config order.
    /// Values have had secrets removed.
    pub(crate) headers: Vec<(String, String)>,
    /// When this is a retry after a 401 that was refused with a 401 too, a note that says both happened and which
    /// header commands ran again.
    pub(crate) refused_twice: Option<String>,
    /// The values fieldtrip generated for this request's `generate` headers, in config order.
    pub(crate) sent: Vec<Generated>,
}

/// A request that got no answer, or none that could be read.
#[derive(Debug)]
pub(crate) struct Failed {
    /// What went wrong, for the calling agent.
    pub(crate) message: String,
    /// The values generated for the request's `generate` headers, when it was sent. Empty when it never left, for
    /// instance because a header could not be resolved.
    pub(crate) sent: Vec<Generated>,
}

impl fmt::Display for Failed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)?;
        if !self.sent.is_empty() {
            let ids = self
                .sent
                .iter()
                .map(|made| format!("{} {}", made.name, made.value))
                .collect::<Vec<_>>()
                .join(", ");
            write!(
                f,
                "\nThe request was sent with {ids}. Even without an answer, the endpoint's logs may hold it under that \
                 ID, so pass it on to whoever can read them."
            )?;
        }
        Ok(())
    }
}

impl From<Failed> for String {
    fn from(failed: Failed) -> Self {
        failed.to_string()
    }
}

/// The most characters of one response header's value that are shown.
const MAX_HEADER_VALUE: usize = 200;

impl Reply {
    /// One line with the status, how long the request took, the generated request headers and the chosen response
    /// headers: `HTTP 200 · 812 ms · sent X-Correlation-Id 1b4e… · x-request-id abc123`.
    pub(crate) fn metadata(&self) -> String {
        let mut line = format!("HTTP {} · {} ms", self.status, self.elapsed.as_millis());
        for made in &self.sent {
            let _ = write!(line, " · sent {} {}", made.name, made.value);
        }
        for (name, value) in &self.headers {
            let _ = write!(line, " · {name} {value}");
        }
        line
    }
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
    /// At most `limit` bytes of the body are read. The rest is dropped, so a huge response cannot exhaust memory.
    ///
    /// When the endpoint answers 401 and one of its headers comes from a command, the command runs again and the
    /// request is retried once: the usual cause is a token that expired while its output was being reused. The
    /// retry carries the same generated values, so both attempts share an ID in the endpoint's logs.
    ///
    /// # Errors
    ///
    /// Returns what went wrong when a header cannot be resolved or no response arrives, with the generated values
    /// the request carried. An HTTP error status is not an error here: GraphQL servers put useful detail in the
    /// body, so the caller decides what the status means.
    pub(crate) async fn post(
        &self,
        endpoint: &Endpoint,
        payload: &serde_json::Value,
        limit: usize,
    ) -> Result<Reply, Failed> {
        let mut generated = Vec::new();
        for header in &endpoint.headers {
            let made = header.generate().map_err(|message| Failed {
                message,
                sent: Vec::new(),
            })?;
            generated.extend(made);
        }
        let reply = self.send(endpoint, payload, limit, false, &generated).await?;
        let refreshable: Vec<&Header> = endpoint
            .headers
            .iter()
            .map(AsRef::as_ref)
            .filter(|header: &&Header| header.is_refreshable())
            .collect();
        if reply.status != UNAUTHORIZED || refreshable.is_empty() {
            return Ok(reply);
        }
        let commands = refreshable
            .iter()
            .map(|header| format!("`{}` (`{}`)", header.name, header.program().unwrap_or_default()))
            .collect::<Vec<_>>()
            .join(", ");
        let plural = if refreshable.len() == 1 { "" } else { "s" };
        let mut retry = self
            .send(endpoint, payload, limit, true, &generated)
            .await
            .map_err(|failed| Failed {
                message: format!(
                    "{} answered HTTP {UNAUTHORIZED}, so fieldtrip ran the command{plural} for the header{plural} \
                     {commands} again to get a fresh credential, and that failed. {}",
                    endpoint.shown_url, failed.message
                ),
                // The first request was sent, whether or not the retry was.
                sent: generated.clone(),
            })?;
        if retry.status == UNAUTHORIZED {
            for header in &refreshable {
                header.refused(UNAUTHORIZED);
            }
            retry.refused_twice = Some(format!(
                "{} answered HTTP {UNAUTHORIZED} twice. After the first, fieldtrip ran the command{plural} for the \
                 header{plural} {commands} again and retried with what {} printed, which was refused too. The login \
                 behind it has most likely expired or been revoked: the user has to sign in again, then retry. A \
                 missing permission is usually HTTP 403 instead.",
                endpoint.shown_url,
                if refreshable.len() == 1 { "it" } else { "they" }
            ));
        }
        Ok(retry)
    }

    /// Sends one request. With `fresh_headers`, header commands run again instead of reusing their last output.
    /// `generated` holds the values of the `generate` headers.
    async fn send(
        &self,
        endpoint: &Endpoint,
        payload: &serde_json::Value,
        limit: usize,
        fresh_headers: bool,
        generated: &[Generated],
    ) -> Result<Reply, Failed> {
        // Resolve every header before anything is sent, so a missing value never produces a half-authenticated
        // request.
        let mut headers = Vec::with_capacity(endpoint.headers.len());
        for header in &endpoint.headers {
            let resolved = header
                .resolve(fresh_headers, generated)
                .await
                .map_err(|message| Failed {
                    message,
                    sent: Vec::new(),
                })?;
            headers.push(resolved);
        }
        // From here on the request may have reached the endpoint, so a failure carries what it was sent with.
        let failed = |message: String| Failed {
            message,
            sent: generated.to_vec(),
        };
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
        let started = Instant::now();
        let mut response = request
            .body(payload.to_string())
            .timeout(endpoint.timeout)
            .send()
            .await
            .map_err(|error| {
                if error.is_timeout() {
                    timed_out(endpoint, started)
                } else {
                    format!(
                        "The request to {} failed after {} ms: {}",
                        endpoint.shown_url,
                        started.elapsed().as_millis(),
                        explain(&error.without_url())
                    )
                }
            })
            .map_err(failed)?;
        let status = response.status().as_u16();
        if response.status().is_redirection() {
            let target = response
                .headers()
                .get("location")
                .and_then(|location| location.to_str().ok())
                .unwrap_or("another URL");
            return Err(failed(format!(
                "{} answered HTTP {status}, a redirect to {target}. fieldtrip does not follow redirects, because the \
                 headers configured for this endpoint would go to a host the user did not name. The user has to put \
                 the final URL in the fieldtrip config.",
                endpoint.shown_url
            )));
        }

        let mut bytes = Vec::new();
        let mut cut_short = false;
        loop {
            let chunk = response.chunk().await.map_err(|error| {
                if error.is_timeout() {
                    timed_out(endpoint, started)
                } else {
                    format!(
                        "The response from {} could not be read after {} ms: {}",
                        endpoint.shown_url,
                        started.elapsed().as_millis(),
                        explain(&error.without_url())
                    )
                }
            });
            let Some(chunk) = chunk.map_err(failed)? else { break };
            let room = limit - bytes.len();
            if chunk.len() > room {
                bytes.extend_from_slice(chunk.get(..room).unwrap_or_default());
                cut_short = true;
                break;
            }
            bytes.extend_from_slice(&chunk);
        }
        let elapsed = started.elapsed();
        let headers = chosen_headers(endpoint, response.headers(), &secrets);
        // A server can echo a credential back, in an error message for instance. It must not reach the caller.
        let body = scrubbed(String::from_utf8_lossy(&bytes).into_owned(), &secrets);
        Ok(Reply {
            status,
            body,
            cut_short,
            elapsed,
            headers,
            refused_twice: None,
            sent: generated.to_vec(),
        })
    }
}

/// `text` with every secret in `secrets` replaced.
fn scrubbed(text: String, secrets: &[Secret]) -> String {
    secrets
        .iter()
        .fold(text, |text, secret| secret.scrub(&text).unwrap_or(text))
}

/// The response headers the user chose to show for `endpoint`, with secrets removed and long values cut. A header
/// that came more than once is shown once, its values joined with `, `.
fn chosen_headers(
    endpoint: &Endpoint,
    received: &reqwest::header::HeaderMap,
    secrets: &[Secret],
) -> Vec<(String, String)> {
    endpoint
        .response_headers
        .iter()
        .filter_map(|name| {
            let values: Vec<String> = received
                .get_all(name.as_str())
                .iter()
                .map(|value| String::from_utf8_lossy(value.as_bytes()).into_owned())
                .collect();
            if values.is_empty() {
                return None;
            }
            let value = scrubbed(values.join(", "), secrets);
            let value = if value.chars().count() > MAX_HEADER_VALUE {
                let mut cut: String = value.chars().take(MAX_HEADER_VALUE).collect();
                cut.push('…');
                cut
            } else {
                value
            };
            Some((name.clone(), value))
        })
        .collect()
}

/// The error for an endpoint that took longer than its `timeout`.
fn timed_out(endpoint: &Endpoint, started: Instant) -> String {
    format!(
        "{} did not answer within {} seconds, the `timeout` for this endpoint. fieldtrip waited {} ms. Nothing is \
         known about whether the operation ran. For a query, ask for less. For a mutation, check its effect before \
         sending it again.",
        endpoint.shown_url,
        endpoint.timeout.as_secs(),
        started.elapsed().as_millis()
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
