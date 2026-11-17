//! `paginate`: following the pages of one connection in a response.
//!
//! A connection is an object that holds a `pageInfo` with `hasNextPage` and `endCursor`, as the Relay convention has
//! it. `execute` sends the query, reads the `pageInfo` at the path the calling agent gave, and sends the query again
//! with the cursor in a variable, until the last page or a cap.
//!
//! Many APIs page by other names: a `nextToken` that is null on the last page, or a `cursor` beside a `hasMore`. The
//! calling agent can name the field that holds the cursor and the one that says whether more pages follow, each
//! relative to the object at the path. Paging by offset or page number has no cursor, and is not followed.

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
    /// Where the cursor is, relative to the connection: `pageInfo.endCursor` unless the calling agent named a field.
    cursor: Vec<String>,
    /// Where the flag that says another page follows is, relative to the connection. `None` when the calling agent
    /// named a cursor and no flag: then there is another page for as long as the cursor is not null.
    more: Option<Vec<String>>,
    /// Whether the calling agent named the cursor, so the connection is not read as Relay's.
    named: bool,
}

/// What a page says about the one after it.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Next {
    /// There is another page, after this cursor, which is a string or a number.
    More(Value),
    /// This was the last page.
    Last,
    /// The response has no `pageInfo` at the path.
    Missing,
    /// The `pageInfo` has no `hasNextPage`, or says there is another page and has no `endCursor`.
    Unselected,
    /// The connection has no value at the cursor the calling agent named.
    NoCursor,
    /// The connection has no true or false at the flag the calling agent named.
    NoFlag,
}

/// Whether `name` is a GraphQL name.
fn is_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with(|first: char| first.is_ascii_digit())
        && name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '_')
}

/// Reads `written`, the `paginate` key called `key`, as names joined by `.`.
fn field_path(key: &str, written: &str) -> Result<Vec<String>, String> {
    let names: Vec<String> = written.trim().split('.').map(str::to_owned).collect();
    if names.iter().all(|name| is_name(name)) {
        return Ok(names);
    }
    Err(format!(
        "`paginate.{key}` is `{written}`, which is not the name of a field. Give the field's name as it is in the \
         response, relative to the object at `paginate.path`, such as `nextToken`, or names joined by `.` for a \
         field further in, such as `paging.next`. Nothing was sent."
    ))
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
        fields: (Option<&str>, Option<&str>),
        variable: Option<&str>,
        max_pages: Option<usize>,
        endpoint: &str,
        cap: usize,
    ) -> Result<Self, String> {
        let path = path.trim();
        let (cursor, more) = fields;
        let named = cursor.is_some();
        let more = match (more, named) {
            (Some(more), _) => Some(field_path("more", more)?),
            (None, true) => None,
            (None, false) => Some(vec!["pageInfo".to_owned(), "hasNextPage".to_owned()]),
        };
        let cursor = match cursor {
            Some(cursor) => field_path("cursor", cursor)?,
            None => vec!["pageInfo".to_owned(), "endCursor".to_owned()],
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
            cursor,
            more,
            named,
        })
    }

    /// The field the cursor comes from, as it is shown: `endCursor`, or the field the calling agent named.
    pub(crate) fn cursor_name(&self) -> String {
        if self.named {
            self.cursor.join(".")
        } else {
            "endCursor".to_owned()
        }
    }

    /// The flag that says another page follows, as it is shown, when there is one.
    pub(crate) fn more_name(&self) -> Option<String> {
        let more = self.more.as_ref()?;
        Some(if self.named {
            more.join(".")
        } else {
            "hasNextPage".to_owned()
        })
    }

    /// Whether the calling agent named the cursor, so the connection is not read as Relay's.
    pub(crate) const fn is_named(&self) -> bool {
        self.named
    }

    /// What the response `body` says about the page after it.
    pub(crate) fn next(&self, body: &str) -> Next {
        let Ok(response) = serde_json::from_str::<Value>(body) else {
            return Next::Missing;
        };
        let at = |from: &Value, keys: &[String]| -> Option<Value> {
            keys.iter()
                .try_fold(from, |value, key| value.as_object()?.get(key))
                .cloned()
        };
        let Some(connection) = at(&response, &self.keys).filter(Value::is_object) else {
            return Next::Missing;
        };
        if !self.named && !connection.get("pageInfo").is_some_and(Value::is_object) {
            return Next::Missing;
        }
        let (no_flag, no_cursor) = if self.named {
            (Next::NoFlag, Next::NoCursor)
        } else {
            (Next::Unselected, Next::Unselected)
        };
        if let Some(more) = &self.more {
            match at(&connection, more) {
                Some(Value::Bool(false)) => return Next::Last,
                Some(Value::Bool(true)) => {}
                _ => return no_flag,
            }
        }
        match at(&connection, &self.cursor) {
            Some(cursor @ (Value::String(_) | Value::Number(_))) => Next::More(cursor),
            // With no flag, a null cursor is how the endpoint says that this was the last page.
            Some(Value::Null) if self.more.is_none() => Next::Last,
            _ => no_cursor,
        }
    }
}
