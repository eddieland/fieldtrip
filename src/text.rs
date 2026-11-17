//! Holding a tool result to a number of characters.
//!
//! Limits are counted in characters, not bytes, since that is how `max_output_chars` is written and a cut can then
//! never fall inside one.

use std::fmt::Write as _;

/// The first `limit` characters of `text`, to quote in a message.
pub(crate) fn excerpt(text: &str, limit: usize) -> String {
    text.chars().take(limit).collect()
}

/// Cuts `text` to `limit` characters and says so, with `advice` on how to get a smaller result.
pub(crate) fn capped(text: String, limit: usize, advice: &str) -> String {
    let length = text.chars().count();
    if length <= limit {
        return text;
    }
    let mut kept: String = text.chars().take(limit).collect();
    let _ = write!(
        kept,
        "\n[fieldtrip cut this off at {limit} characters. The full result had at least {length}. {advice}]"
    );
    kept
}

/// Like [`capped`], but `head` is always kept whole, and only `rest`, which follows it, is cut.
pub(crate) fn capped_after(head: String, rest: &str, limit: usize, advice: &str) -> String {
    let head_length = head.chars().count();
    let length = head_length + rest.chars().count();
    if length <= limit {
        return head + rest;
    }
    let mut kept = head;
    kept.extend(rest.chars().take(limit.saturating_sub(head_length)));
    let _ = write!(
        kept,
        "\n[fieldtrip cut this off at {limit} characters. The full result had at least {length}. Errors are never \
         cut. {advice}]"
    );
    kept
}
