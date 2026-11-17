//! What to add to a validation error, so the calling agent can fix the operation without another tool call.
//!
//! The validator only gives each error as text, so this reads the names back out of the message.

use std::fmt::Write as _;

use apollo_compiler::Node;
use apollo_compiler::ast::InputValueDefinition;
use apollo_compiler::schema::ExtendedType;

use crate::schema::Loaded;
use crate::suggest::{backticked, nearest};

/// How many fields, arguments or enum values one error lists before it counts the rest.
const MAX_LISTED: usize = 12;

/// How many near names an unknown output field gets.
const NEAR_FIELDS: usize = 5;

/// Returns `message`, one error from the validator, followed by the names the schema does have.
///
/// An unknown output field gets the nearest fields of its type. An unknown input object field, argument or enum
/// value gets the whole list, with types. A message about anything else comes back unchanged.
pub(crate) fn explain(loaded: &Loaded, message: &str) -> String {
    output_field(loaded, message)
        .or_else(|| input_field(loaded, message))
        .or_else(|| enum_value(loaded, message))
        .or_else(|| argument(loaded, message))
        .unwrap_or_else(|| message.to_owned())
}

/// ``type `T` does not have a field `f` ``: the nearest fields that the type does have.
fn output_field(loaded: &Loaded, message: &str) -> Option<String> {
    let rest = message.strip_prefix("type `")?;
    let (type_name, rest) = rest.split_once("` does not have a field `")?;
    let (field_name, _) = rest.split_once('`')?;
    let near = nearest(field_name, loaded.field_names(type_name), NEAR_FIELDS);
    (!near.is_empty()).then(|| format!("{message}. Nearest fields on `{type_name}`: {}", backticked(near)))
}

/// ``field `f` does not exist on `T` ``, where `T` is an input object: its fields.
fn input_field(loaded: &Loaded, message: &str) -> Option<String> {
    let (field_name, type_name) = missing(message, "field `")?;
    let ExtendedType::InputObject(input) = loaded.schema().types.get(type_name)? else {
        return None;
    };
    let fields = input.fields.values().map(|field| input_value(field)).collect();
    Some(format!(
        "field `{field_name}` does not exist on input `{type_name}`. {}",
        listed("fields", field_name, fields)
    ))
}

/// ``value `V` does not exist on `E` ``, where `E` is an enum: its values.
fn enum_value(loaded: &Loaded, message: &str) -> Option<String> {
    let (value, type_name) = missing(message, "value `")?;
    let ExtendedType::Enum(definition) = loaded.schema().types.get(type_name)? else {
        return None;
    };
    let values = definition
        .values
        .keys()
        .map(|name| (name.as_str(), name.to_string()))
        .collect();
    Some(format!(
        "value `{value}` does not exist on enum `{type_name}`. {}",
        listed("values", value, values)
    ))
}

/// ``the argument `a` is not supported by `T.f` ``, or by `@directive`: the arguments it does take.
fn argument(loaded: &Loaded, message: &str) -> Option<String> {
    let rest = message.strip_prefix("the argument `")?;
    let (name, rest) = rest.split_once("` is not supported by `")?;
    let coordinate = rest.strip_suffix('`')?;
    let schema = loaded.schema();
    let definitions = if let Some(directive) = coordinate.strip_prefix('@') {
        &schema.directive_definitions.get(directive)?.arguments
    } else {
        let (type_name, field_name) = coordinate.split_once('.')?;
        &schema.type_field(type_name, field_name).ok()?.arguments
    };
    if definitions.is_empty() {
        return Some(format!("{message}. It takes no arguments."));
    }
    let arguments = definitions.iter().map(input_value).collect();
    Some(format!("{message}. {}", listed("arguments", name, arguments)))
}

/// Splits ``<prefix>name` does not exist on `Type` `` into the name and the type.
fn missing<'a>(message: &'a str, prefix: &str) -> Option<(&'a str, &'a str)> {
    let rest = message.strip_prefix(prefix)?;
    let (name, rest) = rest.split_once("` does not exist on `")?;
    Some((name, rest.strip_suffix('`')?))
}

/// An argument or input field by name and as `name: Type`, with its default when it has one.
fn input_value(definition: &Node<InputValueDefinition>) -> (&str, String) {
    let mut shown = format!("{}: {}", definition.name, definition.ty);
    if let Some(default) = &definition.default_value {
        let _ = write!(shown, " = {default}");
    }
    (definition.name.as_str(), shown)
}

/// `Its fields are: a: A, b: B`, with `entries` as name and text, in schema order.
///
/// Past [`MAX_LISTED`] entries, the names nearest to `unknown` come first, then the others in schema order, and
/// the ones left out are counted.
fn listed(what: &str, unknown: &str, entries: Vec<(&str, String)>) -> String {
    let total = entries.len();
    if total <= MAX_LISTED {
        let shown: Vec<String> = entries.into_iter().map(|(_, shown)| shown).collect();
        return format!("Its {what} are: {}", shown.join(", "));
    }
    let near = nearest(unknown, entries.iter().map(|(name, _)| *name), MAX_LISTED);
    let (mut first, mut others) = (Vec::new(), Vec::new());
    for (name, shown) in entries {
        match near.iter().position(|close| *close == name) {
            Some(rank) => first.push((rank, shown)),
            None => others.push(shown),
        }
    }
    first.sort_unstable();
    let shown: Vec<String> = first
        .into_iter()
        .map(|(_, shown)| shown)
        .chain(others)
        .take(MAX_LISTED)
        .collect();
    format!(
        "Its {total} {what}, nearest first: {}, and {} more",
        shown.join(", "),
        total - MAX_LISTED
    )
}
