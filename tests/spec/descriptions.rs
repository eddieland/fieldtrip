//! Slice 25: an endpoint can say what it is for, and `list_endpoints` shows it.

use serde_json::json;

use crate::support::Mcp;

#[test]
fn list_endpoints_shows_the_description_on_one_line() {
    let mut mcp = Mcp::start(
        "[endpoints.store-prod]\nurl = \"https://api.store.example/graphql\"\n\
         description = \"\"\"\nProduction store.\n  Real customers: read freely, write only when asked.\n\"\"\"\n\n\
         [endpoints.store-dev]\nurl = \"https://dev.store.example/graphql\"\n",
    );

    let reply = mcp.call("list_endpoints", &json!({}));

    assert_eq!(
        reply.text,
        "store-dev\n  \
           url: https://dev.store.example/graphql\n  \
           schema: introspection, cached for 1 day\n  \
           approval: the user is asked before every operation\n\
         store-prod\n  \
           description: Production store. Real customers: read freely, write only when asked.\n  \
           url: https://api.store.example/graphql\n  \
           schema: introspection, cached for 1 day\n  \
           approval: the user is asked before every operation\n",
        "the description comes first, with its whitespace collapsed"
    );
}
