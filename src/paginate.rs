//! `paginate`: following the pages of one connection in a response.
//!
//! A connection is an object that holds a `pageInfo` with `hasNextPage` and `endCursor`, as the Relay convention has
//! it. `execute` sends the query, reads the `pageInfo` at the path the calling agent gave, and sends the query again
//! with the cursor in a variable, until the last page or a cap.

use serde_json::Value;

/// How many pages are followed when the calling agent does not say.
const DEFAULT_PAGES: usize = 5;

/// The variable that carries the cursor when the calling agent does not name one.
const DEFAULT_VARIABLE: &str = "after";

/// Which connection to follow, and how far.
#[derive(Debug)]
pub(crate) struct Follow {
    /// The path to the connection, as the calling agent wrote it, such as `data.repository.issues`.
    pub(crate) path: String,
    /// The names in the path, `data` included.
    keys: Vec<String>,
    /// The variable of the operation that carries the cursor, without its `$`.
    pub(crate) variable: String,
    /// The most pages to send for.
    pub(crate) max_pages: usize,
}

/// What a page says about the one after it.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Next {
    /// There is another page, after this cursor.
    More(String),
    /// This was the last page.
    Last,
    /// The response has no `pageInfo` at the path.
    Missing,
    /// The `pageInfo` has no `hasNextPage`, or says there is another page and has no `endCursor`.
    Unselected,
}

impl Follow {
    /// Reads the `paginate` argument of a call on the endpoint called `endpoint`, whose `max_batch` is `cap`.
    ///
    /// # Errors
    ///
    /// Returns a message that says how the argument is written when the path is not a chain of names from `data`,
    /// the variable is not a name, or `max_pages` is zero or over the cap.
    pub(crate) fn parse(
        path: &str,
        variable: Option<&str>,
        max_pages: Option<usize>,
        endpoint: &str,
        cap: usize,
    ) -> Result<Self, String> {
        let path = path.trim();
        let is_name = |name: &str| {
            !name.is_empty()
                && !name.starts_with(|first: char| first.is_ascii_digit())
                && name
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric() || character == '_')
        };
        let keys: Vec<String> = path.split('.').map(str::to_owned).collect();
        if keys.len() < 2 || keys.first().map(String::as_str) != Some("data") || !keys.iter().all(|key| is_name(key)) {
            return Err(format!(
                "`paginate.path` is `{path}`, which is not the path to a connection. Write the path to the one \
                 object that holds `pageInfo`: it starts with `data`, followed by names joined by `.`, such as \
                 `data.repository.issues`. A connection inside a list cannot be followed. Nothing was sent."
            ));
        }
        let variable = variable.map_or(DEFAULT_VARIABLE, |variable| variable.trim().trim_start_matches('$'));
        if !is_name(variable) {
            return Err(format!(
                "`paginate.variable` is `{variable}`, which is not the name of a variable. Give the name of the \
                 variable the query passes as the connection's cursor, such as `after`. Nothing was sent."
            ));
        }
        let max_pages = max_pages.unwrap_or_else(|| DEFAULT_PAGES.min(cap));
        if max_pages == 0 {
            return Err("`paginate.max_pages` must be at least 1. Nothing was sent.".to_owned());
        }
        if max_pages > cap {
            return Err(format!(
                "`paginate.max_pages` is {max_pages}, and one call on `{endpoint}` sends at most {cap} (`max_batch` \
                 in the user's config). Nothing was sent. Ask for fewer pages, then call again from the last \
                 `endCursor`."
            ));
        }
        Ok(Self {
            path: path.to_owned(),
            keys,
            variable: variable.to_owned(),
            max_pages,
        })
    }

    /// What the response `body` says about the page after it.
    pub(crate) fn next(&self, body: &str) -> Next {
        let Ok(response) = serde_json::from_str::<Value>(body) else {
            return Next::Missing;
        };
        let connection = self
            .keys
            .iter()
            .try_fold(&response, |value, key| value.as_object()?.get(key));
        let Some(Value::Object(info)) = connection.and_then(|connection| connection.get("pageInfo")) else {
            return Next::Missing;
        };
        match (info.get("hasNextPage"), info.get("endCursor")) {
            (Some(Value::Bool(false)), _) => Next::Last,
            (Some(Value::Bool(true)), Some(Value::String(cursor))) => Next::More(cursor.clone()),
            _ => Next::Unselected,
        }
    }
}
