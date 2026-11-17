//! Slice 56: a response file is deleted `result_ttl` after it was written, the audit log names the file of each
//! attempt, and `history` lists the files that are still there.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde_json::{Value, json};

use crate::support::{Launch, Mcp, MockServer, Reply};

/// The config of a `store` endpoint that answers from `mock` and never prompts, after the top-level `settings`.
fn config(mock: &MockServer, settings: &str) -> String {
    format!(
        "{settings}\n[endpoints.store]\nurl = \"{}\"\nschema = \"schema.graphql\"\napproval = \"client\"\n",
        mock.url()
    )
}

/// A mock whose every response is long enough to go to a file.
fn mock() -> MockServer {
    let products: Vec<Value> = (0..200)
        .map(|index| json!({ "id": format!("p-{index}"), "title": "t".repeat(100) }))
        .collect();
    MockServer::answering(&json!({ "data": { "products": products } }))
}

/// Executes a query whose response goes to a file, and returns the file.
fn execute(mcp: &mut Mcp) -> PathBuf {
    let reply = mcp.call(
        "execute",
        &json!({ "endpoint": "store", "operation": "query Products { products { id title } }" }),
    );
    reply
        .text
        .lines()
        .find_map(|line| line.strip_prefix("Saved the whole response, "))
        .and_then(|line| line.split_once(" bytes, to "))
        .map_or_else(
            || panic!("the response went to a file: {}", reply.text),
            |(_, path)| PathBuf::from(path),
        )
}

/// The ID of the result in `file`.
fn id(file: &Path) -> String {
    file.file_stem()
        .and_then(|stem| stem.to_str())
        .expect("a file name")
        .to_owned()
}

/// Makes `file` look as if it was written `age` ago.
fn aged(file: &Path, age: Duration) {
    let written = SystemTime::now().checked_sub(age).expect("a time in the past");
    std::fs::File::options()
        .write(true)
        .open(file)
        .and_then(|opened| opened.set_modified(written))
        .expect("set the time the file was written");
}

const HOUR: Duration = Duration::from_secs(3_600);

#[test]
fn a_file_older_than_a_day_is_deleted_when_the_next_is_written() {
    let mock = mock();
    let mut mcp = Launch::new(&config(&mock, "")).start();
    let old = execute(&mut mcp);
    let recent = execute(&mut mcp);
    aged(&old, 25 * HOUR);
    aged(&recent, 23 * HOUR);

    let new = execute(&mut mcp);

    assert!(!old.exists(), "a file is deleted a day after it was written");
    assert!(recent.is_file() && new.is_file(), "a newer one is kept");
}

#[test]
fn result_ttl_sets_how_long_a_file_is_kept() {
    let mock = mock();
    let mut mcp = Launch::new(&config(&mock, "result_ttl = 600\n")).start();
    let old = execute(&mut mcp);
    let recent = execute(&mut mcp);
    aged(&old, Duration::from_secs(700));
    aged(&recent, Duration::from_secs(500));

    let read = mcp.call("read_result", &json!({ "result": id(&old), "select": "data" }));

    assert!(
        !old.exists(),
        "reading a result deletes the ones that are too old first"
    );
    assert!(recent.is_file(), "ten minutes have not passed for this one");
    assert!(read.is_error, "{}", read.text);
    assert!(
        read.text.contains("`result_ttl`") && read.text.contains("10 min"),
        "the error says a result is deleted after a while: {}",
        read.text
    );
}

#[test]
fn files_of_earlier_sessions_are_deleted_at_startup() {
    let mock = mock();
    let mut earlier = Launch::new(&config(&mock, "")).start();
    let old = execute(&mut earlier);
    let kept = execute(&mut earlier);
    let responses = old.parent().and_then(Path::parent).expect("the responses directory");
    let other_session = responses.join("0123456789abcdef");
    std::fs::create_dir(&other_session).expect("create a directory");
    let lone = other_session.join("store-000000000000.json");
    let legacy = responses.join("store-1700000000000-1-0.json");
    let notes = responses.join("notes.txt");
    for file in [&lone, &legacy, &notes] {
        std::fs::write(file, "{}").expect("write a file");
    }
    for file in [&old, &lone, &legacy, &notes] {
        aged(file, 48 * HOUR);
    }
    let cache = earlier.dir().join("cache-home");

    let mut later = Launch::new(&config(&mock, ""))
        .env("XDG_CACHE_HOME", &cache.display().to_string())
        .start();
    let listed = later.call("list_endpoints", &json!({}));

    assert!(!listed.is_error, "{}", listed.text);
    assert!(!old.exists(), "an old file of an earlier session is deleted");
    assert!(kept.is_file(), "a recent one is kept, whichever session wrote it");
    assert!(
        !legacy.exists(),
        "so is one that `save` wrote before sessions had directories"
    );
    assert!(!other_session.exists(), "a session's directory goes with its last file");
    assert!(notes.is_file(), "a file fieldtrip did not write is left alone");
    assert_eq!(mock.requests().len(), 2, "startup sends nothing");
}

#[test]
fn a_result_ttl_of_zero_stops_the_server() {
    let stderr = Launch::new(
        "result_ttl = 0\n\n[endpoints.store]\nurl = \"http://localhost:9/graphql\"\nschema = \"schema.graphql\"\n",
    )
    .fails_to_start();

    assert!(
        stderr.contains("`result_ttl`") && stderr.contains("at least 1"),
        "{stderr}"
    );
}

#[test]
fn check_shows_where_results_go_and_how_long_they_are_kept() {
    let endpoint = "[endpoints.store]\nurl = \"http://localhost:9/graphql\"\nschema = \"schema.graphql\"\n";

    let default = Launch::new(endpoint).check();
    let set = Launch::new(&format!("result_ttl = 7200\n\n{endpoint}")).check();

    let responses = default
        .dir
        .path()
        .join("cache-home")
        .join("fieldtrip")
        .join("responses");
    assert!(
        default
            .stdout
            .contains(&format!("Result files: {}, deleted after 1 day\n", responses.display())),
        "{}",
        default.stdout
    );
    assert!(set.stdout.contains(", deleted after 2 h\n"), "{}", set.stdout);
    assert!(!responses.exists(), "check creates nothing");
}

/// The JSON lines of a `history` result, parsed.
fn entries(reply: &Reply) -> Vec<Value> {
    reply
        .text
        .lines()
        .filter(|line| line.starts_with('{'))
        .map(|line| serde_json::from_str(line).unwrap_or_else(|error| panic!("{error}: {line}")))
        .collect()
}

#[test]
fn the_audit_log_names_the_result_and_history_lists_the_files_that_exist() {
    let small = MockServer::answering(&json!({ "data": { "products": [] } }));
    let large = mock();
    let settings = format!(
        "audit_log = \"audit.jsonl\"\n\n[endpoints.small]\nurl = \"{}\"\nschema = \"schema.graphql\"\n\
         approval = \"client\"\n",
        small.url()
    );
    let mut mcp = Launch::new(&config(&large, &settings)).start();
    let first = execute(&mut mcp);
    let second = execute(&mut mcp);
    let inline = mcp.call(
        "execute",
        &json!({ "endpoint": "small", "operation": "{ products { id } }" }),
    );
    assert!(!inline.text.contains("Saved"), "{}", inline.text);

    let before = mcp.call("history", &json!({}));
    std::fs::remove_file(&first).expect("delete a result file");
    let after = mcp.call("history", &json!({}));

    let results: Vec<Value> = entries(&before).iter().map(|entry| entry["result"].clone()).collect();
    assert_eq!(
        results,
        [json!(id(&first)), json!(id(&second)), Value::Null],
        "a line names the result when there is a file: {}",
        before.text
    );
    let listed = |reply: &Reply, file: &Path| reply.text.contains(&format!("`{}` in {}", id(file), file.display()));
    assert!(listed(&before, &first) && listed(&before, &second), "{}", before.text);
    assert!(before.text.contains("`read_result`"), "{}", before.text);
    assert!(
        !listed(&after, &first) && listed(&after, &second),
        "a file is listed only while it exists: {}",
        after.text
    );
    assert!(
        after.text.contains("1 other has been deleted"),
        "the result says a file is gone: {}",
        after.text
    );
    assert_eq!(
        entries(&after),
        entries(&before),
        "the lines are the log's, whatever became of the files"
    );
}

#[test]
fn history_has_no_note_when_no_attempt_has_a_file() {
    let small = MockServer::answering(&json!({ "data": { "products": [] } }));
    let mut mcp = Launch::new(&config(&small, "audit_log = \"audit.jsonl\"\n")).start();
    mcp.call(
        "execute",
        &json!({ "endpoint": "store", "operation": "{ products { id } }" }),
    );

    let reply = mcp.call("history", &json!({}));

    assert_eq!(entries(&reply).len(), 1, "{}", reply.text);
    assert!(!reply.text.contains("read_result"), "{}", reply.text);
}
