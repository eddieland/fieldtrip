//! Planning an operation: parse it, validate it against the schema, and estimate what it costs.
//!
//! Nothing here touches the network. The same plan backs `estimate_cost` and the approval prompt of `execute`.

use std::fmt::Write as _;

use apollo_compiler::ExecutableDocument;
use apollo_compiler::ast::{OperationType, Value};
use apollo_compiler::executable::{Field, Operation, Selection, SelectionSet};
use apollo_compiler::validation::Valid;
use serde_json::Map;

use crate::schema::Loaded;
use crate::suggest::{backticked, nearest};

/// What one object costs.
const OBJECT_COST: u64 = 1;
/// What one top-level mutation field costs, in place of [`OBJECT_COST`].
const MUTATION_COST: u64 = 10;
/// The arguments that bound the size of a list.
const SIZE_ARGUMENTS: [&str; 3] = ["first", "last", "limit"];
/// How many validation errors to report.
const MAX_ERRORS: usize = 10;

/// The JSON object a caller passes as GraphQL variables.
pub(crate) type Variables = Map<String, serde_json::Value>;

/// Whether an operation reads or writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    /// A query.
    Query,
    /// A mutation.
    Mutation,
}

impl Kind {
    /// The GraphQL keyword.
    pub(crate) const fn keyword(self) -> &'static str {
        match self {
            Self::Query => "query",
            Self::Mutation => "mutation",
        }
    }
}

/// A validated operation and its estimated cost.
#[derive(Debug)]
pub(crate) struct Plan {
    /// Whether the operation is a query or a mutation.
    pub(crate) kind: Kind,
    /// The operation's name, when it has one.
    pub(crate) name: Option<String>,
    /// The document in canonical form, without comments. This is the text that is shown for approval and sent.
    pub(crate) text: String,
    /// The estimated total cost.
    pub(crate) cost: u64,
    /// The deepest nesting of fields.
    pub(crate) depth: u32,
    /// The cost of each top-level field, by response key.
    pub(crate) breakdown: Vec<(String, u64)>,
    /// Every guess the estimate rests on.
    pub(crate) assumptions: Vec<String>,
}

impl Plan {
    /// Parses and validates `operation` against `loaded`, then estimates its cost.
    ///
    /// `assumed_list_size` is used for a list whose size nothing in the operation or the schema bounds.
    ///
    /// # Errors
    ///
    /// Returns a message listing what is wrong when the text is not exactly one valid query or mutation.
    pub(crate) fn new(
        loaded: &Loaded,
        endpoint: &str,
        operation: &str,
        variables: &Variables,
        assumed_list_size: u64,
    ) -> Result<Self, String> {
        let document = ExecutableDocument::parse_and_validate(loaded.schema(), operation, "operation.graphql")
            .map_err(|invalid| invalid_operation(loaded, endpoint, &invalid.errors))?;
        let operation = single_operation(&document)?;
        let kind = match operation.operation_type {
            OperationType::Query => Kind::Query,
            OperationType::Mutation => Kind::Mutation,
            OperationType::Subscription => {
                return Err(
                    "Subscriptions are not supported. Only queries and mutations can be estimated and \
                            executed."
                        .to_owned(),
                );
            }
        };

        let mut estimator = Estimator {
            document: &document,
            operation,
            variables,
            assumed_list_size,
            assumptions: Vec::new(),
        };
        let mut breakdown = Vec::new();
        let mut depth = 0;
        estimator.root_fields(&operation.selection_set, kind, &mut breakdown, &mut depth);
        let cost = breakdown
            .iter()
            .fold(0_u64, |total, (_, cost)| total.saturating_add(*cost));
        Ok(Self {
            kind,
            name: operation.name.as_ref().map(ToString::to_string),
            text: document.to_string().trim().to_owned(),
            cost,
            depth,
            breakdown,
            assumptions: estimator.assumptions,
        })
    }

    /// Checks that `variables` has a usable value for every variable the operation declares.
    ///
    /// # Errors
    ///
    /// Returns a message naming the variable that is missing or has the wrong type.
    pub(crate) fn check_variables(&self, loaded: &Loaded, variables: &Variables) -> Result<(), String> {
        let document = ExecutableDocument::parse_and_validate(loaded.schema(), &self.text, "operation.graphql")
            .map_err(|invalid| format!("The operation stopped being valid: {}", invalid.errors))?;
        let operation = single_operation(&document)?;
        let values = serde_json::from_value(serde_json::Value::Object(variables.clone()))
            .map_err(|error| format!("The variables could not be read: {error}"))?;
        apollo_compiler::request::coerce_variable_values(loaded.schema(), operation, &values)
            .map(drop)
            .map_err(|error| format!("The variables do not fit the operation: {}", error.message()))
    }

    /// The request body for this operation, as GraphQL over HTTP defines it.
    pub(crate) fn payload(&self, variables: &Variables) -> serde_json::Value {
        let mut payload = serde_json::json!({ "query": self.text, "variables": variables });
        if let (Some(name), Some(object)) = (&self.name, payload.as_object_mut()) {
            object.insert("operationName".to_owned(), serde_json::Value::String(name.clone()));
        }
        payload
    }

    /// The estimate as text: cost, depth, cost per top-level field, and the assumptions.
    pub(crate) fn estimate(&self) -> String {
        let mut out = format!(
            "Estimated cost: {}\nDepth: {}\nBy top-level field:\n",
            self.cost, self.depth
        );
        for (key, cost) in &self.breakdown {
            let _ = writeln!(out, "  {key}: {cost}");
        }
        if !self.assumptions.is_empty() {
            out.push_str("Assumptions:\n");
            for assumption in &self.assumptions {
                let _ = writeln!(out, "  - {assumption}");
            }
        }
        out
    }

    /// `query` or `` mutation `Name` ``.
    pub(crate) fn title(&self) -> String {
        self.name.as_ref().map_or_else(
            || self.kind.keyword().to_owned(),
            |name| format!("{} `{name}`", self.kind.keyword()),
        )
    }
}

/// Returns the document's only operation.
fn single_operation(document: &Valid<ExecutableDocument>) -> Result<&Operation, String> {
    let mut operations = document.operations.iter();
    match (operations.next(), operations.next()) {
        (Some(operation), None) => Ok(operation),
        (None, _) => Err("The document has no operation in it. Send one query or one mutation.".to_owned()),
        (Some(_), Some(_)) => {
            let names: Vec<String> = document
                .operations
                .iter()
                .map(|operation| {
                    operation
                        .name
                        .as_ref()
                        .map_or_else(|| "(unnamed)".to_owned(), ToString::to_string)
                })
                .collect();
            Err(format!(
                "The document has {} operations ({}). Send exactly one operation per call, with the fragments it \
                 uses.",
                names.len(),
                names.join(", ")
            ))
        }
    }
}

/// The error for an operation that does not parse or does not validate.
fn invalid_operation(loaded: &Loaded, endpoint: &str, errors: &apollo_compiler::validation::DiagnosticList) -> String {
    let mut out = format!("The operation is not valid for `{endpoint}`:\n");
    for diagnostic in errors.iter().take(MAX_ERRORS) {
        let message = diagnostic.error.to_string();
        let position = diagnostic.line_column_range().map_or_else(String::new, |range| {
            format!("{}:{} ", range.start.line, range.start.column)
        });
        let _ = write!(out, "- {position}{message}");
        if let Some(hint) = field_hint(loaded, &message) {
            let _ = write!(out, ". {hint}");
        }
        out.push('\n');
    }
    if errors.len() > MAX_ERRORS {
        let _ = writeln!(out, "- and {} more", errors.len() - MAX_ERRORS);
    }
    out.push_str("Use `describe` to check the names and types, then try again.");
    out
}

/// For an unknown-field error, the nearest fields that the type does have.
///
/// The validator only gives the error as text, so this reads the type and field names back out of it.
fn field_hint(loaded: &Loaded, message: &str) -> Option<String> {
    let rest = message.strip_prefix("type `")?;
    let (type_name, rest) = rest.split_once("` does not have a field `")?;
    let (field_name, _) = rest.split_once('`')?;
    let near = nearest(field_name, loaded.field_names(type_name), 5);
    (!near.is_empty()).then(|| format!("Nearest fields on `{type_name}`: {}", backticked(near)))
}

/// Walks an operation and adds up its cost.
struct Estimator<'doc> {
    document: &'doc Valid<ExecutableDocument>,
    operation: &'doc Operation,
    variables: &'doc Variables,
    assumed_list_size: u64,
    assumptions: Vec<String>,
}

/// What an operation says about the size of a list.
#[derive(Clone)]
enum Size {
    /// A size argument with a known value.
    Known(u64),
    /// A size argument was given, but as a variable with no value.
    Unresolved(String),
    /// No size argument applies.
    Absent,
}

impl Estimator<'_> {
    /// Costs each top-level field, following fragments, and records the deepest nesting seen.
    fn root_fields(&mut self, set: &SelectionSet, kind: Kind, breakdown: &mut Vec<(String, u64)>, depth: &mut u32) {
        for selection in &set.selections {
            match selection {
                Selection::Field(field) => {
                    let mut cost = self.field(set, field, &Size::Absent, 1, depth);
                    if kind == Kind::Mutation {
                        let is_single_object = !field.selection_set.selections.is_empty() && !field.ty().is_list();
                        let replaced = if is_single_object { OBJECT_COST } else { 0 };
                        cost = cost.saturating_sub(replaced).saturating_add(MUTATION_COST);
                    }
                    breakdown.push((field.response_key().to_string(), cost));
                }
                Selection::InlineFragment(inline) => self.root_fields(&inline.selection_set, kind, breakdown, depth),
                Selection::FragmentSpread(spread) => {
                    if let Some(fragment) = spread.fragment_def(self.document) {
                        self.root_fields(&fragment.selection_set, kind, breakdown, depth);
                    }
                }
            }
        }
    }

    /// The cost of everything selected in `set`.
    ///
    /// `inherited` is a size argument given on the field that owns this set, when that field is not a list
    /// itself. It bounds the lists directly inside, which is how connection types page.
    fn selection_set(&mut self, set: &SelectionSet, inherited: &Size, level: u32, depth: &mut u32) -> u64 {
        let mut total = 0_u64;
        for selection in &set.selections {
            let cost = match selection {
                Selection::Field(field) => self.field(set, field, inherited, level, depth),
                Selection::InlineFragment(inline) => self.selection_set(&inline.selection_set, inherited, level, depth),
                Selection::FragmentSpread(spread) => spread.fragment_def(self.document).map_or(0, |fragment| {
                    self.selection_set(&fragment.selection_set, inherited, level, depth)
                }),
            };
            total = total.saturating_add(cost);
        }
        total
    }

    /// The cost of one field and everything under it.
    fn field(&mut self, parent: &SelectionSet, field: &Field, inherited: &Size, level: u32, depth: &mut u32) -> u64 {
        *depth = (*depth).max(level);
        // Scalars and enums are free, and so are the introspection fields, which servers answer from memory.
        if field.selection_set.selections.is_empty() || field.name.starts_with("__") {
            return 0;
        }
        let own_size = self.size_argument(field);
        if !field.ty().is_list() {
            let children = self.selection_set(&field.selection_set, &own_size, level + 1, depth);
            return OBJECT_COST.saturating_add(children);
        }
        // A list's own size argument wins. Without one, the list takes the size given on the field above it.
        let size = if matches!(own_size, Size::Absent) {
            inherited.clone()
        } else {
            own_size
        };

        let path = format!("`{}.{}`", parent.ty, field.name);
        let size = match size {
            Size::Known(size) => size,
            Size::Unresolved(variable) => {
                self.assume(format!(
                    "{path} is sized by `${variable}`, which has no value, so {} items were assumed.",
                    self.assumed_list_size
                ));
                self.assumed_list_size
            }
            Size::Absent => {
                let takes_one = SIZE_ARGUMENTS
                    .iter()
                    .any(|name| field.definition.argument_by_name(name).is_some());
                let why = if takes_one {
                    "was given no size argument"
                } else {
                    "takes no size argument"
                };
                self.assume(format!(
                    "{path} {why}, so {} items were assumed.",
                    self.assumed_list_size
                ));
                self.assumed_list_size
            }
        };
        let children = self.selection_set(&field.selection_set, &Size::Absent, level + 1, depth);
        size.saturating_mul(OBJECT_COST.saturating_add(children))
    }

    /// Records an assumption once.
    fn assume(&mut self, assumption: String) {
        if !self.assumptions.contains(&assumption) {
            self.assumptions.push(assumption);
        }
    }

    /// Reads the field's size arguments: from the operation, else from the defaults the schema declares.
    fn size_argument(&self, field: &Field) -> Size {
        let mut known: Option<u64> = None;
        let mut unresolved: Option<String> = None;
        for name in SIZE_ARGUMENTS {
            let Some(definition) = field.definition.argument_by_name(name) else {
                continue;
            };
            let given = field
                .arguments
                .iter()
                .find(|argument| argument.name == name)
                .map(|argument| &*argument.value);
            let value = match given {
                Some(Value::Variable(variable)) => {
                    let value = self.variable(variable.as_str());
                    if value.is_none() {
                        unresolved = Some(variable.to_string());
                    }
                    value
                }
                Some(value) => integer(value),
                None => definition.default_value.as_deref().and_then(integer),
            };
            if let Some(value) = value {
                known = Some(known.map_or(value, |current| current.max(value)));
            }
        }
        match (known, unresolved) {
            (Some(size), _) => Size::Known(size),
            (None, Some(variable)) => Size::Unresolved(variable),
            (None, None) => Size::Absent,
        }
    }

    /// The value of an integer variable: what the caller passed, else the default the operation declares.
    fn variable(&self, name: &str) -> Option<u64> {
        if let Some(value) = self.variables.get(name) {
            return value.as_u64().or_else(|| value.as_i64().map(|_| 0));
        }
        self.operation
            .variables
            .iter()
            .find(|definition| definition.name == name)
            .and_then(|definition| definition.default_value.as_deref())
            .and_then(integer)
    }
}

/// A GraphQL integer literal as a list size. Negative sizes count as zero.
fn integer(value: &Value) -> Option<u64> {
    let Value::Int(int) = value else { return None };
    int.try_to_i32().ok().map(|int| u64::try_from(int).unwrap_or(0))
}
