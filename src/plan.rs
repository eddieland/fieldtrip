//! Planning an operation: parse it, validate it against the schema, and estimate what it costs.
//!
//! Nothing here touches the network. The same plan backs `estimate_cost` and the approval prompt of `execute`.

use std::fmt::Write as _;

use apollo_compiler::ExecutableDocument;
use apollo_compiler::ast::{OperationType, Value};
use apollo_compiler::executable::{Field, Operation, Selection, SelectionSet};
use apollo_compiler::validation::Valid;
use serde_json::Map;

use crate::config::Endpoint;
use crate::costs::{CostMap, Step};
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
    /// Every cost map rule the estimate used, and what it set.
    pub(crate) overrides: Vec<String>,
}

impl Plan {
    /// Parses and validates `operation` against `loaded`, then estimates its cost.
    ///
    /// `name` is the endpoint's name, for messages. `endpoint` supplies the cost map and the size to assume for a
    /// list that nothing bounds.
    ///
    /// # Errors
    ///
    /// Returns a message listing what is wrong when the text is not exactly one valid query or mutation.
    pub(crate) fn new(
        loaded: &Loaded,
        name: &str,
        endpoint: &Endpoint,
        operation: &str,
        variables: &Variables,
    ) -> Result<Self, String> {
        let document = ExecutableDocument::parse_and_validate(loaded.schema(), operation, "operation.graphql")
            .map_err(|invalid| invalid_operation(loaded, name, &invalid.errors))?;
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
            assumed_list_size: endpoint.assumed_list_size,
            costs: &endpoint.costs,
            path: Vec::new(),
            assumptions: Vec::new(),
            overrides: Vec::new(),
        };
        let mut breakdown = Vec::new();
        let mut depth = 0;
        estimator.top_level(&operation.selection_set, kind, &mut breakdown, &mut depth);
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
            overrides: estimator.overrides,
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
        if !self.overrides.is_empty() {
            out.push_str("From the cost map:\n");
            for used in &self.overrides {
                let _ = writeln!(out, "  - {used}");
            }
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
    costs: &'doc CostMap,
    /// The fields from the root down to the one being costed. Cost map rules are matched against it.
    path: Vec<Step<'doc>>,
    assumptions: Vec<String>,
    overrides: Vec<String>,
}

/// What an operation says about the size of a list.
#[derive(Clone)]
enum Size {
    /// A size argument with a known value, or a multiplier from the cost map.
    Known(u64),
    /// A size argument was given, but as a variable with no value.
    Unresolved(String),
    /// No size argument applies.
    Absent,
}

impl<'doc> Estimator<'doc> {
    /// Costs each top-level field, following fragments, and records the deepest nesting seen.
    fn top_level(&mut self, set: &'doc SelectionSet, kind: Kind, breakdown: &mut Vec<(String, u64)>, depth: &mut u32) {
        let document = self.document;
        for selection in &set.selections {
            match selection {
                Selection::Field(field) => {
                    let cost = self.field(set, field, &Size::Absent, 1, depth, kind == Kind::Mutation);
                    breakdown.push((field.response_key().to_string(), cost));
                }
                Selection::InlineFragment(inline) => self.top_level(&inline.selection_set, kind, breakdown, depth),
                Selection::FragmentSpread(spread) => {
                    if let Some(fragment) = spread.fragment_def(document) {
                        self.top_level(&fragment.selection_set, kind, breakdown, depth);
                    }
                }
            }
        }
    }

    /// The cost of everything selected in `set`.
    ///
    /// `inherited` is the size given on the field that owns this set, when that field is not a list itself. It
    /// bounds the lists directly inside, which is how connection types page.
    fn selection_set(&mut self, set: &'doc SelectionSet, inherited: &Size, level: u32, depth: &mut u32) -> u64 {
        let document = self.document;
        let mut total = 0_u64;
        for selection in &set.selections {
            let cost = match selection {
                Selection::Field(field) => self.field(set, field, inherited, level, depth, false),
                Selection::InlineFragment(inline) => self.selection_set(&inline.selection_set, inherited, level, depth),
                Selection::FragmentSpread(spread) => spread.fragment_def(document).map_or(0, |fragment| {
                    self.selection_set(&fragment.selection_set, inherited, level, depth)
                }),
            };
            total = total.saturating_add(cost);
        }
        total
    }

    /// The cost of one field and everything under it. `is_mutation` marks a top-level field of a mutation.
    fn field(
        &mut self,
        parent: &'doc SelectionSet,
        field: &'doc Field,
        inherited: &Size,
        level: u32,
        depth: &mut u32,
        is_mutation: bool,
    ) -> u64 {
        *depth = (*depth).max(level);
        // The introspection fields are answered from memory, and no rule can name them.
        if field.name.starts_with("__") {
            return 0;
        }
        self.path.push(Step {
            parent: parent.ty.as_str(),
            field: field.name.as_str(),
        });
        let cost = self.costed(field, inherited, level, depth, is_mutation);
        self.path.pop();
        cost
    }

    /// The cost of the field at the end of the path: what it costs itself, times its size, plus what is under it.
    fn costed(&mut self, field: &'doc Field, inherited: &Size, level: u32, depth: &mut u32, is_mutation: bool) -> u64 {
        let costs = self.costs;
        let is_leaf = field.selection_set.selections.is_empty();
        let is_list = field.ty().is_list();

        // What the field itself costs: the cost map's word, or the built-in guess.
        let own = if let Some((cost, rule)) = costs.cost(&self.path) {
            self.used(&format!("cost {cost}"), rule);
            cost
        } else if is_mutation {
            MUTATION_COST
        } else if is_leaf {
            0
        } else {
            OBJECT_COST
        };
        // A scalar that costs nothing needs no size, and working one out would only add noise to the assumptions.
        if is_leaf && (own == 0 || !is_list || is_mutation) {
            return own;
        }

        // The size the field carries: the cost map's word, or what its arguments say.
        let own_size = if let Some((multiplier, rule)) = costs.multiplier(&self.path) {
            self.used(&format!("multiplier {multiplier}"), rule);
            Size::Known(multiplier)
        } else {
            self.size_argument(field)
        };
        if !is_list {
            let children = self.selection_set(&field.selection_set, &own_size, level + 1, depth);
            return own.saturating_add(children);
        }

        // A list's own size wins. Without one, the list takes the size given on the field above it.
        let size = if matches!(own_size, Size::Absent) {
            inherited.clone()
        } else {
            own_size
        };
        let size = self.settled(field, size);
        let children = self.selection_set(&field.selection_set, &Size::Absent, level + 1, depth);
        if is_mutation {
            // The mutation is charged once, and each item it returns is an object like any other.
            return own.saturating_add(size.saturating_mul(OBJECT_COST.saturating_add(children)));
        }
        size.saturating_mul(own.saturating_add(children))
    }

    /// Turns what is known about a list's size into a number, assuming one when nothing is known.
    fn settled(&mut self, field: &Field, size: Size) -> u64 {
        let path = self.coordinate();
        match size {
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
        }
    }

    /// `` `Type.field` `` for the field at the end of the path.
    fn coordinate(&self) -> String {
        self.path
            .last()
            .map_or_else(String::new, |step| format!("`{}.{}`", step.parent, step.field))
    }

    /// Records, once, that the cost map rule `rule` set `what` for the field at the end of the path.
    fn used(&mut self, what: &str, rule: &str) {
        let used = format!("{}: {what} (rule `{rule}`)", self.coordinate());
        if !self.overrides.contains(&used) {
            self.overrides.push(used);
        }
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
