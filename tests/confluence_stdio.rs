//! stdio-transport acceptance: the `conseqa-confluence stdio` command
//! serves multi-project MCP over stdin/stdout, the transport Claude
//! Desktop / Code accepts for a local server without TLS. Spawns the
//! real binary and speaks newline-delimited JSON-RPC to it, so the
//! whole path — no session id, no bearer, project tools, a commit — is
//! exercised as the UI would.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::Duration;

struct StdioClient {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl StdioClient {
    fn spawn(data_dir: &std::path::Path) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_conseqa-confluence"))
            .arg("stdio")
            .arg("--data-dir")
            .arg(data_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("the stdio server launches");

        let stdin = child.stdin.take().expect("stdin");
        let stdout = BufReader::new(child.stdout.take().expect("stdout"));

        let mut client = Self {
            child,
            stdin,
            stdout,
            next_id: 1,
        };

        let init = client.request(
            0,
            "initialize",
            serde_json::json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "stdio-test", "version": "0"},
            }),
        );

        assert!(init.get("result").is_some(), "initialize: {init}");

        client.notify("notifications/initialized", serde_json::json!({}));

        client
    }

    fn send(&mut self, message: serde_json::Value) {
        writeln!(self.stdin, "{message}").expect("write");
        self.stdin.flush().expect("flush");
    }

    fn notify(&mut self, method: &str, params: serde_json::Value) {
        self.send(serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        }));
    }

    /// Sends a request and reads until the matching JSON-RPC response.
    fn request(&mut self, id: u64, method: &str, params: serde_json::Value) -> serde_json::Value {
        self.send(serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        }));

        loop {
            let mut line = String::new();

            let read = self.stdout.read_line(&mut line).expect("read");
            assert_ne!(read, 0, "the server closed stdout before responding");

            let line = line.trim();

            if line.is_empty() {
                continue;
            }

            if let Ok(message) = serde_json::from_str::<serde_json::Value>(line)
                && message.get("id").and_then(|value| value.as_u64()) == Some(id)
            {
                return message;
            }
        }
    }

    /// Calls a tool, returning (parsed payload, is_error).
    fn call(&mut self, name: &str, arguments: serde_json::Value) -> (serde_json::Value, bool) {
        let id = self.next_id;
        self.next_id += 1;

        let response = self.request(
            id,
            "tools/call",
            serde_json::json!({"name": name, "arguments": arguments}),
        );

        let result = &response["result"];
        let is_error = result["isError"].as_bool().unwrap_or(false);
        let text = result["content"][0]["text"].as_str().unwrap_or("");

        let payload = serde_json::from_str(text).unwrap_or(serde_json::Value::Null);

        (payload, is_error)
    }
}

impl Drop for StdioClient {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn stdio_server_hosts_projects_without_a_session_id_or_bearer() {
    let dir = std::env::temp_dir().join(format!("conseqa-stdio-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).expect("data dir");

    // A watchdog: fail loudly rather than hang if the protocol stalls.
    let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    {
        let done = std::sync::Arc::clone(&done);
        std::thread::spawn(move || {
            for _ in 0..300 {
                if done.load(std::sync::atomic::Ordering::SeqCst) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            eprintln!("stdio test exceeded 30s; aborting");
            std::process::abort();
        });
    }

    let mut client = StdioClient::spawn(&dir);

    // No project active yet: architecture tools guide rather than fail.
    let (early, is_error) = client.call("task_context", serde_json::json!({}));
    assert!(is_error);
    assert!(
        early["error"].as_str().unwrap_or_default().contains("no project is open"),
        "{early}"
    );

    // list_projects works with no Mcp-Session-Id and no bearer — the
    // exact conditions that broke the http multi-project server.
    let (list, is_error) = client.call("list_projects", serde_json::json!({}));
    assert!(!is_error, "{list}");
    assert_eq!(list["projects"].as_array().map(Vec::len), Some(0));

    // Create a project, grounded by its prompt, and commit into it.
    let (created, is_error) = client.call(
        "create_project",
        serde_json::json!({"project": "todo", "prompt": "A todo service."}),
    );
    assert!(!is_error, "{created}");
    assert_eq!(created["created"], true);

    let (context, _) = client.call("task_context", serde_json::json!({}));
    assert_eq!(context["prompt_evidence"][0]["excerpt"], "A todo service.");

    let (committed, is_error) = client.call(
        "submit_patch",
        serde_json::json!({"patch": {"mutations": [
            {"kind": "put_service", "id": "service.todo", "value": {"kind": "backend"}}
        ]}}),
    );
    assert!(!is_error, "{committed}");
    assert_eq!(committed["committed"], true);

    let (search, _) = client.call("search_symbols", serde_json::json!({"kind": "service"}));
    let services: Vec<&str> = search["symbols"]
        .as_array()
        .expect("symbols")
        .iter()
        .filter_map(|symbol| symbol["value"].as_str())
        .collect();
    assert!(services.contains(&"service.todo"), "{search}");

    done.store(true, std::sync::atomic::Ordering::SeqCst);

    drop(client);
    std::fs::remove_dir_all(&dir).ok();
}
