//! What `execute` shows of a response when the parts after `errors` are too long for a result: a summary in which
//! each part is still valid JSON.
//!
//! `errors` is never cut, and is shown before this. The parts that follow it, `data` first, are cut by structure,
//! not by text: long lists keep their first items and long strings their first characters, and each cut is marked
//! with how much it dropped, so the calling agent knows exactly what is missing.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use serde_json::value::RawValue;
use serde_json::{Map, Value};

use crate::config::{Endpoint, show_bytes};
use crate::http::Reply;
use crate::results::Stored;
use crate::select::{self, Selected};
use crate::shape;
use crate::text::{capped, capped_after, excerpt};

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

/// How much of a response a result shows: all it holds, or less of a response that was written to a file.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Room {
    /// A result holds at most this many characters, the endpoint's `max_output_chars`.
    Result(usize),
    /// The response was written to a file, and the result shows at most this many characters of it.
    Preview(usize),
    /// The response was written to a file, and the result shows the part `select` picked, up to this many
    /// characters, the endpoint's `max_inline_chars`.
    Selection(usize),
    /// `read_result` shows the part of a file that `select` picked, up to this many characters, `max_output_chars`.
    Read(usize),
    /// The result holds several responses, and each gets at most this many characters of it.
    Share(usize),
}

impl Room {
    /// The most characters the result holds.
    pub(crate) const fn limit(self) -> usize {
        match self {
            Self::Result(limit)
            | Self::Preview(limit)
            | Self::Selection(limit)
            | Self::Read(limit)
            | Self::Share(limit) => limit,
        }
    }

    /// Whether the whole response is in a file, so that what is shown is never all there is.
    pub(crate) const fn is_of_a_file(self) -> bool {
        matches!(self, Self::Preview(_) | Self::Selection(_) | Self::Read(_))
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
        Room::Share(limit) => (
            format!("more than the {limit} that each response gets of a result that holds several"),
            "Ask for less with fewer fields or a smaller page, pick one part with `select`, or send fewer at once.",
        ),
        Room::Preview(limit) => (
            format!("more than the {limit} shown of a saved response"),
            "The file has all of it, and `read_result` returns any part of it.",
        ),
        Room::Selection(limit) => (
            format!("more than the {limit} shown of a selection (`max_inline_chars`)"),
            "The file has all of it. `read_result` shows more of a selection, and a longer `select` path picks a \
             smaller part.",
        ),
        Room::Read(limit) => (
            format!("more than the {limit} it holds here (`max_output_chars`)"),
            "The file has all of it. Pick a smaller part with a longer `select` path, such as one item with `[0]`.",
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

/// The most characters shown of a response that was saved to a file.
pub(crate) const SAVED_PREVIEW: usize = 4_000;

/// Room kept in a result for the note that follows a summary.
pub(crate) const NOTE_ROOM: usize = 500;

/// The least room the preview of a saved response gets, however long the `errors` before it are: this much, or a
/// quarter of what the result may hold when that is less.
const MIN_PREVIEW: usize = 1_000;

/// The most paths shown of a response's shape.
const SHAPE_PATHS: usize = 40;

/// The most characters of one line of a response's shape.
const SHAPE_LINE: usize = 200;

/// The shape of `body`, a response that was written to a file, in at most `budget` characters: a heading, then one
/// line per path. Empty when the body is not JSON.
fn shape_of(body: &str, budget: usize) -> String {
    let Ok(response) = serde_json::from_str::<Value>(body) else {
        return String::new();
    };
    let lines = shape::lines(&response);
    let mut out = "\nShape of the whole response, one line per path. `nodes[50]` is a list of 50 items, and `select` \
                   takes one with `nodes[0]` or all with `nodes[*]`:"
        .to_owned();
    let mut room = budget.saturating_sub(out.chars().count() + 40);
    let mut shown = 0;
    for line in lines.iter().take(SHAPE_PATHS) {
        let mut line: String = line.chars().take(SHAPE_LINE).collect();
        if line.chars().count() == SHAPE_LINE {
            line.push('…');
        }
        let length = line.chars().count() + 1;
        if length > room {
            break;
        }
        room -= length;
        let _ = write!(out, "\n{line}");
        shown += 1;
    }
    match lines.len() - shown {
        0 => {}
        1 => out.push_str("\n… and 1 more path."),
        more => {
            let _ = write!(out, "\n… and {more} more paths.");
        }
    }
    out
}

/// The result of `execute`: `head`, which ends with the errors, then each of the `rest` of the response's parts as
/// the server wrote it when they fit in the `room`, or a summary of them that is still valid JSON when not. `shape`
/// follows a summary of a response that was written to a file.
fn shown(head: String, rest: &[(String, Box<RawValue>)], room: Room, shape: &str) -> String {
    let limit = room.limit();
    let mut written = String::new();
    for (name, value) in rest {
        let _ = write!(written, "\n{name}: {}", value.get().trim());
    }
    let length = head.chars().count() + written.chars().count();
    if length <= limit {
        return head + &written;
    }
    let parts: Map<String, Value> = rest
        .iter()
        .filter_map(|(name, value)| Some((name.clone(), serde_json::from_str(value.get()).ok()?)))
        .collect();
    let mut space = limit.saturating_sub(head.chars().count() + NOTE_ROOM + shape.chars().count());
    if room.is_of_a_file() {
        // Errors are never cut, and can use the room up. The preview is all the result shows of the rest.
        space = space.max(MIN_PREVIEW.min(limit / 4));
    }
    match summarize(&parts, space) {
        Some(summary) => {
            let note = note(&summary, length, room);
            format!("{head}{}\n{note}{shape}", summary.lines)
        }
        None if room.is_of_a_file() => {
            let limit = limit.saturating_sub(shape.chars().count());
            capped_after(head, &written, limit, "The file has all of it.") + shape
        }
        None => capped_after(head, &written, limit, ASK_FOR_LESS),
    }
}

/// What a result shows of the GraphQL response `body`, whose top-level members are `parts`.
///
/// `summary` comes first, then a line for each connection with more pages, then `saved_note`. Errors follow, whole,
/// because they say what went wrong. After them comes the part `select` picked, or `data` and the rest, cut to fit
/// the `room`, and the shape of the response when a preview of a file was cut.
pub(crate) fn laid_out(
    summary: String,
    saved_note: &str,
    body: &str,
    mut parts: BTreeMap<String, Box<RawValue>>,
    select: Option<&select::Path>,
    room: Room,
) -> String {
    let mut head = summary;
    let data = parts.get("data").and_then(|data| serde_json::from_str(data.get()).ok());
    for hint in data.as_ref().map_or_else(Vec::new, page_hints) {
        let _ = write!(head, "\n{hint}");
    }
    head.push_str(saved_note);
    let errors = parts.remove("errors");
    let rest = if let Some(path) = select {
        let picked = serde_json::from_str(body).map_or_else(|_error| Selected::nothing(), |whole| path.select(&whole));
        let _ = write!(head, "\n{}", picked.line);
        if let Some(errors) = errors.filter(|_| !path.is_in_errors()) {
            let _ = write!(head, "\nerrors: {}", errors.get().trim());
        }
        picked
            .value
            .and_then(|value| serde_json::value::to_raw_value(&value).ok())
            .map(|value| ("selected".to_owned(), value))
            .into_iter()
            .collect()
    } else {
        if let Some(errors) = errors {
            let _ = write!(head, "\nerrors: {}", errors.get().trim());
        }
        let data = parts.remove("data").map(|data| ("data".to_owned(), data));
        data.into_iter().chain(parts).collect::<Vec<_>>()
    };
    let shape = match room {
        Room::Preview(limit) => shape_of(body, limit / 3),
        Room::Result(_) | Room::Selection(_) | Room::Read(_) | Room::Share(_) => String::new(),
    };
    shown(head, &rest, room, &shape)
}

/// How `execute` says to get a smaller result.
const ASK_FOR_LESS: &str = "Ask for less: fewer fields, or a smaller page with `first` or `limit`.";

/// How much of a response that is not JSON to quote in an error.
pub(crate) const EXCERPT: usize = 300;

/// The result of `execute` for `reply`, the answer of `endpoint`.
///
/// `summary` is what was sent and how it went, and comes first. `written` is the response's length in characters
/// and what came of writing it to a file, when that was tried, and `inline` is the most characters of a response
/// that are shown. With `select` only that part of the response is shown.
///
/// # Errors
///
/// Returns the text as an error when the answer is not a GraphQL response, or was cut short on an HTTP error.
pub(crate) fn render(
    summary: String,
    reply: &Reply,
    endpoint: &Endpoint,
    written: Option<(usize, Result<Stored, String>)>,
    inline: usize,
    select: Option<&select::Path>,
    save: bool,
) -> Result<String, String> {
    if reply.cut_short {
        // Too long to read in full, so it cannot be parsed. The status says whether it is a result.
        let excerpt = excerpt(&reply.body, EXCERPT);
        let unsaved = if save {
            " Nothing was saved, because only part of the body was read."
        } else {
            ""
        };
        let text = format!(
            "{summary}\n[The response is larger than {}, the `max_response_bytes` limit for this endpoint, so \
             fieldtrip read only that much. A cut-off body is not valid JSON, so `data` and `errors` are both \
             lost. The operation did reach the server, so a mutation has run. Ask for less: fewer fields, or a \
             smaller page with `first` or `limit`, and page through the rest. The user can raise \
             `max_response_bytes`.{unsaved}]\n\
             It began: {excerpt}",
            show_bytes(endpoint.max_response_bytes)
        );
        return if reply.status < 400 { Ok(text) } else { Err(text) };
    }
    let (saved_note, room) = match written {
        Some((length, Ok(stored))) => {
            let why = if length > inline {
                format!(
                    "\nThe response has {} characters, more than the {} that are shown here \
                     (`max_inline_chars`), so it was saved instead.",
                    shape::grouped(length),
                    shape::grouped(inline)
                )
            } else {
                String::new()
            };
            let room = if select.is_some() {
                Room::Selection(inline)
            } else {
                Room::Preview(inline.min(SAVED_PREVIEW))
            };
            (
                format!(
                    "{why}\nSaved the whole response, {} bytes, to {}\n`read_result` with `result: \"{}\"` and a \
                     `select` path returns any part of it, and sends nothing.",
                    reply.body.len(),
                    stored.path.display(),
                    stored.id
                ),
                room,
            )
        }
        Some((_, Err(error))) => (
            format!("\nThe response was not saved: {error}."),
            Room::Result(endpoint.max_output_chars),
        ),
        None => (String::new(), Room::Result(endpoint.max_output_chars)),
    };
    let Ok(parts) = serde_json::from_str::<BTreeMap<String, Box<RawValue>>>(&reply.body) else {
        let excerpt = excerpt(&reply.body, EXCERPT);
        return Err(format!(
            "{summary}{saved_note}\nThe response was not a GraphQL response: {excerpt}"
        ));
    };
    if !parts.contains_key("errors") && !parts.contains_key("data") {
        return Err(capped(
            format!(
                "{summary}{saved_note}\nThe response has neither `data` nor `errors`, so it is not a GraphQL \
                 response:\n{}",
                reply.body.trim()
            ),
            room.limit(),
            if room.is_of_a_file() {
                "The file has all of it."
            } else {
                ASK_FOR_LESS
            },
        ));
    }

    Ok(laid_out(summary, &saved_note, &reply.body, parts, select, room))
}

/// One response of a call that sends several, under `label`, such as `[0]`: the status line, then `errors` in full,
/// then the rest or the part `select` picks, in `share` characters. Nothing is written to a file. A response that
/// is not GraphQL is shown as what is wrong with it.
pub(crate) fn section(
    label: &str,
    reply: &Reply,
    endpoint: &Endpoint,
    select: Option<&select::Path>,
    share: usize,
) -> String {
    let mut head = format!("{label} {}", reply.metadata());
    if let Some(note) = &reply.refused_twice {
        let _ = write!(head, "\n{note}");
    }
    if reply.cut_short {
        return format!(
            "{head}\n[The response is larger than {}, the `max_response_bytes` limit for this endpoint, so it could \
             not be read whole and `data` and `errors` are both lost. Ask for fewer fields.]",
            show_bytes(endpoint.max_response_bytes)
        );
    }
    let parts = serde_json::from_str::<BTreeMap<String, Box<RawValue>>>(&reply.body)
        .ok()
        .filter(|parts| parts.contains_key("errors") || parts.contains_key("data"));
    let Some(parts) = parts else {
        return format!(
            "{head}\nThe response was not a GraphQL response: {}",
            excerpt(&reply.body, EXCERPT)
        );
    };
    laid_out(head, "", &reply.body, parts, select, Room::Share(share))
}
