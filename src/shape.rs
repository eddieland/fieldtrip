//! The shape of a response: one line for each path that holds a value, with its type, such as
//! `data.repository.issues.nodes[50].body: string (31,204 chars)`.
//!
//! It goes with the preview of a response that was written to a file, and tells the calling agent what the file
//! holds, and so which path to ask for. The items of a list are merged into one set of lines, with the list's length
//! in the path, so a list of thousands takes no more room than one item.

use std::collections::HashMap;
use std::fmt::Write as _;

use serde_json::Value;

/// A string at least this long has its length shown.
const LONG_STRING: usize = 100;

/// Everything seen at one path, across the items of every list above it.
#[derive(Debug, Default)]
struct Node {
    /// Whether a null was seen here.
    null: bool,
    /// Whether a boolean was.
    boolean: bool,
    /// Whether a number was.
    number: bool,
    /// The shortest and longest string seen here, in characters.
    strings: Option<(usize, usize)>,
    /// How many objects were seen here.
    objects: usize,
    /// The members of the objects seen here, in the order they first appeared.
    members: Vec<(String, Self)>,
    /// Where each member is in `members`.
    positions: HashMap<String, usize>,
    /// The shortest and longest list seen here, in items.
    lists: Option<(usize, usize)>,
    /// The items of the lists seen here, merged.
    items: Option<Box<Self>>,
}

/// Widens `range` to hold `length`.
fn widen(range: &mut Option<(usize, usize)>, length: usize) {
    *range = Some(range.map_or((length, length), |(low, high)| (low.min(length), high.max(length))));
}

impl Node {
    /// Adds `value` to what was seen here.
    fn see(&mut self, value: &Value) {
        match value {
            Value::Null => self.null = true,
            Value::Bool(_) => self.boolean = true,
            Value::Number(_) => self.number = true,
            Value::String(text) => widen(&mut self.strings, text.chars().count()),
            Value::Array(items) => {
                widen(&mut self.lists, items.len());
                for item in items {
                    self.items.get_or_insert_default().see(item);
                }
            }
            Value::Object(map) => {
                self.objects += 1;
                for (name, inner) in map {
                    let position = *self.positions.entry(name.clone()).or_insert_with(|| {
                        self.members.push((name.clone(), Self::default()));
                        self.members.len() - 1
                    });
                    if let Some((_, member)) = self.members.get_mut(position) {
                        member.see(inner);
                    }
                }
            }
        }
    }

    /// Writes this node's lines, and those of everything under it, to `lines`. `path` is how it was reached.
    fn write(&self, path: &str, lines: &mut Vec<String>) {
        let mut kinds = Vec::new();
        if self.null {
            kinds.push("null".to_owned());
        }
        if self.boolean {
            kinds.push("boolean".to_owned());
        }
        if self.number {
            kinds.push("number".to_owned());
        }
        match self.strings {
            Some((_, longest)) if longest < LONG_STRING => kinds.push("string".to_owned()),
            Some((shortest, longest)) if shortest == longest => {
                kinds.push(format!("string ({} chars)", grouped(longest)));
            }
            Some((_, longest)) => kinds.push(format!("string (up to {} chars)", grouped(longest))),
            None => {}
        }
        if self.objects > 0 && self.members.is_empty() {
            kinds.push("empty object".to_owned());
        }
        // A null next to an object or a list says only that the field is nullable, which the schema says better.
        let scalar = self.boolean || self.number || self.strings.is_some();
        let holds_more = !self.members.is_empty() || self.lists.is_some();
        if !kinds.is_empty() && (scalar || !holds_more) {
            lines.push(format!("{path}: {}", kinds.join(" | ")));
        }
        for (name, member) in &self.members {
            let inner = if path.is_empty() {
                name.clone()
            } else {
                format!("{path}.{name}")
            };
            member.write(&inner, lines);
        }
        if let Some((shortest, longest)) = self.lists {
            let mut inner = path.to_owned();
            if shortest == longest {
                let _ = write!(inner, "[{}]", grouped(longest));
            } else {
                let _ = write!(inner, "[{}-{}]", grouped(shortest), grouped(longest));
            }
            match &self.items {
                Some(items) => items.write(&inner, lines),
                None => lines.push(format!("{inner}: empty list")),
            }
        }
    }
}

/// `number` with a comma between each group of three digits: `31,204`.
pub(crate) fn grouped(number: usize) -> String {
    let digits = number.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

/// One line for each path in `response` that holds a value, in the order the response has them.
///
/// A path is written as `select` takes it, except that a list shows how many items it has, as `nodes[50]`, or
/// `nodes[0-7]` when lists at that path differ in length.
pub(crate) fn lines(response: &Value) -> Vec<String> {
    let mut root = Node::default();
    root.see(response);
    let mut lines = Vec::new();
    root.write("", &mut lines);
    lines
}
