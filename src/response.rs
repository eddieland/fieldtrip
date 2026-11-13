//! What `execute` shows of a response when the parts after `errors` are too long for a result: a summary in which
//! each part is still valid JSON.
//!
//! `errors` is never cut, and is shown before this. The parts that follow it, `data` first, are cut by structure,
//! not by text: long lists keep their first items and long strings their first characters, and each cut is marked
//! with how much it dropped, so the calling agent knows exactly what is missing.

use std::fmt::Write as _;

use serde_json::{Map, Value};

/// The string lengths a summary tries, longest first. Shorter strings leave room for more list items.
const STRING_CAPS: [usize; 3] = [2_000, 500, 100];

/// A summary with fewer list items than this tries shorter strings first, unless no list is this long.
const FEW_ITEMS: usize = 5;

/// A response cut down to fit, and what was cut.
#[derive(Debug)]
pub(crate) struct Summary {
    /// One line per part, `\nname: JSON`, in the order given.
    pub(crate) lines: String,
    /// How many items each cut list kept, when any list was cut.
    pub(crate) items: Option<usize>,
    /// How many characters each cut string kept, when any string was cut.
    pub(crate) chars: Option<usize>,
}

/// What one pass of cutting did.
#[derive(Debug, Default)]
struct Cuts {
    lists: bool,
    strings: bool,
}

/// The longest string and list in `value`, in characters and items.
fn longest(value: &Value) -> (usize, usize) {
    match value {
        Value::String(text) => (text.chars().count(), 0),
        Value::Array(items) => items
            .iter()
            .map(longest)
            .fold((0, items.len()), |(text, list), (inner_text, inner_list)| {
                (text.max(inner_text), list.max(inner_list))
            }),
        Value::Object(map) => map
            .values()
            .map(longest)
            .fold((0, 0), |(text, list), (inner_text, inner_list)| {
                (text.max(inner_text), list.max(inner_list))
            }),
        Value::Null | Value::Bool(_) | Value::Number(_) => (0, 0),
    }
}

/// `value` with each list cut to `items` and each string to `chars`, and a marker where something was dropped.
fn cut(value: &Value, items: usize, chars: usize, cuts: &mut Cuts) -> Value {
    match value {
        Value::String(text) => {
            let length = text.chars().count();
            if length <= chars {
                return value.clone();
            }
            cuts.strings = true;
            let mut kept: String = text.chars().take(chars).collect();
            let _ = write!(kept, "…(+{} more characters)", length - chars);
            Value::String(kept)
        }
        Value::Array(list) => {
            let mut kept: Vec<Value> = list
                .iter()
                .take(items)
                .map(|item| cut(item, items, chars, cuts))
                .collect();
            if list.len() > items {
                cuts.lists = true;
                let dropped = list.len() - items;
                let noun = if dropped == 1 { "item" } else { "items" };
                kept.push(Value::String(format!("…(+{dropped} more {noun})")));
            }
            Value::Array(kept)
        }
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(key, inner)| (key.clone(), cut(inner, items, chars, cuts)))
                .collect(),
        ),
        Value::Null | Value::Bool(_) | Value::Number(_) => value.clone(),
    }
}

/// `parts`, each cut to `items` and `chars`, as one line per part. Returns the lines and what was cut.
fn attempt(parts: &Map<String, Value>, items: usize, chars: usize) -> (String, Cuts) {
    let mut cuts = Cuts::default();
    let mut lines = String::new();
    for (name, value) in parts {
        let _ = write!(lines, "\n{name}: {}", cut(value, items, chars, &mut cuts));
    }
    (lines, cuts)
}

/// Cuts `parts` to fit in `budget` characters, keeping as many list items as fit.
///
/// Returns `None` when even the smallest summary does not fit, as when an object has thousands of keys.
pub(crate) fn summarize(parts: &Map<String, Value>, budget: usize) -> Option<Summary> {
    let whole = Value::Object(parts.clone());
    let (longest_string, longest_list) = longest(&whole);
    let enough = FEW_ITEMS.min(longest_list);
    let fits = |lines: &String| lines.chars().count() <= budget;

    let mut fallback: Option<(usize, Summary)> = None;
    for chars in STRING_CAPS {
        let chars = chars.min(longest_string);
        let (lines, cuts) = attempt(parts, 0, chars);
        if !fits(&lines) {
            continue;
        }
        // The size grows with the number of items kept, so the most that fit is found by bisection.
        let mut best = (0, lines, cuts);
        let (mut low, mut high) = (1, longest_list);
        while low <= high {
            let middle = low + (high - low) / 2;
            let (lines, cuts) = attempt(parts, middle, chars);
            if fits(&lines) {
                best = (middle, lines, cuts);
                low = middle + 1;
            } else {
                high = middle - 1;
            }
        }
        let (items, lines, cuts) = best;
        let summary = Summary {
            lines,
            items: cuts.lists.then_some(items),
            chars: cuts.strings.then_some(chars),
        };
        if items >= enough {
            return Some(summary);
        }
        // Too few items: shorter strings may make room for more. Keep the one with the most so far.
        if fallback.as_ref().is_none_or(|(most, _)| items > *most) {
            fallback = Some((items, summary));
        }
    }
    fallback.map(|(_, summary)| summary)
}

/// How much of a response a result shows: all it holds, or a preview of a response that was saved to a file.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Room {
    /// A result holds at most this many characters, the endpoint's `max_output_chars`.
    Result(usize),
    /// The response was saved to a file, and the result shows at most this many characters of it.
    Preview(usize),
}

impl Room {
    /// The most characters the result holds.
    pub(crate) const fn limit(self) -> usize {
        match self {
            Self::Result(limit) | Self::Preview(limit) => limit,
        }
    }
}

/// The note that follows a summary: what was cut, and how to get the rest. `length` is how long the result would
/// have been.
pub(crate) fn note(summary: &Summary, length: usize, room: Room) -> String {
    let (why, advice) = match room {
        Room::Result(limit) => (
            format!("more than the {limit} it holds here (`max_output_chars`)"),
            "Ask for less with fewer fields or a smaller page, or pick one part with `select`.",
        ),
        Room::Preview(limit) => (
            format!("more than the {limit} shown of a saved response"),
            "The file has all of it.",
        ),
    };
    let mut cuts = Vec::new();
    if let Some(items) = summary.items {
        let noun = if items == 1 { "item" } else { "items" };
        cuts.push(format!("lists to their first {items} {noun}"));
    }
    if let Some(chars) = summary.chars {
        cuts.push(format!("strings to {chars} characters"));
    }
    let why = format!("[The result would have {length} characters, {why}");
    if cuts.is_empty() {
        return format!("{why}, so each part is shown without its whitespace. Nothing was dropped.]");
    }
    format!(
        "{why}, so this is a summary. `errors` is whole. Each part after it is still valid JSON, cut by structure: \
         {}, each marked with what was dropped, such as \"…(+412 more items)\". {advice}]",
        cuts.join(" and ")
    )
}

/// The most connections a result points out as having more pages.
const MAX_PAGE_HINTS: usize = 5;

/// A connection in the response, wherever its path matched: how often it appeared, and how often with more pages.
#[derive(Debug)]
struct Connection {
    /// The path, with `[*]` for any item of a list.
    path: String,
    /// How many times the path held a `pageInfo`.
    seen: usize,
    /// How many of those said `hasNextPage: true`.
    more: usize,
    /// The `endCursor` of the first that had more pages.
    cursor: Option<String>,
}

/// Finds every object under `value` with a `pageInfo` in it, and notes whether it has more pages.
fn connections(value: &Value, path: &str, found: &mut Vec<Connection>) {
    match value {
        Value::Object(map) => {
            if let Some(Value::Object(info)) = map.get("pageInfo") {
                let more = info.get("hasNextPage") == Some(&Value::Bool(true));
                let index = found
                    .iter()
                    .position(|connection| connection.path == path)
                    .unwrap_or_else(|| {
                        found.push(Connection {
                            path: path.to_owned(),
                            seen: 0,
                            more: 0,
                            cursor: None,
                        });
                        found.len() - 1
                    });
                if let Some(connection) = found.get_mut(index) {
                    connection.seen += 1;
                    if more {
                        connection.more += 1;
                        if connection.cursor.is_none() {
                            connection.cursor = info.get("endCursor").and_then(Value::as_str).map(str::to_owned);
                        }
                    }
                }
            }
            for (key, inner) in map {
                connections(inner, &format!("{path}.{key}"), found);
            }
        }
        Value::Array(items) => {
            let path = format!("{path}[*]");
            for item in items {
                connections(item, &path, found);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

/// One line for each connection in a response's `data` that says it has more pages, so the calling agent pages
/// through it instead of asking for a bigger page.
pub(crate) fn page_hints(data: &Value) -> Vec<String> {
    let mut found = Vec::new();
    connections(data, "data", &mut found);
    let with_more: Vec<&Connection> = found.iter().filter(|connection| connection.more > 0).collect();
    let mut hints: Vec<String> = with_more
        .iter()
        .take(MAX_PAGE_HINTS)
        .map(|connection| {
            let path = &connection.path;
            if connection.seen == 1 {
                let cursor = connection.cursor.as_ref().map_or_else(String::new, |cursor| {
                    format!(" Its `endCursor` is {}.", Value::from(cursor.as_str()))
                });
                format!(
                    "More pages: `{path}` has `hasNextPage: true`.{cursor} To get the rest, pass the cursor as \
                     `after` with the same page size, rather than raising `first`."
                )
            } else {
                format!(
                    "More pages: `{path}` has `hasNextPage: true` in {} of its {} appearances. Page through each \
                     with `after` and its `endCursor`, rather than raising `first`.",
                    connection.more, connection.seen
                )
            }
        })
        .collect();
    if with_more.len() > MAX_PAGE_HINTS {
        hints.push(format!(
            "More pages: {} more connections have `hasNextPage: true`.",
            with_more.len() - MAX_PAGE_HINTS
        ));
    }
    hints
}
