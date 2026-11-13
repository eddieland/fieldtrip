//! The in-memory schema that every tool reads, whichever source it was loaded from.

mod describe;
pub(crate) mod introspection;
mod lint;
mod overview;
mod render;
mod search;

use apollo_compiler::collections::HashMap;
use apollo_compiler::schema::{ExtendedType, Implementers};
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

    /// The validated schema, for checking operations against.
    pub(crate) const fn schema(&self) -> &Valid<Schema> {
        &self.schema
    }

    /// The names of the fields of the object or interface called `type_name`. Empty for anything else.
    pub(crate) fn field_names(&self, type_name: &str) -> Vec<&str> {
        match self.schema.types.get(type_name) {
            Some(ExtendedType::Object(object)) => object.fields.keys().map(Name::as_str).collect(),
            Some(ExtendedType::Interface(interface)) => interface.fields.keys().map(Name::as_str).collect(),
            _ => Vec::new(),
        }
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

    /// Says which rules of `costs` can never match this schema. Empty when the endpoint has no cost map.
    pub(crate) fn cost_map_report(&self, costs: &crate::costs::CostMap) -> String {
        lint::report(self, costs)
    }

    /// One sentence per kind of cost map rule that cannot apply in this schema. Empty when every rule can.
    pub(crate) fn cost_map_findings(&self, costs: &crate::costs::CostMap) -> Vec<String> {
        lint::findings(self, costs)
    }

    /// Returns the schema's description, its type counts and the names of its root fields.
    pub(crate) fn overview(&self) -> String {
        overview::overview(self)
    }
}
