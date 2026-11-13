//! Slice 2: `describe` returns compact SDL for one type or one field of the fixture schema.

use serde_json::json;

use crate::support::Mcp;

const CONFIG: &str = r#"
[endpoints.store]
url    = "http://localhost:9/graphql"
schema = "schema.graphql"
"#;

/// Calls `describe` for `name` on the fixture endpoint and returns the text, failing on a tool error.
fn describe(name: &str) -> String {
    let mut mcp = Mcp::start(CONFIG);
    let reply = mcp.call("describe", &json!({ "endpoint": "store", "name": name }));
    assert!(!reply.is_error, "describe {name} should succeed: {}", reply.text);
    reply.text
}

/// Calls `describe` and returns the error text, failing if the tool succeeded.
fn describe_error(arguments: &serde_json::Value) -> String {
    let mut mcp = Mcp::start(CONFIG);
    let reply = mcp.call("describe", arguments);
    assert!(reply.is_error, "describe {arguments} should fail, got: {}", reply.text);
    reply.text
}

#[test]
fn an_object_type_is_compact_sdl() {
    let expected = "\
\"A person with an account. Customers and staff are both users.\"
type User implements Node {
  id: ID!
  \"The name shown on the profile.\"
  name: String!
  email: String
  role: Role!
  \"Orders placed by this user.\"
  orders(first: Int, last: Int): [Order!]!
  nickname: String @deprecated(reason: \"Use name.\")
}
";
    assert_eq!(
        describe("User"),
        expected,
        "descriptions are one line, fields keep their order"
    );
}

#[test]
fn an_interface_lists_its_implementers() {
    let expected = "\
\"Anything that can be fetched by ID.\"
interface Node {
  id: ID!
}
# implemented by: User, Product, Order
";
    assert_eq!(describe("Node"), expected, "an interface says which types implement it");
}

#[test]
fn a_union_an_enum_an_input_and_a_scalar() {
    assert_eq!(
        describe("SearchResult"),
        "union SearchResult = User | Product\n",
        "union"
    );
    assert_eq!(
        describe("OrderStatus"),
        "enum OrderStatus {\n  PENDING\n  PAID\n  SHIPPED\n  CANCELLED\n  \
         REFUNDED @deprecated(reason: \"Refunds are tracked on payments now.\")\n}\n",
        "enum"
    );
    assert_eq!(
        describe("CreateOrderInput"),
        "input CreateOrderInput {\n  items: [OrderItemInput!]!\n  \"Free-text note for the warehouse.\"\n  \
         note: String\n  currency: Currency = USD\n}\n",
        "input object, with its default value"
    );
    assert_eq!(
        describe("DateTime"),
        "\"An ISO 8601 timestamp.\"\nscalar DateTime\n",
        "scalar"
    );
}

#[test]
fn a_field_comes_with_the_types_needed_to_use_it() {
    let expected = "\
type Mutation {
  \"Create an order for the signed-in user.\"
  createOrder(input: CreateOrderInput!): Order!
}

input CreateOrderInput {
  items: [OrderItemInput!]!
  \"Free-text note for the warehouse.\"
  note: String
  currency: Currency = USD
}

type Order implements Node {
  id: ID!
  status: OrderStatus!
  placedAt: DateTime!
  items: [OrderItem!]!
  total: Money!
  customer: User!
}
";
    assert_eq!(
        describe("Mutation.createOrder"),
        expected,
        "a field is shown inside its parent type, followed by its argument types and its return type"
    );
}

#[test]
fn a_field_spells_out_documented_arguments() {
    let text = describe("Query.users");

    let expected_field = "\
type Query {
  \"Users, newest first.\"
  users(
    \"How many users to return.\"
    first: Int = 20
    after: String
    role: Role
  ): UserConnection!
}
";
    assert!(
        text.starts_with(expected_field),
        "arguments with descriptions go one per line, got:\n{text}"
    );
    assert!(
        text.contains("enum Role {"),
        "the enum argument type is included:\n{text}"
    );
    assert!(
        text.contains("type UserConnection {"),
        "the return type is included:\n{text}"
    );
}

#[test]
fn an_unknown_type_suggests_the_nearest_names() {
    let error = describe_error(&json!({ "endpoint": "store", "name": "Usr" }));

    assert!(error.contains("no type named `Usr`"), "says what is wrong: {error}");
    assert!(error.contains("`User`"), "suggests the nearest type: {error}");
}

#[test]
fn an_unknown_field_suggests_the_nearest_fields() {
    let error = describe_error(&json!({ "endpoint": "store", "name": "Query.usr" }));

    assert!(
        error.contains("`Query` has no field `usr`"),
        "says what is wrong: {error}"
    );
    assert!(
        error.contains("`user`") && error.contains("`users`"),
        "suggests the nearest fields: {error}"
    );
}

#[test]
fn a_bare_root_field_name_points_at_the_qualified_one() {
    let error = describe_error(&json!({ "endpoint": "store", "name": "createOrder" }));

    assert!(
        error.contains("`Mutation.createOrder`"),
        "points at the qualified name: {error}"
    );
}

#[test]
fn an_unknown_endpoint_lists_the_configured_ones() {
    let error = describe_error(&json!({ "endpoint": "shop", "name": "User" }));

    assert!(
        error.contains("no endpoint named `shop`"),
        "says what is wrong: {error}"
    );
    assert!(error.contains("store"), "lists the endpoints that exist: {error}");
}
