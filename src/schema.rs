//! The in-memory schema that every tool reads, whichever source it was loaded from.

mod describe;
pub(crate) mod introspection;
mod overview;
mod render;
mod search;

use apollo_compiler::collections::HashMap;
use apollo_compiler::schema::Implementers;
use apollo_compiler::validation::Valid;
use apollo_compiler::{Name, Schema};

/// How many lines of schema diagnostics to pass on before cutting off.
const MAX_DIAGNOSTIC_LINES: usize = 40;

/// A loaded, validated schema.
#[derive(Debug)]
pub(crate) struct Loaded {
    schema: Valid<Schema>,
    implementers: HashMap<Name, Implementers>,
}

impl Loaded {
    /// Builds a schema from SDL text. `origin` says where the text came from, for error messages, as in
    /// `the schema file /path/schema.graphql`.
    ///
    /// # Errors
    ///
    /// Returns the parser's and validator's diagnostics when the text is not a valid schema.
    pub(crate) fn from_sdl(text: &str, origin: &str) -> Result<Self, String> {
        let schema = Schema::parse_and_validate(text, "schema.graphql").map_err(|invalid| {
            let report = invalid.errors.to_string();
            let mut lines: Vec<&str> = report.lines().take(MAX_DIAGNOSTIC_LINES + 1).collect();
            if lines.len() > MAX_DIAGNOSTIC_LINES {
                lines.truncate(MAX_DIAGNOSTIC_LINES);
                lines.push("(more diagnostics left out)");
            }
            format!("{origin} is not a valid GraphQL schema:\n{}", lines.join("\n"))
        })?;
        let implementers = schema.implementers_map();
        Ok(Self { schema, implementers })
    }

    /// Returns compact SDL for the type or `Type.field` called `name`.
    ///
    /// # Errors
    ///
    /// Returns a message naming the nearest matches when nothing in the schema is called `name`.
    pub(crate) fn describe(&self, name: &str) -> Result<String, String> {
        describe::describe(self, name)
    }

    /// Searches names and descriptions for `query` and returns the best matches as one-line signatures.
    ///
    /// # Errors
    ///
    /// Returns a message when `query` has no words in it.
    pub(crate) fn search(&self, endpoint: &str, query: &str) -> Result<String, String> {
        search::search(self, endpoint, query)
    }

    /// Returns the schema's description, its type counts and the names of its root fields.
    pub(crate) fn overview(&self) -> String {
        overview::overview(self)
    }
}
