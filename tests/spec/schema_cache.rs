//! Slice 38: an introspected schema is kept on disk, so a restart does not introspect again, and a kept copy stands
//! in, clearly labelled, when the endpoint cannot be introspected.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime};

use serde_json::json;
use tempfile::TempDir;

use crate::support::{FixtureApi, Launch, Mcp, MockServer, Response};

/// A mock that serves the fixture schema until `down` is set, and then answers 503.
fn switchable() -> (MockServer, Arc<AtomicBool>) {
    let down = Arc::new(AtomicBool::new(false));
    let switch = Arc::clone(&down);
    let api = FixtureApi::new();
    let mock = MockServer::start(move |request| {
        if switch.load(Ordering::SeqCst) {
            Response::new(503, "Service Unavailable")
        } else {
            api.respond(request)
        }
    });
    (mock, down)
}

/// A config with one introspected endpoint, `live`, at `url`, with `extra` lines added to it.
fn config(url: &str, extra: &str) -> String {
    format!("[endpoints.live]\nurl = \"{url}\"\n{extra}")
}

/// Starts a server with `config` and `cache` as the user's cache directory, shared between servers.
fn start(config: &str, cache: &TempDir) -> Mcp {
    Launch::new(config)
        .env("XDG_CACHE_HOME", &cache.path().display().to_string())
        .start()
}

/// The directory schemas are kept in.
fn schemas(cache: &TempDir) -> PathBuf {
    cache.path().join("fieldtrip").join("schemas")
}

/// Every file in the schema directory.
fn kept(cache: &TempDir) -> Vec<PathBuf> {
    std::fs::read_dir(schemas(cache)).map_or_else(
        |_| Vec::new(),
        |entries| entries.map(|entry| entry.expect("a directory entry").path()).collect(),
    )
}

/// Makes the one kept schema look as if it was written `age` ago.
fn age(cache: &TempDir, age: Duration) {
    let files = kept(cache);
    let [file] = files.as_slice() else {
        panic!("one kept schema, got {files:?}");
    };
    set_modified(file, SystemTime::now() - age);
}

/// Sets the modification time of `path`.
fn set_modified(path: &Path, time: SystemTime) {
    std::fs::File::options()
        .write(true)
        .open(path)
        .and_then(|file| file.set_modified(time))
        .expect("set the modification time");
}

/// Starts a server that introspects `live` once, so the cache holds its schema, and stops it.
fn warm(config: &str, cache: &TempDir) {
    introspect(&mut start(config, cache), false);
}

/// Introspects `live` and returns the text, failing on an error.
fn introspect(mcp: &mut Mcp, refresh: bool) -> String {
    let reply = mcp.call("introspect", &json!({ "endpoint": "live", "refresh": refresh }));
    assert!(!reply.is_error, "introspect should succeed: {}", reply.text);
    reply.text
}

const DAY: Duration = Duration::from_hours(24);

#[test]
fn a_restart_reads_the_schema_from_the_cache() {
    let (mock, _down) = switchable();
    let cache = tempfile::tempdir().expect("a cache directory");
    let config = config(&mock.url(), "");

    let mut first = start(&config, &cache);
    let fresh = introspect(&mut first, false);
    let described = first
        .call("describe", &json!({ "endpoint": "live", "name": "User" }))
        .text;
    drop(first);
    assert_eq!(mock.requests().len(), 1, "the first start introspects");

    let mut second = start(&config, &cache);
    let cached = introspect(&mut second, false);

    assert_eq!(mock.requests().len(), 1, "the second start sends nothing");
    assert!(fresh.contains("loaded by introspection"), "{fresh}");
    assert!(
        cached.contains("fieldtrip's cache"),
        "says where it came from: {cached}"
    );
    assert!(cached.contains("refresh"), "says how to load it again: {cached}");
    assert_eq!(
        second
            .call("describe", &json!({ "endpoint": "live", "name": "User" }))
            .text,
        described,
        "a kept schema is the same schema"
    );
}

#[test]
fn refresh_bypasses_the_cache_and_renews_it() {
    let (mock, _down) = switchable();
    let cache = tempfile::tempdir().expect("a cache directory");
    let config = config(&mock.url(), "");
    warm(&config, &cache);
    age(&cache, Duration::from_hours(1));

    let mut mcp = start(&config, &cache);
    let text = introspect(&mut mcp, true);

    assert_eq!(mock.requests().len(), 2, "refresh introspects");
    assert!(text.contains("loaded by introspection"), "{text}");
    let modified = std::fs::metadata(&kept(&cache)[0])
        .and_then(|meta| meta.modified())
        .expect("mtime");
    assert!(
        modified.elapsed().unwrap_or_default() < Duration::from_secs(60),
        "the kept copy was written again"
    );
}

#[test]
fn a_copy_older_than_a_day_is_not_used() {
    let (mock, _down) = switchable();
    let cache = tempfile::tempdir().expect("a cache directory");
    let config = config(&mock.url(), "");
    warm(&config, &cache);
    age(&cache, DAY + Duration::from_secs(60));

    let mut mcp = start(&config, &cache);
    let text = introspect(&mut mcp, false);

    assert_eq!(mock.requests().len(), 2, "an expired copy is loaded again");
    assert!(text.contains("loaded by introspection"), "{text}");
}

#[test]
fn schema_ttl_sets_how_long_a_copy_is_used_and_can_come_from_a_template() {
    let (mock, _down) = switchable();
    let cache = tempfile::tempdir().expect("a cache directory");
    let config = format!(
        "[templates.acme]\nschema_ttl = 3600\n\n{}",
        config(&mock.url(), "template = \"acme\"\n")
    );
    warm(&config, &cache);
    age(&cache, Duration::from_mins(30));
    warm(&config, &cache);
    assert_eq!(mock.requests().len(), 1, "half an hour old is within an hour");

    age(&cache, Duration::from_hours(2));
    warm(&config, &cache);
    assert_eq!(mock.requests().len(), 2, "two hours old is not");
}

#[test]
fn a_schema_ttl_of_zero_keeps_nothing() {
    let (mock, _down) = switchable();
    let cache = tempfile::tempdir().expect("a cache directory");
    let config = config(&mock.url(), "schema_ttl = 0\n");

    let mut mcp = start(&config, &cache);
    introspect(&mut mcp, false);
    let listed = mcp.call("list_endpoints", &json!({})).text;

    assert!(kept(&cache).is_empty(), "nothing is written: {:?}", kept(&cache));
    assert!(listed.contains("schema: introspection, not cached"), "{listed}");
}

#[test]
fn list_endpoints_says_how_long_a_schema_is_kept() {
    let (mock, _down) = switchable();
    let cache = tempfile::tempdir().expect("a cache directory");
    let mut mcp = start(&config(&mock.url(), ""), &cache);

    let listed = mcp.call("list_endpoints", &json!({})).text;

    assert!(listed.contains("schema: introspection, cached for 1 day"), "{listed}");
}

#[test]
fn an_expired_copy_stands_in_when_the_endpoint_is_down_and_says_so() {
    let (mock, down) = switchable();
    let cache = tempfile::tempdir().expect("a cache directory");
    let config = config(&mock.url(), "");
    warm(&config, &cache);
    age(&cache, 3 * DAY);
    down.store(true, Ordering::SeqCst);

    let mut mcp = start(&config, &cache);
    let text = introspect(&mut mcp, false);

    assert!(text.contains("fieldtrip's cache"), "says where it came from: {text}");
    assert!(text.contains("3 days ago"), "says how old it is: {text}");
    assert!(text.contains("out of date"), "says it may be stale: {text}");
    assert!(text.contains("503"), "says why it was not loaded again: {text}");

    let described = mcp.call("describe", &json!({ "endpoint": "live", "name": "User" }));
    assert!(!described.is_error, "the copy serves every tool: {}", described.text);
    assert!(
        described.text.contains("out of date"),
        "every tool that reads the copy says so: {}",
        described.text
    );
    let searched = mcp.call("search_schema", &json!({ "endpoint": "live", "query": "user" }));
    assert!(searched.text.contains("out of date"), "{}", searched.text);
}

#[test]
fn refresh_does_not_fall_back_to_the_copy() {
    let (mock, down) = switchable();
    let cache = tempfile::tempdir().expect("a cache directory");
    let config = config(&mock.url(), "");
    warm(&config, &cache);
    down.store(true, Ordering::SeqCst);

    let mut mcp = start(&config, &cache);
    let reply = mcp.call("introspect", &json!({ "endpoint": "live", "refresh": true }));

    assert!(reply.is_error, "a refresh that fails is an error: {}", reply.text);
    assert!(reply.text.contains("503"), "{}", reply.text);
}

#[test]
fn with_no_copy_a_failed_introspection_is_still_an_error() {
    let (mock, down) = switchable();
    down.store(true, Ordering::SeqCst);
    let cache = tempfile::tempdir().expect("a cache directory");

    let mut mcp = start(&config(&mock.url(), ""), &cache);
    let reply = mcp.call("introspect", &json!({ "endpoint": "live" }));

    assert!(reply.is_error, "{}", reply.text);
}

#[test]
fn an_endpoint_with_a_schema_file_keeps_nothing() {
    let cache = tempfile::tempdir().expect("a cache directory");
    let mut mcp = start(
        &config("http://localhost:9/graphql", "schema = \"schema.graphql\"\n"),
        &cache,
    );

    introspect(&mut mcp, false);

    assert!(kept(&cache).is_empty(), "{:?}", kept(&cache));
}

#[test]
fn a_kept_schema_does_not_show_the_url_key_and_only_the_user_can_read_it() {
    let (mock, _down) = switchable();
    let cache = tempfile::tempdir().expect("a cache directory");
    let url = format!("{}?key=sekrit-url-key", mock.url());
    let mut mcp = start(&config(&url, ""), &cache);

    introspect(&mut mcp, false);

    let files = kept(&cache);
    let [file] = files.as_slice() else {
        panic!("one kept schema, got {files:?}");
    };
    let text = std::fs::read_to_string(file).expect("read the kept schema");
    assert!(!file.display().to_string().contains("sekrit"), "{}", file.display());
    assert!(!text.contains("sekrit"), "the key is not written down");
    assert!(text.contains("type User"), "it is SDL");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = |path: &Path| std::fs::metadata(path).expect("metadata").permissions().mode() & 0o777;
        assert_eq!(mode(file), 0o600, "only the user can read the file");
        assert_eq!(mode(&schemas(&cache)), 0o700, "or list the directory");
    }
}
