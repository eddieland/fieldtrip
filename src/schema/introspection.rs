//! Loading a schema by introspection: the queries, and turning the response into SDL.
//!
//! The response is rewritten as SDL and then parsed like a schema file, so every tool sees the same kind of
//! schema whichever source it came from.

use std::fmt::Write as _;

use serde::Deserialize;

use crate::config::{Endpoint, show_bytes};
use crate::http::Client;

/// The parts every introspection query shares: the type reference fragment, deep enough for any real schema.
const TYPE_REF: &str = "fragment TypeRef on __Type { kind name ofType { kind name ofType { kind name ofType { kind \
     name ofType { kind name ofType { kind name ofType { kind name ofType { kind name ofType { kind name ofType { \
     kind name } } } } } } } } } }";

/// The introspection query for servers that follow the October 2021 specification.
const FULL_QUERY: &str = "query IntrospectionQuery { __schema { description queryType { name } mutationType { name } \
     subscriptionType { name } types { ...FullType } } } fragment FullType on __Type { kind name description \
     specifiedByURL fields(includeDeprecated: true) { name description args(includeDeprecated: true) { ...InputValue \
     } type { ...TypeRef } isDeprecated deprecationReason } inputFields(includeDeprecated: true) { ...InputValue } \
     interfaces { ...TypeRef } enumValues(includeDeprecated: true) { name description isDeprecated deprecationReason \
     } possibleTypes { ...TypeRef } } fragment InputValue on __InputValue { name description type { ...TypeRef } \
     defaultValue isDeprecated deprecationReason }";

/// The introspection query that servers following the June 2018 specification accept.
const BASIC_QUERY: &str = "query IntrospectionQuery { __schema { queryType { name } mutationType { name } \
     subscriptionType { name } types { ...FullType } } } fragment FullType on __Type { kind name description \
     fields(includeDeprecated: true) { name description args { ...InputValue } type { ...TypeRef } isDeprecated \
     deprecationReason } inputFields { ...InputValue } interfaces { ...TypeRef } enumValues(includeDeprecated: true) \
     { name description isDeprecated deprecationReason } possibleTypes { ...TypeRef } } fragment InputValue on \
     __InputValue { name description type { ...TypeRef } defaultValue }";

/// How much of an unexpected response body to quote in an error.
const EXCERPT: usize = 200;

/// The scalars every schema has, which SDL must not define again.
const BUILT_IN_SCALARS: [&str; 5] = ["Int", "Float", "String", "Boolean", "ID"];

/// Asks `endpoint` for its schema and returns it as SDL.
///
/// The full query goes first. A server that rejects it gets the older, smaller query.
///
/// # Errors
///
/// Returns a message when the endpoint cannot be reached, answers neither query, or answers with something that
/// is not an introspection result.
pub(crate) async fn fetch(http: &Client, endpoint: &Endpoint) -> Result<String, String> {
    let schema = match attempt(http, endpoint, FULL_QUERY).await? {
        Ok(schema) => schema,
        Err(_) => attempt(http, endpoint, BASIC_QUERY).await?.map_err(|reason| {
            format!(
                "{} did not answer the introspection query: {reason}. If introspection is turned off for this API, \
                 ask the user to export the schema to a file and set `schema` for this endpoint in the fieldtrip \
                 config.",
                endpoint.shown_url
            )
        })?,
    };
    to_sdl(&schema)
}

/// Sends one introspection query. The outer error is final: the request failed, the credentials were refused, or the
/// answer is too long to read. The inner one is the server's refusal, which the older query may avoid.
async fn attempt(
    http: &Client,
    endpoint: &Endpoint,
    query: &str,
) -> Result<Result<IntrospectedSchema, String>, String> {
    let payload = serde_json::json!({
        "query": format!("{query} {TYPE_REF}"),
        "operationName": "IntrospectionQuery",
    });
    let reply = http.post(endpoint, &payload, endpoint.max_schema_bytes).await?;
    if reply.cut_short {
        // The server did answer. Its answer is only too long, and the fallback query would be no shorter.
        return Err(format!(
            "{} answered the introspection query with HTTP {} and a schema larger than {}, the `max_schema_bytes` \
             limit for this endpoint, so fieldtrip stopped reading it. Ask the user to raise `max_schema_bytes` for \
             this endpoint in the fieldtrip config, or to export the schema to an SDL file and set `schema`.",
            endpoint.shown_url,
            reply.status,
            show_bytes(endpoint.max_schema_bytes)
        ));
    }
    if matches!(reply.status, 401 | 403) {
        return Err(format!(
            "{} answered HTTP {}: it did not accept the credentials. The user has to check the headers configured \
             for this endpoint.",
            endpoint.shown_url, reply.status
        ));
    }
    let Ok(response) = serde_json::from_str::<Response>(&reply.body) else {
        let excerpt: String = reply.body.chars().take(EXCERPT).collect();
        return Ok(Err(format!(
            "HTTP {} with a body that is not a GraphQL response: {excerpt}",
            reply.status
        )));
    };
    if let Some(schema) = response.data.and_then(|data| data.schema) {
        return Ok(Ok(schema));
    }
    let messages: Vec<String> = response.errors.into_iter().take(3).map(|error| error.message).collect();
    Ok(Err(if messages.is_empty() {
        format!("HTTP {} with no schema in the response", reply.status)
    } else {
        format!("HTTP {}: {}", reply.status, messages.join("; "))
    }))
}

/// A GraphQL response to an introspection query.
#[derive(Deserialize)]
struct Response {
    data: Option<Data>,
    #[serde(default)]
    errors: Vec<ResponseError>,
}

/// One entry of a GraphQL response's `errors`.
#[derive(Deserialize)]
struct ResponseError {
    #[serde(default)]
    message: String,
}

/// The `data` of an introspection response.
#[derive(Deserialize)]
struct Data {
    #[serde(rename = "__schema")]
    schema: Option<IntrospectedSchema>,
}

/// `__Schema`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct IntrospectedSchema {
    description: Option<String>,
    query_type: Option<Named>,
    mutation_type: Option<Named>,
    subscription_type: Option<Named>,
    types: Vec<FullType>,
}

/// A reference to a root type.
#[derive(Deserialize)]
struct Named {
    name: String,
}

/// `__Type`, as a definition.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct FullType {
    kind: String,
    name: Option<String>,
    description: Option<String>,
    #[serde(rename = "specifiedByURL")]
    specified_by_url: Option<String>,
    fields: Option<Vec<Field>>,
    input_fields: Option<Vec<InputValue>>,
    interfaces: Option<Vec<TypeRef>>,
    enum_values: Option<Vec<EnumValue>>,
    possible_types: Option<Vec<TypeRef>>,
}

/// `__Field`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Field {
    name: String,
    description: Option<String>,
    #[serde(default)]
    args: Vec<InputValue>,
    #[serde(rename = "type")]
    ty: TypeRef,
    #[serde(default)]
    is_deprecated: bool,
    deprecation_reason: Option<String>,
}

/// `__InputValue`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct InputValue {
    name: String,
    description: Option<String>,
    #[serde(rename = "type")]
    ty: TypeRef,
    default_value: Option<String>,
    #[serde(default)]
    is_deprecated: bool,
    deprecation_reason: Option<String>,
}

/// `__EnumValue`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct EnumValue {
    name: String,
    description: Option<String>,
    #[serde(default)]
    is_deprecated: bool,
    deprecation_reason: Option<String>,
}

/// `__Type`, as a reference: a named type, possibly wrapped in lists and non-nulls.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TypeRef {
    kind: String,
    name: Option<String>,
    of_type: Option<Box<Self>>,
}

impl TypeRef {
    /// The reference in GraphQL syntax, such as `[User!]!`.
    fn render(&self) -> Result<String, String> {
        let inner = || {
            self.of_type
                .as_ref()
                .ok_or_else(|| {
                    "a type in the introspection result is nested deeper than the query asked for".to_owned()
                })
                .and_then(|inner| inner.render())
        };
        match self.kind.as_str() {
            "NON_NULL" => Ok(format!("{}!", inner()?)),
            "LIST" => Ok(format!("[{}]", inner()?)),
            _ => self
                .name
                .clone()
                .ok_or_else(|| "a type in the introspection result has no name".to_owned()),
        }
    }
}

/// Rewrites an introspection result as SDL.
fn to_sdl(schema: &IntrospectedSchema) -> Result<String, String> {
    let mut out = String::new();
    description(&mut out, "", schema.description.as_deref());
    out.push_str("schema {\n");
    for (operation, root) in [
        ("query", &schema.query_type),
        ("mutation", &schema.mutation_type),
        ("subscription", &schema.subscription_type),
    ] {
        if let Some(root) = root {
            let _ = writeln!(out, "  {operation}: {}", root.name);
        }
    }
    out.push_str("}\n");

    for ty in &schema.types {
        let Some(name) = ty.name.as_deref() else { continue };
        if name.starts_with("__") || BUILT_IN_SCALARS.contains(&name) {
            continue;
        }
        out.push('\n');
        description(&mut out, "", ty.description.as_deref());
        match ty.kind.as_str() {
            "SCALAR" => {
                let _ = write!(out, "scalar {name}");
                if let Some(url) = &ty.specified_by_url {
                    let _ = write!(out, " @specifiedBy(url: {})", string(url));
                }
                out.push('\n');
            }
            "OBJECT" | "INTERFACE" => {
                let keyword = if ty.kind == "OBJECT" { "type" } else { "interface" };
                let _ = write!(out, "{keyword} {name}{}", implements(ty.interfaces.as_deref())?);
                fields(&mut out, ty.fields.as_deref().unwrap_or_default())?;
            }
            "UNION" => {
                let members = names(ty.possible_types.as_deref().unwrap_or_default())?;
                let _ = writeln!(out, "union {name} = {}", members.join(" | "));
            }
            "ENUM" => {
                let _ = writeln!(out, "enum {name} {{");
                for value in ty.enum_values.as_deref().unwrap_or_default() {
                    description(&mut out, "  ", value.description.as_deref());
                    let deprecated = deprecation(value.is_deprecated, value.deprecation_reason.as_deref());
                    let _ = writeln!(out, "  {}{deprecated}", value.name);
                }
                out.push_str("}\n");
            }
            "INPUT_OBJECT" => {
                let _ = writeln!(out, "input {name} {{");
                for field in ty.input_fields.as_deref().unwrap_or_default() {
                    description(&mut out, "  ", field.description.as_deref());
                    let _ = writeln!(out, "  {}", input_value(field)?);
                }
                out.push_str("}\n");
            }
            other => return Err(format!("the introspection result has a type of unknown kind `{other}`")),
        }
    }
    Ok(out)
}

/// Writes the body of an object or interface: ` { fields }`, or just a newline when there are none.
fn fields(out: &mut String, fields: &[Field]) -> Result<(), String> {
    if fields.is_empty() {
        out.push('\n');
        return Ok(());
    }
    out.push_str(" {\n");
    for field in fields {
        description(out, "  ", field.description.as_deref());
        let _ = write!(out, "  {}", field.name);
        if !field.args.is_empty() {
            out.push_str("(\n");
            for argument in &field.args {
                description(out, "    ", argument.description.as_deref());
                let _ = writeln!(out, "    {}", input_value(argument)?);
            }
            out.push_str("  )");
        }
        let deprecated = deprecation(field.is_deprecated, field.deprecation_reason.as_deref());
        let _ = writeln!(out, ": {}{deprecated}", field.ty.render()?);
    }
    out.push_str("}\n");
    Ok(())
}

/// `name: Type = default @deprecated`.
fn input_value(value: &InputValue) -> Result<String, String> {
    let mut out = format!("{}: {}", value.name, value.ty.render()?);
    if let Some(default) = &value.default_value {
        let _ = write!(out, " = {default}");
    }
    out.push_str(&deprecation(value.is_deprecated, value.deprecation_reason.as_deref()));
    Ok(out)
}

/// ` implements A & B`, or nothing.
fn implements(interfaces: Option<&[TypeRef]>) -> Result<String, String> {
    let names = names(interfaces.unwrap_or_default())?;
    Ok(if names.is_empty() {
        String::new()
    } else {
        format!(" implements {}", names.join(" & "))
    })
}

/// The names of some type references.
fn names(types: &[TypeRef]) -> Result<Vec<String>, String> {
    types.iter().map(TypeRef::render).collect()
}

/// ` @deprecated(reason: "...")`, ` @deprecated`, or nothing.
fn deprecation(is_deprecated: bool, reason: Option<&str>) -> String {
    match (is_deprecated, reason) {
        (false, _) => String::new(),
        (true, Some(reason)) => format!(" @deprecated(reason: {})", string(reason)),
        (true, None) => " @deprecated".to_owned(),
    }
}

/// Writes a description on its own line, or nothing when there is none.
fn description(out: &mut String, indent: &str, text: Option<&str>) {
    if let Some(text) = text.filter(|text| !text.trim().is_empty()) {
        let _ = writeln!(out, "{indent}{}", string(text));
    }
}

/// `text` as a GraphQL string literal. JSON's escapes are a subset of GraphQL's.
fn string(text: &str) -> String {
    serde_json::Value::String(text.to_owned()).to_string()
}
