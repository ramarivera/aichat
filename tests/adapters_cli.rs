use serde_json::{json, Value};
use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        let root = std::env::var_os("HOME").map(PathBuf::from).unwrap();
        let path = root
            .join("dev/agentic-scratchpads/aichat/adapter-cli-tests")
            .join(uuid::Uuid::new_v4().to_string());
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_rr-aichat"));
        // Inherit no account or application configuration into fixture processes.
        command
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", std::env::var_os("HOME").unwrap())
            .env("RR_AICHAT_CONFIG_DIR", &self.0)
            .env("RR_AICHAT_ENV_FILE", self.0.join("absent.env"))
            .env("RR_AICHAT_LOG_LEVEL", "off")
            .stdin(Stdio::null());
        command
    }

    fn config(&self, base: &str) {
        fs::write(self.0.join("config.yaml"), format!(
            "model: native:configured\nsave: false\nsave_session: true\nfunction_calling: false\nclients:\n  - type: openai-responses\n    name: native\n    api_base: {base}\n    api_key: fixture-only\n  - type: codex-app-server\n    name: subscription\n  - type: openai\n    name: legacy\n    api_key: fixture-only\n    models:\n      - name: listed\n"
        )).unwrap();
    }

    #[cfg(unix)]
    fn codex_config(&self, executable: &std::path::Path, model: &str) {
        let executable = serde_json::to_string(&executable.display().to_string()).unwrap();
        fs::write(
            self.0.join("config.yaml"),
            format!(
                "model: subscription:{model}\nsave: false\nsave_session: true\nfunction_calling: false\nclients:\n  - type: codex-app-server\n    name: subscription\n    executable: {executable}\n    models:\n      - name: gpt-6.1-sol\n      - name: gpt-test-configured\n      - name: gpt-test-env\n      - name: gpt-test-cli\n"
            ),
        )
        .unwrap();
    }

    #[cfg(unix)]
    fn fake_codex(&self) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;

        let executable = self.0.join("fake-codex.py");
        fs::write(&executable, FAKE_CODEX_APP_SERVER).unwrap();
        let mut permissions = fs::metadata(&executable).unwrap().permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(&executable, permissions).unwrap();
        executable
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

// One response, one captured real HTTP request. No external service or account.
fn endpoint(text: String) -> (String, thread::JoinHandle<Value>) {
    endpoint_status(text, "completed")
}

fn endpoint_status(
    text: String,
    terminal_status: &'static str,
) -> (String, thread::JoinHandle<Value>) {
    endpoint_protocol(text, terminal_status, true)
}

fn endpoint_protocol(
    text: String,
    terminal_status: &'static str,
    responses: bool,
) -> (String, thread::JoinHandle<Value>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}/v1", listener.local_addr().unwrap());
    listener.set_nonblocking(true).unwrap();
    let worker = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(15);
        let mut socket = loop {
            match listener.accept() {
                Ok((socket, _)) => break socket,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(Instant::now() < deadline, "client never reached fixture");
                    thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("accept: {error}"),
            }
        };
        socket
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let mut request = Vec::new();
        let (header_end, length) = loop {
            let mut chunk = [0; 4096];
            let count = socket.read(&mut chunk).unwrap();
            assert_ne!(count, 0, "EOF while reading request headers");
            request.extend_from_slice(&chunk[..count]);
            if let Some(end) = request.windows(4).position(|v| v == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&request[..end]);
                let path = if responses {
                    "/v1/responses"
                } else {
                    "/v1/chat/completions"
                };
                assert!(headers.starts_with(&format!("POST {path} ")), "{headers}");
                assert!(
                    headers.lines().any(|line| {
                        line.split_once(':').is_some_and(|(name, value)| {
                            name.eq_ignore_ascii_case("authorization")
                                && value.trim() == "Bearer fixture-only"
                        })
                    }),
                    "configured fixture credential wasn't sent"
                );
                let length = headers
                    .lines()
                    .find_map(|line| {
                        let (key, value) = line.split_once(':')?;
                        key.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().unwrap())
                    })
                    .unwrap();
                break (end + 4, length);
            }
        };
        while request.len() < header_end + length {
            let mut chunk = [0; 4096];
            let count = socket.read(&mut chunk).unwrap();
            assert_ne!(count, 0, "EOF while reading request body");
            request.extend_from_slice(&chunk[..count]);
        }
        let body: Value =
            serde_json::from_slice(&request[header_end..header_end + length]).unwrap();
        let response = if responses {
            json!({"id":"resp_cli", "object":"response", "status":terminal_status,
                "incomplete_details": {"reason":"max_output_tokens"}, "output":[
                {"id":"msg_cli", "type":"message", "role":"assistant", "status":"completed", "content":[
                    {"type":"output_text", "text":text, "annotations":[]}
                ]}
            ],"usage":{"input_tokens":12,"output_tokens":4}})
        } else {
            json!({"id":"chatcmpl_legacy","choices":[{"index":0,"message":{"role":"assistant","content":text},"finish_reason":"stop"}],
                "usage":{"prompt_tokens":12,"completion_tokens":4}})
        };
        let streaming = body["stream"].as_bool().unwrap_or_default();
        let (content_type, payload) = if streaming && !responses {
            let delta = json!({"id":"chatcmpl_legacy","choices":[{"index":0,"delta":{"role":"assistant","content":text}}]});
            let done = json!({"id":"chatcmpl_legacy","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]});
            (
                "text/event-stream",
                format!("data: {delta}\n\ndata: {done}\n\ndata: [DONE]\n\n"),
            )
        } else if streaming {
            let mut payload = String::new();
            let events = [
                json!({"type":"response.created", "response":{"id":"resp_cli","status":"in_progress"}}),
                json!({"type":"response.output_item.added", "output_index":0,"item":{"id":"msg_cli","type":"message","role":"assistant","status":"in_progress","content":[]}}),
                json!({"type":"response.content_part.added", "item_id":"msg_cli","output_index":0,"content_index":0,"part":{"type":"output_text","text":"","annotations":[]}}),
                json!({"type":"response.output_text.delta", "item_id":"msg_cli","output_index":0,"content_index":0,"delta":text}),
                json!({"type":"response.output_text.done", "item_id":"msg_cli","output_index":0,"content_index":0,"text":text}),
                json!({"type":"response.output_item.done", "output_index":0,"item":response["output"][0]}),
                json!({"type":format!("response.{terminal_status}"), "response":response}),
            ];
            for event in events {
                payload.push_str(&format!(
                    "event: {}\ndata: {event}\n\n",
                    event["type"].as_str().unwrap()
                ));
            }
            ("text/event-stream", payload)
        } else {
            ("application/json", response.to_string())
        };
        write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", payload.len()).unwrap();
        // Split events and Unicode code points across transport chunks.
        for bytes in payload.as_bytes().chunks(7) {
            socket.write_all(bytes).unwrap();
        }
        body
    });
    (base, worker)
}

#[cfg(unix)]
const FAKE_CODEX_APP_SERVER: &str = r#"#!/usr/bin/env python3
import json
import os
import sys

log_path = os.environ["RR_TEST_CODEX_LOG"]
mode = os.environ.get("RR_TEST_CODEX_MODE", "ok")
reply = os.environ.get("RR_TEST_CODEX_REPLY", "fixture reply")
thread_id = "thread-" + str(os.getpid())
turn_id = "turn-" + str(os.getpid())

def record(value):
    with open(log_path, "a", encoding="utf-8") as log:
        log.write(json.dumps(value, ensure_ascii=False) + "\n")

def send(value):
    sys.stdout.write(json.dumps(value, ensure_ascii=False) + "\n")
    sys.stdout.flush()

record({"_process_start": True, "pid": os.getpid()})
for line in sys.stdin:
    request = json.loads(line)
    record(request)
    method = request.get("method")
    if method == "initialize":
        send({"id": request["id"], "result": {}})
    elif method == "initialized":
        pass
    elif method == "account/read":
        send({"id": request["id"], "result": {"account": {"type": "chatgpt"}}})
    elif method == "config/read":
        send({"id": request["id"], "result": {"config": {
            "model_provider": "openai",
            "mcp_servers": {"fixture-mcp": {"enabled": True}},
            "plugins": {"fixture-plugin": {"enabled": True}}
        }}})
    elif method == "model/list":
        send({"id": request["id"], "result": {
            "data": [{"id": model, "model": model,
                      "supportedReasoningEfforts": [{"reasoningEffort": "low"}, {"reasoningEffort": "high"}]} for model in
                     ["gpt-6.1-sol", "gpt-test-configured", "gpt-test-env", "gpt-test-cli"]],
            "nextCursor": None
        }})
    elif method == "thread/start":
        record({"_thread_id": thread_id})
        send({"id": request["id"], "result": {"thread": {"id": thread_id}}})
    elif method == "turn/start":
        send({"id": request["id"], "result": {"turn": {"id": turn_id, "status": "inProgress"}}})
        if mode == "approval":
            send({"id": "approval-1", "method": "item/commandExecution/requestApproval",
                  "params": {"threadId": thread_id, "turnId": turn_id,
                             "itemId": "command-1", "command": "must-not-run"}})
            response = sys.stdin.readline()
            if response:
                record(json.loads(response))
            status = "failed"
            error = {"message": "server tool request rejected"}
        else:
            if mode == "tool-item":
                send({"method": "item/started",
                      "params": {"threadId": thread_id, "turnId": turn_id,
                                 "item": {"id": "tool-1", "type": "commandExecution",
                                          "command": "must-not-run"}}})
            send({"method": "item/started",
                  "params": {"threadId": thread_id, "turnId": turn_id,
                             "item": {"id": "message-1", "type": "agentMessage",
                                      "text": "", "phase": "final_answer"}}})
            if mode in ("delta", "failed-delta"):
                send({"method": "item/agentMessage/delta",
                      "params": {"threadId": thread_id, "turnId": turn_id,
                                 "itemId": "message-1", "delta": reply}})
            status = {"failed": "failed", "failed-delta": "failed", "interrupted": "interrupted"}.get(mode, "completed")
            error = None if status == "completed" else {"message": "fixture turn " + status}
        item = {"id": "message-1", "type": "agentMessage", "text": reply,
                "phase": "final_answer"}
        if status == "completed":
            send({"method": "item/completed",
                  "params": {"threadId": thread_id, "turnId": turn_id, "item": item}})
        send({"method": "turn/completed",
              "params": {"threadId": thread_id,
                         "turn": {"id": turn_id, "status": status,
                                  "items": [item] if status == "completed" else [],
                                  "error": error}}})
    else:
        record({"_unknown_method": method})
        sys.exit(17)
"#;

fn assert_success(output: &Output) -> String {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout.clone()).unwrap()
}
#[cfg(unix)]
fn codex_command(
    scratch: &Scratch,
    log_path: &std::path::Path,
    mode: &str,
    reply: &str,
) -> Command {
    let mut command = scratch.command();
    command
        .env("RR_TEST_CODEX_LOG", log_path)
        .env("RR_TEST_CODEX_MODE", mode)
        .env("RR_TEST_CODEX_REPLY", reply);
    command
}

#[cfg(unix)]
fn codex_log(path: &std::path::Path) -> Vec<Value> {
    fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[cfg(unix)]
fn requests_for<'a>(log: &'a [Value], method: &str) -> Vec<&'a Value> {
    log.iter()
        .filter(|entry| entry["method"] == method)
        .collect()
}

#[cfg(unix)]
#[test]
fn codex_cli_uses_one_ephemeral_thread_and_turn_per_invocation() {
    let scratch = Scratch::new();
    let executable = scratch.fake_codex();
    let log_path = scratch.0.join("codex-requests.jsonl");
    scratch.codex_config(&executable, "gpt-test-configured");

    for (no_stream, prompt, reply) in [
        (false, "first-request-only", "streamed 🌍 response"),
        (true, "second-request-only", "nonstreamed response"),
    ] {
        let mut command = codex_command(&scratch, &log_path, "delta", reply);
        command
            .env("TMPDIR", &scratch.0)
            .env("RR_AICHAT_MODEL", "subscription:gpt-test-env")
            .arg("--model")
            .arg("subscription:gpt-test-cli");
        if no_stream {
            command.arg("--no-stream");
        }
        let output = command.arg(prompt).output().unwrap();
        assert_eq!(assert_success(&output).trim(), reply);

        let log = codex_log(&log_path);
        let threads = requests_for(&log, "thread/start");
        let turns = requests_for(&log, "turn/start");
        assert_eq!(threads.len(), if no_stream { 2 } else { 1 });
        assert_eq!(turns.len(), if no_stream { 2 } else { 1 });
        let thread = threads.last().unwrap()["params"].clone();
        let turn = turns.last().unwrap()["params"].clone();
        assert_eq!(thread["model"], "gpt-test-cli");
        assert_eq!(thread["modelProvider"], "openai");
        assert_eq!(thread["approvalPolicy"], "never");
        assert_eq!(thread["sandbox"], "read-only");
        assert_eq!(thread["ephemeral"], true);
        assert_eq!(thread["config"]["features"]["shell_tool"], false);
        assert_eq!(thread["config"]["features"]["plugins"], false);
        assert_eq!(
            thread["config"]["mcp_servers"]["fixture-mcp"]["enabled"],
            false
        );
        assert_eq!(
            thread["config"]["plugins"]["fixture-plugin"]["enabled"],
            false
        );
        let cwd = PathBuf::from(thread["cwd"].as_str().unwrap());
        assert!(
            cwd.starts_with(&scratch.0),
            "thread escaped isolated scratch cwd"
        );
        assert_eq!(turn["model"], "gpt-test-cli");
        assert_eq!(turn["approvalPolicy"], "never");
        assert!(turn["input"].to_string().contains(prompt));
        assert!(!turn["input"].to_string().contains(if no_stream {
            "first-request-only"
        } else {
            "second-request-only"
        }));
    }

    let log = codex_log(&log_path);
    let threads = requests_for(&log, "thread/start");
    let turns = requests_for(&log, "turn/start");
    assert_eq!(threads.len(), 2);
    assert_eq!(turns.len(), 2);
    let ids = log
        .iter()
        .filter_map(|entry| entry["_thread_id"].as_str())
        .collect::<Vec<_>>();
    assert_eq!(ids.len(), 2);
    assert_ne!(ids[0], ids[1]);
    for turn in turns {
        assert!(turn["params"]["threadId"].as_str().is_some());
    }
}
#[cfg(unix)]
#[test]
fn codex_replays_saved_history_once_without_resuming_or_leaking_threads() {
    let scratch = Scratch::new();
    let executable = scratch.fake_codex();
    let log_path = scratch.0.join("history.jsonl");
    scratch.codex_config(&executable, "gpt-6.1-sol");
    for prompt in ["first saved user message", "second saved user message"] {
        let output = codex_command(&scratch, &log_path, "ok", "saved assistant answer")
            .env("TMPDIR", &scratch.0)
            .args([
                "--session",
                "codex-history",
                "--save-session",
                "--no-stream",
                prompt,
            ])
            .output()
            .unwrap();
        assert_success(&output);
    }
    let log = codex_log(&log_path);
    let threads = requests_for(&log, "thread/start");
    let turns = requests_for(&log, "turn/start");
    assert_eq!(threads.len(), 2);
    assert_eq!(turns.len(), 2);
    let second_input = turns[1]["params"]["input"].to_string();
    assert_eq!(second_input.matches("first saved user message").count(), 1);
    assert_eq!(second_input.matches("saved assistant answer").count(), 1);
    assert_eq!(second_input.matches("second saved user message").count(), 1);
    let ids = log
        .iter()
        .filter_map(|entry| entry["_thread_id"].as_str())
        .collect::<Vec<_>>();
    assert_eq!(ids.len(), 2);
    assert_ne!(ids[0], ids[1]);
    assert!(turns
        .iter()
        .all(|turn| turn["params"].get("previousResponseId").is_none()));
}

#[cfg(unix)]
#[test]
fn codex_code_extraction_and_execute_mode_only_return_reviewable_text() {
    let scratch = Scratch::new();
    let executable = scratch.fake_codex();
    let log_path = scratch.0.join("codex-requests.jsonl");
    let sentinel = scratch.0.join("generated-command-ran");
    scratch.codex_config(&executable, "gpt-6.1-sol");

    let code_reply = "```nu\nprint 'review this'\n```";
    let output = codex_command(&scratch, &log_path, "ok", code_reply)
        .env("TMPDIR", &scratch.0)
        .args(["--code", "write a harmless example"])
        .output()
        .unwrap();
    assert_eq!(assert_success(&output).trim(), "print 'review this'");

    let suggestion = format!("printf safe > '{}'", sentinel.display());
    let output = codex_command(&scratch, &log_path, "ok", &suggestion)
        .env("TMPDIR", &scratch.0)
        .args(["-e", "suggest a command, do not run it"])
        .output()
        .unwrap();
    assert_eq!(assert_success(&output).trim(), suggestion);
    assert!(
        !sentinel.exists(),
        "returned command ran without an execution selection"
    );
}

#[cfg(unix)]
#[test]
fn codex_failed_and_interrupted_turns_are_reported_as_failures() {
    let scratch = Scratch::new();
    let executable = scratch.fake_codex();
    scratch.codex_config(&executable, "gpt-6.1-sol");
    for mode in ["failed", "interrupted", "failed-delta"] {
        let log_path = scratch.0.join(format!("{mode}.jsonl"));
        let output = codex_command(&scratch, &log_path, mode, "partial fixture output")
            .env("TMPDIR", &scratch.0)
            .arg("request a text response")
            .output()
            .unwrap();
        assert!(
            !output.status.success(),
            "mode {mode} unexpectedly succeeded"
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains(if mode == "interrupted" {
                "interrupted"
            } else {
                "failed"
            }),
            "terminal error was not surfaced for {mode}: {stderr}"
        );
        assert_eq!(requests_for(&codex_log(&log_path), "turn/start").len(), 1);
    }
}

#[cfg(unix)]
#[test]
fn codex_unexpected_tool_item_is_not_returned_as_a_successful_answer() {
    let scratch = Scratch::new();
    let executable = scratch.fake_codex();
    let log_path = scratch.0.join("tool-item.jsonl");
    scratch.codex_config(&executable, "gpt-6.1-sol");
    let output = codex_command(&scratch, &log_path, "tool-item", "must not be returned")
        .env("TMPDIR", &scratch.0)
        .args(["--no-stream", "request a text response"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        output.stdout.is_empty(),
        "tool activity became a successful answer"
    );
    assert_eq!(requests_for(&codex_log(&log_path), "turn/start").len(), 1);
}

#[cfg(unix)]
#[test]
fn codex_server_tool_approval_stops_the_turn_without_execution() {
    let scratch = Scratch::new();
    let executable = scratch.fake_codex();
    let log_path = scratch.0.join("approval.jsonl");
    let sentinel = scratch.0.join("approval-command-ran");
    scratch.codex_config(&executable, "gpt-6.1-sol");
    let output = codex_command(
        &scratch,
        &log_path,
        "approval",
        &format!("printf unsafe > '{}'", sentinel.display()),
    )
    .env("TMPDIR", &scratch.0)
    .arg("request a text response")
    .output()
    .unwrap();
    assert!(!output.status.success());
    assert!(!sentinel.exists());
    let log = codex_log(&log_path);
    assert_eq!(requests_for(&log, "thread/start").len(), 1);
    assert_eq!(requests_for(&log, "turn/start").len(), 1);
}

#[cfg(unix)]
#[test]
fn codex_reviewed_command_requires_the_execution_key() {
    const DRIVE_REVIEW: &str = r#"
import os, pty, re, select, signal, sys, time
sentinel, key, binary = sys.argv[1:4]
pid, master = pty.fork()
if pid == 0:
    os.execvpe(binary, [binary, "--execute", "generate a harmless command"], os.environ)
buffer = b""
pressed = False
deadline = time.monotonic() + 15
finished = False
try:
    while time.monotonic() < deadline:
        if select.select([master], [], [], 0.05)[0]:
            try:
                chunk = os.read(master, 65536)
            except OSError:
                chunk = b""
            buffer += chunk
        plain = re.sub(rb"\x1b\[[0-?]*[ -/]*[@-~]", b"", buffer)
        if not pressed and b"quit: " in plain:
            assert not os.path.exists(sentinel), "command ran before execution key"
            os.write(master, key.encode())
            pressed = True
        done, status = os.waitpid(pid, os.WNOHANG)
        if done:
            finished = True
            assert pressed, "review prompt never appeared: " + repr(buffer)
            assert os.waitstatus_to_exitcode(status) == 0, repr(buffer)
            break
    else:
        raise AssertionError("review command timed out: " + repr(buffer))
finally:
    if not finished:
        try:
            os.kill(pid, signal.SIGKILL)
            os.waitpid(pid, 0)
        except ProcessLookupError:
            pass
    os.close(master)
"#;
    let scratch = Scratch::new();
    let executable = scratch.fake_codex();
    scratch.codex_config(&executable, "gpt-6.1-sol");
    for key in ["q", "e"] {
        let sentinel = scratch.0.join(format!("review-{key}-command-ran"));
        let log_path = scratch.0.join(format!("review-{key}.jsonl"));
        let inner = codex_command(
            &scratch,
            &log_path,
            "ok",
            &format!("printf reviewed > '{}'", sentinel.display()),
        );
        let output = Command::new("python3")
            .env_clear()
            .envs(
                inner
                    .get_envs()
                    .filter_map(|(name, value)| value.map(|value| (name, value))),
            )
            .env("TMPDIR", &scratch.0)
            .env("RR_AICHAT_SHELL", "sh")
            .env("TERM", "xterm")
            .args(["-c", DRIVE_REVIEW])
            .arg(&sentinel)
            .arg(key)
            .arg(inner.get_program())
            .output()
            .unwrap();
        assert_success(&output);
        if key == "e" {
            assert_eq!(fs::read_to_string(&sentinel).unwrap(), "reviewed");
        } else {
            assert!(!sentinel.exists(), "quitting executed the returned command");
        }
        assert_eq!(requests_for(&codex_log(&log_path), "turn/start").len(), 1);
    }
}
#[cfg(unix)]
#[test]
fn codex_reasoning_patches_merge_model_then_client_or_environment() {
    let scratch = Scratch::new();
    let executable = scratch.fake_codex();
    scratch.codex_config(&executable, "gpt-6.1-sol");
    let path = scratch.0.join("config.yaml");
    let mut config: Value = serde_yaml::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    config["clients"][0]["models"][0]["patch"] =
        json!({"body":{"reasoning":{"effort":"high","summary":"detailed"}}});
    config["clients"][0]["patch"] =
        json!({"chat_completions":{".*":{"body":{"reasoning":{"effort":"low"}}}}});
    fs::write(&path, serde_yaml::to_string(&config).unwrap()).unwrap();
    for (index, env_patch, effort, summary) in [
        (0, None, json!("low"), json!("detailed")),
        (
            1,
            Some(json!({".*":{"body":{"reasoning":{"summary":"none"}}}})),
            json!("high"),
            json!("none"),
        ),
        (
            2,
            Some(json!({".*":{"body":{"reasoning":{"effort":null,"summary":null}}}})),
            Value::Null,
            Value::Null,
        ),
    ] {
        let log_path = scratch.0.join(format!("reasoning-{index}.jsonl"));
        let mut command = codex_command(&scratch, &log_path, "ok", "reasoning response");
        command.env("TMPDIR", &scratch.0);
        if let Some(patch) = env_patch {
            command.env(
                "RR_AICHAT_PATCH_SUBSCRIPTION_CHAT_COMPLETIONS",
                patch.to_string(),
            );
        }
        assert_success(&command.arg("return a text response").output().unwrap());
        let log = codex_log(&log_path);
        let turn = requests_for(&log, "turn/start").into_iter().next().unwrap();
        assert_eq!(turn["params"]["effort"], effort);
        assert_eq!(turn["params"]["summary"], summary);
    }
}

#[cfg(unix)]
#[test]
fn codex_model_selection_obeys_config_environment_then_cli_precedence() {
    let scratch = Scratch::new();
    let executable = scratch.fake_codex();
    let log_path = scratch.0.join("model-precedence.jsonl");
    scratch.codex_config(&executable, "gpt-test-configured");
    let cases = [
        (None, None, "gpt-test-configured"),
        (Some("subscription:gpt-test-env"), None, "gpt-test-env"),
        (
            Some("subscription:gpt-test-env"),
            Some("subscription:gpt-test-cli"),
            "gpt-test-cli",
        ),
    ];
    for (env_model, cli_model, expected) in cases {
        let mut command = codex_command(&scratch, &log_path, "ok", "model response");
        command.env("TMPDIR", &scratch.0);
        if let Some(model) = env_model {
            command.env("RR_AICHAT_MODEL", model);
        }
        if let Some(model) = cli_model {
            command.args(["--model", model]);
        }
        let output = command.arg("select model").output().unwrap();
        assert_success(&output);
        let log = codex_log(&log_path);
        let thread = requests_for(&log, "thread/start")
            .into_iter()
            .last()
            .unwrap();
        assert_eq!(thread["params"]["model"], expected);
    }
    assert_eq!(requests_for(&codex_log(&log_path), "thread/start").len(), 3);
}

#[test]
fn responses_cli_streams_and_preserves_model_precedence() {
    let scratch = Scratch::new();
    for no_stream in [false, true] {
        let (base, fixture) = endpoint("héllo 🌍".into());
        scratch.config(&base);
        let mut command = scratch.command();
        command
            .env("RR_AICHAT_MODEL", "subscription:wrong")
            .env("AICHAT_MODEL", "legacy:wrong")
            .args(["--model", "native:unlisted-model"]);
        if no_stream {
            command.arg("--no-stream");
        }
        let output = command.arg("hello").output().unwrap();
        assert_eq!(assert_success(&output).trim(), "héllo 🌍");
        let request = fixture.join().unwrap();
        assert_eq!(request["model"], "unlisted-model");
        assert_eq!(request["store"], false);
    }
}

#[test]
fn responses_credentials_follow_client_env_config_then_shared_env() {
    let scratch = Scratch::new();
    for mode in ["configured", "client-env", "shared-env"] {
        let (base, fixture) = endpoint("credential precedence".into());
        scratch.config(&base);
        let path = scratch.0.join("config.yaml");
        let mut config = fs::read_to_string(&path).unwrap();
        let mut command = scratch.command();
        match mode {
            "configured" => {
                command.env("OPENAI_API_KEY", "ambient-must-not-win");
            }
            "client-env" => {
                config = config.replacen(
                    "api_key: fixture-only",
                    "api_key: configured-must-not-win",
                    1,
                );
                command
                    .env("NATIVE_API_KEY", "fixture-only")
                    .env("OPENAI_API_KEY", "ambient-must-not-win");
            }
            "shared-env" => {
                config = config.replacen("    api_key: fixture-only\n", "", 1);
                command.env("OPENAI_API_KEY", "fixture-only");
            }
            _ => unreachable!(),
        }
        fs::write(path, config).unwrap();
        let output = command
            .args(["--no-stream", "credential precedence"])
            .output()
            .unwrap();
        assert_eq!(assert_success(&output).trim(), "credential precedence");
        fixture.join().unwrap();
    }
}

#[test]
fn responses_command_review_never_executes_suggested_code() {
    let scratch = Scratch::new();
    let sentinel = scratch.0.join("must-not-exist");
    for shell in ["nu", "zsh"] {
        let suggestion = if shell == "nu" {
            format!("'unsafe' | save '{}'", sentinel.display())
        } else {
            format!("printf unsafe > '{}'", sentinel.display())
        };
        for no_stream in [false, true] {
            let (base, fixture) = endpoint(suggestion.clone());
            scratch.config(&base);
            let mut command = scratch.command();
            command
                .env("RR_AICHAT_SHELL", shell)
                .args(["--role", "%shell%"]);
            if no_stream {
                command.arg("--no-stream");
            }
            let output = command
                .arg("suggest a command, do not run it")
                .output()
                .unwrap();
            assert_eq!(assert_success(&output).trim(), suggestion);
            let body = fixture.join().unwrap();
            let transcript = body.to_string();
            assert!(transcript.contains(if shell == "nu" { "nushell" } else { "zsh" }));
            assert!(
                !sentinel.exists(),
                "adapter executed a generated command before review"
            );
        }
        for mode in ["--execute", "--code"] {
            let (base, fixture) = endpoint(format!("```{shell}\n{suggestion}\n```"));
            scratch.config(&base);
            let output = scratch
                .command()
                .env("RR_AICHAT_SHELL", shell)
                .args([mode, "suggest a command, do not run it"])
                .output()
                .unwrap();
            assert_eq!(assert_success(&output).trim(), suggestion);
            fixture.join().unwrap();
            assert!(
                !sentinel.exists(),
                "CLI executed a generated command before review"
            );
        }
    }
}

#[test]
fn responses_saved_history_is_replayed_once_without_cross_invocation_leakage() {
    let scratch = Scratch::new();
    let (base, fixture) = endpoint("first assistant answer".into());
    scratch.config(&base);
    let output = scratch
        .command()
        .args([
            "--session",
            "history",
            "--save-session",
            "--no-stream",
            "first user message",
        ])
        .output()
        .unwrap();
    assert_success(&output);
    fixture.join().unwrap();

    let (base, fixture) = endpoint("second assistant answer".into());
    scratch.config(&base);
    assert_success(
        &scratch
            .command()
            .args([
                "--session",
                "history",
                "--save-session",
                "--no-stream",
                "second user message",
            ])
            .output()
            .unwrap(),
    );
    let body = fixture.join().unwrap();
    let transcript = body["input"].to_string();
    assert_eq!(transcript.matches("first user message").count(), 1);
    assert_eq!(transcript.matches("first assistant answer").count(), 1);
    assert_eq!(transcript.matches("second user message").count(), 1);
    assert!(body.get("previous_response_id").is_none());

    let (base, fixture) = endpoint("isolated answer".into());
    scratch.config(&base);
    assert_success(
        &scratch
            .command()
            .args(["--no-stream", "isolated user message"])
            .output()
            .unwrap(),
    );
    let body = fixture.join().unwrap();
    assert!(!body["input"].to_string().contains("first user message"));
    assert!(!body["input"].to_string().contains("second user message"));
}

#[test]
fn legacy_model_selection_and_config_namespace_remain_independent() {
    let scratch = Scratch::new();
    scratch.config("http://127.0.0.1:1/v1");
    let wrong = Scratch::new();
    fs::write(
        wrong.0.join("config.yaml"),
        "this is not valid configuration",
    )
    .unwrap();
    let output = scratch
        .command()
        .env("AICHAT_CONFIG_DIR", &wrong.0)
        .args(["--model", "legacy:unlisted", "--dry-run", "namespace test"])
        .output()
        .unwrap();
    assert!(assert_success(&output).contains("namespace test"));
    let output = scratch.command().args(["--list-models"]).output().unwrap();
    assert!(assert_success(&output)
        .lines()
        .any(|line| line == "legacy:listed"));
}

struct ServerProcess(std::process::Child);

impl Drop for ServerProcess {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn proxy_request(scratch: &Scratch, streaming: bool) -> String {
    let reservation = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = reservation.local_addr().unwrap();
    drop(reservation);
    let _server = ServerProcess(
        scratch
            .command()
            .args(["--serve", &addr.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut socket = loop {
        match std::net::TcpStream::connect(addr) {
            Ok(socket) => break socket,
            Err(_) => {
                assert!(Instant::now() < deadline, "proxy never became ready");
                thread::sleep(Duration::from_millis(10));
            }
        }
    };
    socket
        .set_read_timeout(Some(Duration::from_secs(15)))
        .unwrap();
    let body = json!({"model":"native:configured","messages":[{"role":"user","content":"hello"}],"stream":streaming}).to_string();
    write!(socket, "POST /v1/chat/completions HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
    let mut response = String::new();
    socket.read_to_string(&mut response).unwrap();
    response
}

#[test]
fn proxy_exposes_native_usage_and_does_not_turn_terminal_failure_into_success() {
    let scratch = Scratch::new();
    for streaming in [false, true] {
        let (base, fixture) = endpoint("proxy reply".into());
        scratch.config(&base);
        let response = proxy_request(&scratch, streaming);
        fixture.join().unwrap();
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        assert!(response.contains("proxy reply"), "{response}");
        assert!(response.contains("\"prompt_tokens\":12"), "{response}");
        assert!(response.contains("\"completion_tokens\":4"), "{response}");
        if streaming {
            assert!(
                response.contains("\"upstream_response_id\":\"resp_cli\""),
                "{response}"
            );
            assert!(
                response.find("proxy reply").unwrap()
                    < response.find("\"upstream_response_id\"").unwrap(),
                "usage overtook buffered text: {response}"
            );
        }
    }
    let (base, fixture) = endpoint_status("partial reply".into(), "incomplete");
    scratch.config(&base);
    let response = proxy_request(&scratch, true);
    fixture.join().unwrap();
    assert!(response.contains("\"error\""), "{response}");
    assert!(
        !response.contains("\"finish_reason\":\"stop\""),
        "{response}"
    );
}

#[test]
fn legacy_proxy_providers_keep_their_existing_stream_contract() {
    let scratch = Scratch::new();
    for client_type in ["openai", "openai-compatible"] {
        for streaming in [false, true] {
            let (base, fixture) =
                endpoint_protocol("legacy proxy reply".into(), "completed", false);
            fs::write(scratch.0.join("config.yaml"), format!(
                "model: native:legacy-model\nsave: false\nfunction_calling: false\nclients:\n  - type: {client_type}\n    name: native\n    api_base: {base}\n    api_key: fixture-only\n"
            )).unwrap();
            let response = proxy_request(&scratch, streaming);
            fixture.join().unwrap();
            assert!(response.starts_with("HTTP/1.1 200"), "{response}");
            assert!(response.contains("legacy proxy reply"), "{response}");
            assert!(
                response.contains("\"finish_reason\":\"stop\""),
                "{response}"
            );
            assert!(!response.contains("upstream_response_id"), "{response}");
        }
    }
}
