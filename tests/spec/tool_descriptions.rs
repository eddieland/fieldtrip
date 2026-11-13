//! Slice 21: the tool list and the server instructions tell a calling agent how to use GraphQL well.
//!
//! Descriptions are prompts, so their exact wording is not pinned here. What is pinned is what a client and a model
//! rely on: every tool and argument is described, the annotations say which tools change nothing, and the text
//! points at the workflow and the habits that keep an operation valid, cheap and readable to the human approving it.

use serde_json::Value;

use crate::support::Mcp;

const CONFIG: &str = r#"
[endpoints.acme]
url    = "http://localhost:9/graphql"
schema = "schema.graphql"
"#;

/// The tools in the order a calling agent uses them.
const WORKFLOW: [&str; 6] = [
    "list_endpoints",
    "introspect",
    "search_schema",
    "describe",
    "estimate_cost",
    "execute",
];

/// The tool called `name` in `tools`.
fn tool<'list>(tools: &'list [Value], name: &str) -> &'list Value {
    tools
        .iter()
        .find(|tool| tool.get("name").and_then(Value::as_str) == Some(name))
        .unwrap_or_else(|| panic!("{name} should be in tools/list"))
}

/// The string at `pointer` in `value`, or an empty string.
fn text<'value>(value: &'value Value, pointer: &str) -> &'value str {
    value.pointer(pointer).and_then(Value::as_str).unwrap_or_default()
}

/// The boolean annotation `hint` of `tool`.
fn hint(tool: &Value, hint: &str) -> Option<bool> {
    tool.pointer(&format!("/annotations/{hint}")).and_then(Value::as_bool)
}

#[test]
fn every_tool_and_argument_is_described_and_titled() {
    let mut mcp = Mcp::start(CONFIG);
    let tools = mcp.tool_list();

    assert_eq!(tools.len(), WORKFLOW.len(), "unexpected tools: {tools:#?}");
    for name in WORKFLOW {
        let tool = tool(&tools, name);
        assert!(!text(tool, "/title").is_empty(), "{name} should have a title");
        assert!(
            text(tool, "/description").len() > 200,
            "{name} should say when to use it and what it returns: {tool}"
        );
        let properties = tool
            .pointer("/inputSchema/properties")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        for (argument, schema) in properties {
            assert!(
                text(&schema, "/description").len() > 20,
                "{name}.{argument} should be described: {schema}"
            );
        }
    }
}

#[test]
fn annotations_say_only_execute_can_change_anything() {
    let mut mcp = Mcp::start(CONFIG);
    let tools = mcp.tool_list();

    for name in WORKFLOW.iter().filter(|name| **name != "execute") {
        assert_eq!(
            hint(tool(&tools, name), "readOnlyHint"),
            Some(true),
            "{name} is read-only"
        );
    }
    let execute = tool(&tools, "execute");
    assert_eq!(hint(execute, "readOnlyHint"), Some(false), "execute can send mutations");
    assert_eq!(
        hint(execute, "destructiveHint"),
        Some(true),
        "a mutation can delete or overwrite"
    );
    assert_eq!(
        hint(execute, "idempotentHint"),
        Some(false),
        "sending a mutation twice can act twice"
    );
    assert_eq!(
        hint(execute, "openWorldHint"),
        Some(true),
        "execute reaches the endpoint"
    );
    assert_eq!(
        hint(tool(&tools, "list_endpoints"), "openWorldHint"),
        Some(false),
        "list_endpoints only reads the config"
    );
}

#[test]
fn the_instructions_lay_out_the_workflow_in_order() {
    let mcp = Mcp::start(CONFIG);
    let instructions = mcp.instructions().expect("the server gives instructions").to_owned();

    let mut last = 0;
    for name in WORKFLOW {
        let at = instructions
            .find(&format!("`{name}`"))
            .unwrap_or_else(|| panic!("the instructions should name `{name}`:\n{instructions}"));
        assert!(
            at >= last,
            "`{name}` is out of order in the instructions:\n{instructions}"
        );
        last = at;
    }
    for habit in ["variables", "deprecated", "`first`"] {
        assert!(
            instructions.contains(habit),
            "the instructions should mention {habit}:\n{instructions}"
        );
    }
}

#[test]
fn descriptions_carry_the_graphql_habits_where_they_apply() {
    let mut mcp = Mcp::start(CONFIG);
    let tools = mcp.tool_list();
    let description = |name: &str| text(tool(&tools, name), "/description").to_owned();

    let expectations: [(&str, &[&str]); 5] = [
        ("introspect", &["`search_schema`", "`describe`"]),
        ("search_schema", &["`describe`"]),
        ("describe", &["deprecated", "... on"]),
        ("estimate_cost", &["`execute`", "`first`", "without sending"]),
        ("execute", &["`estimate_cost`", "variables", "`errors`", "declines"]),
    ];
    for (name, phrases) in expectations {
        let description = description(name);
        for phrase in phrases {
            assert!(
                description.contains(phrase),
                "{name} should mention {phrase}:\n{description}"
            );
        }
    }

    let operation = text(tool(&tools, "execute"), "/inputSchema/properties/operation/description");
    assert!(
        operation.contains('$'),
        "the operation argument should show a declared variable: {operation}"
    );
    let variables = text(tool(&tools, "execute"), "/inputSchema/properties/variables/description");
    assert!(
        variables.contains("enum"),
        "the variables argument should say how enums are written in JSON: {variables}"
    );
}
