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

fn assert_success(output: &Output) -> String {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout.clone()).unwrap()
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
