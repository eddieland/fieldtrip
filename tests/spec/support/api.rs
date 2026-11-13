//! A fake GraphQL API over the fixture schema, for the mock endpoint to serve.
//!
//! It answers introspection queries for real, by executing them against `tests/fixtures/schema.graphql`.

use std::path::Path;

use apollo_compiler::collections::HashMap;
use apollo_compiler::response::JsonMap;
use apollo_compiler::schema::Implementers;
use apollo_compiler::validation::Valid;
use apollo_compiler::{ExecutableDocument, Name, Schema};
use serde_json::{Value, json};

use super::mock::{Recorded, Response};

/// A GraphQL API that serves the fixture schema.
#[derive(Debug)]
pub(crate) struct FixtureApi {
    schema: Valid<Schema>,
    implementers: HashMap<Name, Implementers>,
    legacy: bool,
    introspection: bool,
}

impl FixtureApi {
    /// An API that follows the current specification and allows introspection.
    pub(crate) fn new() -> Self {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join("schema.graphql");
        let text = std::fs::read_to_string(&path).expect("read the fixture schema");
        Self::serving(&text)
    }

    /// An API like [`FixtureApi::new`] that serves `sdl` instead of the fixture schema.
    pub(crate) fn serving(sdl: &str) -> Self {
        let schema = Schema::parse_and_validate(sdl, "served.graphql").expect("the served schema is valid");
        let implementers = schema.implementers_map();
        Self {
            schema,
            implementers,
            legacy: false,
            introspection: true,
        }
    }

    /// Makes the API reject the introspection fields that were added after the June 2018 specification.
    pub(crate) fn legacy(mut self) -> Self {
        self.legacy = true;
        self
    }

    /// Makes the API refuse introspection, as many production APIs do.
    pub(crate) fn without_introspection(mut self) -> Self {
        self.introspection = false;
        self
    }

    /// Answers one request.
    pub(crate) fn respond(&self, request: &Recorded) -> Response {
        let body: Value = serde_json::from_str(&request.body).unwrap_or(Value::Null);
        let query = body.get("query").and_then(Value::as_str).unwrap_or_default();
        let operation_name = body.get("operationName").and_then(Value::as_str);

        if query.contains("__schema") && !self.introspection {
            return errors(
                400,
                "GraphQL introspection is not allowed, but the query contained __schema.",
            );
        }
        if self.legacy {
            for (field, parent) in [("specifiedByURL", "__Type"), ("description", "__Schema")] {
                // `__Schema.description` is the only `description` that is followed directly by `queryType`.
                let marker = if field == "description" {
                    "description queryType"
                } else {
                    field
                };
                if query.contains(marker) {
                    return errors(400, &format!("Cannot query field \"{field}\" on type \"{parent}\"."));
                }
            }
            if query.contains("args(includeDeprecated") {
                return errors(400, "Unknown argument \"includeDeprecated\" on field \"__Field.args\".");
            }
        }

        let document = match ExecutableDocument::parse_and_validate(&self.schema, query, "request.graphql") {
            Ok(document) => document,
            Err(invalid) => return errors(400, &invalid.errors.to_string()),
        };
        let operation = match document.operations.get(operation_name) {
            Ok(operation) => operation,
            Err(error) => return errors(400, &format!("{error:?}")),
        };
        let variables = match apollo_compiler::request::coerce_variable_values(&self.schema, operation, &JsonMap::new())
        {
            Ok(variables) => variables,
            Err(error) => return errors(400, &format!("{error:?}")),
        };
        match apollo_compiler::introspection::partial_execute(
            &self.schema,
            &self.implementers,
            &document,
            operation,
            &variables,
        ) {
            Ok(response) => Response::new(200, serde_json::to_string(&response).expect("serialize the response")),
            Err(error) => errors(400, &format!("{error:?}")),
        }
    }
}

/// A GraphQL response that carries only an error.
fn errors(status: u16, message: &str) -> Response {
    Response::new(status, json!({ "errors": [{ "message": message }] }).to_string())
}
