use super::*;

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{process::Stdio, time::Duration};
use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout, Command},
    task::JoinHandle,
    time::timeout,
};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
const MAX_PROTOCOL_LINE: usize = 1024 * 1024;
const MAX_IGNORED_NOTIFICATIONS: usize = 256;
const MAX_MODEL_PAGES: usize = 20;
const UNSAFE_APP_SERVER_PROTOCOL: &str = "Codex app-server cannot be used safely by rr-aichat: the installed protocol does not provide a complete text-only tool-disable contract. Codex may execute shell, filesystem, network, MCP, or other built-in tools before rr-aichat can review a suggested command. No thread or turn was started. Disablement of approvals or read-only sandboxing is not sufficient. This adapter is unavailable until Codex provides a verifiable mechanism to suppress every agent tool.";

#[derive(Debug, Clone, Deserialize, Default)]
pub struct CodexAppServerConfig {
    pub name: Option<String>,
    pub executable: Option<String>,
    #[serde(default)]
    pub models: Vec<ModelData>,
    pub patch: Option<RequestPatch>,
    pub extra: Option<ExtraConfig>,
}

impl CodexAppServerClient {
    pub const PROMPTS: [PromptAction<'static>; 0] = [];

    fn executable(&self) -> Result<String> {
        let env_name = crate::utils::get_env_name("codex_app_server_executable");
        let configured = std::env::var(&env_name)
            .ok()
            .or_else(|| self.config.executable.clone());
        let executable = configured.unwrap_or_else(|| "codex".to_string());
        if executable.trim().is_empty() {
            bail!(
                "Codex executable is empty; set clients.codex-app-server.executable or {env_name}"
            );
        }
        Ok(executable)
    }

    async fn preflight(&self) -> Result<()> {
        run_preflight(
            AppServerSession::spawn(&self.executable()?).await?,
            self.model().real_name(),
        )
        .await
    }
}

async fn run_preflight(mut session: AppServerSession, model: &str) -> Result<()> {
    let result = validate_account_and_model(&mut session, model).await;
    let shutdown_result = session.shutdown().await;
    result?;
    shutdown_result?;
    Ok(())
}

fn validate_prompt_data(data: &ChatCompletionsData) -> Result<()> {
    if data
        .functions
        .as_ref()
        .is_some_and(|functions| !functions.is_empty())
    {
        bail!("Codex app-server doesn't support rr-aichat function calling");
    }
    for message in &data.messages {
        if matches!(message.role, MessageRole::Tool)
            || matches!(message.content, MessageContent::ToolCalls(_))
        {
            bail!("Codex app-server doesn't support rr-aichat function-call history");
        }
        if let MessageContent::Array(parts) = &message.content {
            if parts
                .iter()
                .any(|part| matches!(part, MessageContentPart::ImageUrl { .. }))
            {
                bail!("Codex app-server doesn't support multimodal input");
            }
        }
    }
    Ok(())
}

#[async_trait::async_trait]
impl Client for CodexAppServerClient {
    client_common_fns!();

    async fn chat_completions_inner(
        &self,
        _client: &reqwest::Client,
        data: ChatCompletionsData,
    ) -> Result<ChatCompletionsOutput> {
        validate_prompt_data(&data)?;
        self.preflight().await?;
        bail!(UNSAFE_APP_SERVER_PROTOCOL)
    }

    async fn chat_completions_streaming_inner(
        &self,
        _client: &reqwest::Client,
        _handler: &mut SseHandler,
        data: ChatCompletionsData,
    ) -> Result<()> {
        validate_prompt_data(&data)?;
        self.preflight().await?;
        bail!(UNSAFE_APP_SERVER_PROTOCOL)
    }
}

struct AppServerSession {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    stderr_drain: JoinHandle<()>,
    next_id: u64,
    request_timeout: Duration,
    process_group: Option<u32>,
}
impl AppServerSession {
    async fn spawn(executable: &str) -> Result<Self> {
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            let mut command = std::process::Command::new(executable);
            command.args(["app-server", "--stdio"]);
            command.process_group(0);
            Self::spawn_command(Command::from(command), true).await
        }
        #[cfg(not(unix))]
        {
            let mut command = Command::new(executable);
            command.args(["app-server", "--stdio"]);
            Self::spawn_command(command, false).await
        }
    }

    async fn spawn_command(mut command: Command, isolate_process_group: bool) -> Result<Self> {
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .context("Unable to start Codex app-server")?;
        let stdin = child
            .stdin
            .take()
            .context("Codex app-server stdin was not available")?;
        let stdout = child
            .stdout
            .take()
            .context("Codex app-server stdout was not available")?;
        let mut stderr = child
            .stderr
            .take()
            .context("Codex app-server stderr was not available")?;
        let stderr_drain = tokio::spawn(async move {
            let mut sink = tokio::io::sink();
            let _ = tokio::io::copy(&mut stderr, &mut sink).await;
        });
        let process_group = if isolate_process_group {
            child.id()
        } else {
            None
        };
        Ok(Self {
            child,
            stdin,
            stdout: BufReader::new(stdout),
            stderr_drain,
            next_id: 1,
            request_timeout: REQUEST_TIMEOUT,
            process_group,
        })
    }

    async fn initialize(&mut self) -> Result<()> {
        self.request(
            "initialize",
            json!({"clientInfo":{"name":"rr-aichat","version":env!("CARGO_PKG_VERSION")}}),
        )
        .await?;
        self.notify("initialized").await
    }

    async fn notify(&mut self, method: &str) -> Result<()> {
        let mut bytes = serde_json::to_vec(&json!({"method":method}))?;
        bytes.push(b'\n');
        timeout(self.request_timeout, self.stdin.write_all(&bytes))
            .await
            .context("Timed out sending Codex app-server notification")??;
        timeout(self.request_timeout, self.stdin.flush())
            .await
            .context("Timed out flushing Codex app-server notification")??;
        Ok(())
    }

    async fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id;
        self.next_id = self
            .next_id
            .checked_add(1)
            .context("App-server request id overflow")?;
        let mut bytes = serde_json::to_vec(&json!({"id":id,"method":method,"params":params}))?;
        bytes.push(b'\n');
        timeout(self.request_timeout, self.stdin.write_all(&bytes))
            .await
            .with_context(|| format!("Timed out sending Codex app-server request '{method}'"))??;
        timeout(self.request_timeout, self.stdin.flush())
            .await
            .with_context(|| format!("Timed out flushing Codex app-server request '{method}'"))??;

        for _ in 0..=MAX_IGNORED_NOTIFICATIONS {
            let line = timeout(self.request_timeout, read_protocol_line(&mut self.stdout))
                .await
                .with_context(|| {
                    format!("Timed out waiting for Codex app-server response to '{method}'")
                })??;
            let mut message: Value = serde_json::from_slice(&line).with_context(|| {
                format!("Codex app-server returned malformed JSON for '{method}'")
            })?;
            if message
                .get("id")
                .is_some_and(|response_id| response_id == &json!(id))
            {
                if message.get("error").is_some() {
                    bail!("Codex app-server rejected request '{method}'");
                }
                return message.get_mut("result").map(Value::take).with_context(|| {
                    format!("Codex app-server response to '{method}' had no result")
                });
            }
            if message.get("id").is_some() {
                bail!("Codex app-server sent an unmatched response while waiting for '{method}'");
            }
            if message.get("method").and_then(Value::as_str).is_none() {
                bail!("Codex app-server sent an invalid notification while waiting for '{method}'");
            }
        }
        bail!("Codex app-server exceeded the notification limit while waiting for '{method}'")
    }

    async fn shutdown(&mut self) -> Result<()> {
        self.kill_process_tree();
        let kill_result = if self.child.try_wait()?.is_none() {
            self.child
                .start_kill()
                .context("Failed to stop Codex app-server")
        } else {
            Ok(())
        };
        let wait_result = timeout(self.request_timeout, self.child.wait())
            .await
            .context("Timed out reaping Codex app-server")?;
        self.stderr_drain.abort();
        kill_result?;
        wait_result?;
        Ok(())
    }
}

impl AppServerSession {
    fn kill_process_tree(&mut self) {
        if let Some(process_group) = self.process_group.take() {
            #[cfg(unix)]
            {
                unsafe extern "C" {
                    fn kill(pid: i32, signal: i32) -> i32;
                }
                if let Ok(process_group) = i32::try_from(process_group) {
                    // spawn() creates a fresh group owned by this session. Negative
                    // pid targets that group, and SIGKILL stops blocked descendants.
                    unsafe {
                        kill(-process_group, 9);
                    }
                }
            }
            #[cfg(not(unix))]
            let _ = process_group;
        }
        #[cfg(not(unix))]
        {
            // Tokio's kill_on_drop handles the owned app-server on other hosts.
            let _ = self.child.start_kill();
        }
    }
}

impl Drop for AppServerSession {
    fn drop(&mut self) {
        self.kill_process_tree();
        self.stderr_drain.abort();
    }
}
async fn read_protocol_line<R>(reader: &mut R) -> Result<Vec<u8>>
where
    R: AsyncBufRead + Unpin,
{
    let mut line = Vec::new();
    loop {
        let available = reader
            .fill_buf()
            .await
            .context("Failed reading Codex app-server stdout")?;
        if available.is_empty() {
            bail!("Codex app-server closed stdout before completing its response");
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let count = newline.map_or(available.len(), |index| index + 1);
        if line.len().saturating_add(count) > MAX_PROTOCOL_LINE {
            bail!("Codex app-server protocol line exceeded the {MAX_PROTOCOL_LINE}-byte limit");
        }
        line.extend_from_slice(&available[..count]);
        reader.consume(count);
        if newline.is_some() {
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            return Ok(line);
        }
    }
}

async fn validate_account_and_model(session: &mut AppServerSession, model: &str) -> Result<()> {
    session.initialize().await?;

    let account_response = session
        .request("account/read", json!({"refreshToken":true}))
        .await
        .context(
            "Unable to verify Codex account. Run `codex login` and select Sign in with ChatGPT.",
        )?;
    let account = account_response.get("account").and_then(Value::as_object);
    match account.and_then(|account| account.get("type")).and_then(Value::as_str) {
        Some("chatgpt") => {}
        Some("apiKey") => bail!("Codex is authenticated with an API key, not a ChatGPT subscription. Run `codex login` and select Sign in with ChatGPT; rr-aichat will not use API-key billing."),
        _ => bail!("No managed ChatGPT account is signed in to Codex. Run `codex login` and select Sign in with ChatGPT."),
    }

    let config = session
        .request("config/read", json!({"includeLayers":false}))
        .await?;
    let provider = config
        .pointer("/config/model_provider")
        .and_then(Value::as_str)
        .unwrap_or("openai");
    if provider != "openai" {
        bail!("Codex is configured to use model provider '{provider}', not its managed OpenAI/ChatGPT provider. Set model_provider = \"openai\" in Codex config; rr-aichat will not route subscription requests through another provider.");
    }

    let mut cursor = Value::Null;
    for _ in 0..MAX_MODEL_PAGES {
        let response = session
            .request(
                "model/list",
                json!({"cursor":cursor,"includeHidden":false,"limit":100}),
            )
            .await?;
        if response
            .get("data")
            .and_then(Value::as_array)
            .is_some_and(|models| {
                models.iter().any(|entry| {
                    entry.get("model").and_then(Value::as_str) == Some(model)
                        || entry.get("id").and_then(Value::as_str) == Some(model)
                })
            })
        {
            return Ok(());
        }
        cursor = match response.get("nextCursor") {
            Some(Value::String(cursor)) => Value::String(cursor.clone()),
            _ => bail!("Codex model '{model}' is not available in the authenticated account's model catalog"),
        };
    }
    bail!("Codex model catalog exceeded {MAX_MODEL_PAGES} pages before model '{model}' was found")
}

#[cfg(all(test, unix))]
mod tests {
    use super::{read_protocol_line, run_preflight, AppServerSession};
    use std::{
        fs,
        sync::atomic::{AtomicU64, Ordering},
        time::Duration,
    };
    use tokio::io::BufReader;
    use tokio::process::Command;

    static NEXT_SENTINEL: AtomicU64 = AtomicU64::new(0);

    const FAKE_SERVER: &str = r#"
mode=$1
while IFS= read -r line; do
  case "$line" in
    *'"method":"initialize"'*)
      if [ "$mode" = "stall" ]; then continue; fi
      if [ "$mode" = "malformed" ]; then printf 'not-json\n'; continue; fi
      if [ "$mode" = "eof" ]; then exit 0; fi
      if [ "$mode" = "pressure" ]; then
        i=0
        while [ "$i" -lt 256 ]; do printf '%4096s' x >&2; i=$((i+1)); done
      fi
      printf '{"method":"server/ready"}\n'
      printf '{"id":1,"result":{}}\n'
      ;;
    *'"method":"initialized"'*) ;;
    *'"method":"account/read"'*)
      if [ "$mode" = "noauth" ]; then
        printf '{"id":2,"result":{"account":null}}\n'
      elif [ "$mode" = "api-key" ]; then
        printf '{"id":2,"result":{"account":{"type":"apiKey"}}}\n'
      else
        printf '{"id":2,"result":{"account":{"type":"chatgpt","planType":"pro","email":"safe-test@example.invalid"}}}\n'
      fi
      ;;
    *'"method":"config/read"'*)
      if [ "$mode" = "provider-mismatch" ]; then
        printf '{"id":3,"result":{"config":{"model_provider":"third-party"}}}\n'
      else
        printf '{"id":3,"result":{"config":{"model_provider":"openai"}}}\n'
      fi
      ;;
    *'"method":"model/list"'*)
      if [ "$mode" = "model-missing" ]; then
        printf '{"id":4,"result":{"data":[{"id":"other","model":"other"}],"nextCursor":null}}\n'
      else
        printf '{"id":4,"result":{"data":[{"id":"gpt-6.1-sol","model":"gpt-6.1-sol"}],"nextCursor":null}}\n'
      fi
      ;;
    *'"method":"thread/start"'*|*'"method":"turn/start"'*)
      : > "$RR_TEST_SENTINEL"
      ;;
    *) exit 23 ;;
  esac
done
"#;

    async fn fake_session(mode: &str, sentinel: &std::path::Path) -> AppServerSession {
        let mut command = Command::new("sh");
        command
            .arg("-c")
            .arg(FAKE_SERVER)
            .arg("rr-aichat-fake-server")
            .arg(mode)
            .env("RR_TEST_SENTINEL", sentinel);
        AppServerSession::spawn_command(command, false)
            .await
            .unwrap()
    }

    fn sentinel_path() -> std::path::PathBuf {
        let serial = NEXT_SENTINEL.fetch_add(1, Ordering::Relaxed);
        let root = std::env::var_os("HOME").expect("HOME is required for adapter fixtures");
        let root =
            std::path::PathBuf::from(root).join("dev/agentic-scratchpads/aichat/adapter-cli-tests");
        fs::create_dir_all(&root).unwrap();
        root.join(format!(
            "rr-aichat-codex-no-turn-{}-{serial}",
            std::process::id()
        ))
    }

    #[tokio::test]
    async fn safe_preflight_does_not_start_thread_or_turn() {
        let sentinel = sentinel_path();
        let session = fake_session("normal", &sentinel).await;
        run_preflight(session, "gpt-6.1-sol").await.unwrap();
        assert!(!sentinel.exists());
        let _ = fs::remove_file(sentinel);
    }

    #[tokio::test]
    async fn api_key_account_is_rejected_without_fallback() {
        let sentinel = sentinel_path();
        let session = fake_session("api-key", &sentinel).await;
        let error = run_preflight(session, "gpt-6.1-sol").await.unwrap_err();
        assert!(error.to_string().contains("not a ChatGPT subscription"));
        assert!(error.to_string().contains("will not use API-key billing"));
        assert!(!sentinel.exists());
        let _ = fs::remove_file(sentinel);
    }
    #[tokio::test]
    async fn absent_account_is_rejected_actionably() {
        let sentinel = sentinel_path();
        let session = fake_session("noauth", &sentinel).await;
        let error = run_preflight(session, "gpt-6.1-sol").await.unwrap_err();
        assert!(error.to_string().contains("Run `codex login`"));
        assert!(!sentinel.exists());
        let _ = fs::remove_file(sentinel);
    }

    #[tokio::test]
    async fn unavailable_model_is_rejected() {
        let sentinel = sentinel_path();
        let session = fake_session("model-missing", &sentinel).await;
        let error = run_preflight(session, "gpt-6.1-sol").await.unwrap_err();
        assert!(error
            .to_string()
            .contains("not available in the authenticated account"));
        assert!(!sentinel.exists());
        let _ = fs::remove_file(sentinel);
    }

    #[tokio::test]
    async fn alternate_model_provider_is_rejected() {
        let sentinel = sentinel_path();
        let session = fake_session("provider-mismatch", &sentinel).await;
        let error = run_preflight(session, "gpt-6.1-sol").await.unwrap_err();
        assert!(error
            .to_string()
            .contains("not its managed OpenAI/ChatGPT provider"));
        assert!(!sentinel.exists());
        let _ = fs::remove_file(sentinel);
    }

    #[tokio::test]
    async fn stderr_is_drained_and_request_ids_are_correlated() {
        let sentinel = sentinel_path();
        let session = fake_session("pressure", &sentinel).await;
        run_preflight(session, "gpt-6.1-sol").await.unwrap();
        assert!(!sentinel.exists());
        let _ = fs::remove_file(sentinel);
    }

    #[tokio::test]
    async fn malformed_and_eof_responses_fail_closed() {
        for mode in ["malformed", "eof"] {
            let sentinel = sentinel_path();
            let session = fake_session(mode, &sentinel).await;
            let error = run_preflight(session, "gpt-6.1-sol").await.unwrap_err();
            assert!(!error.to_string().contains("No thread or turn was started"));
            assert!(!sentinel.exists());
            let _ = fs::remove_file(sentinel);
        }
    }

    #[tokio::test]
    async fn timed_out_server_is_killed_and_reaped() {
        let sentinel = sentinel_path();
        let mut session = fake_session("stall", &sentinel).await;
        session.request_timeout = Duration::from_millis(40);
        let error = run_preflight(session, "gpt-6.1-sol").await.unwrap_err();
        assert!(error.to_string().contains("Timed out waiting"));
        assert!(!sentinel.exists());
        let _ = fs::remove_file(sentinel);
    }
    #[tokio::test]
    async fn cancelling_preflight_kills_owned_child() {
        let sentinel = sentinel_path();
        let session = fake_session("stall", &sentinel).await;
        let pid = session.child.id().expect("spawned server has a process id");
        let task = tokio::spawn(run_preflight(session, "gpt-6.1-sol"));
        tokio::time::sleep(Duration::from_millis(40)).await;
        task.abort();
        let _ = task.await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        let status = Command::new("sh")
            .arg("-c")
            .arg("kill -0 \"$1\" 2>/dev/null")
            .arg("rr-aichat-process-check")
            .arg(pid.to_string())
            .status()
            .await
            .unwrap();
        assert!(
            !status.success(),
            "cancelled app-server process {pid} is still running"
        );
        assert!(!sentinel.exists());
        let _ = fs::remove_file(sentinel);
    }

    #[tokio::test]
    async fn unavailable_executable_is_reported() {
        let error = AppServerSession::spawn("/definitely-not-a-real-codex-app-server-executable")
            .await
            .err()
            .expect("nonexistent executable must fail to spawn");
        assert!(error
            .to_string()
            .contains("Unable to start Codex app-server"));
    }
    #[tokio::test]
    async fn dropping_session_kills_owned_child() {
        let sentinel = sentinel_path();
        let session = fake_session("stall", &sentinel).await;
        let pid = session.child.id().expect("spawned server has a process id");
        drop(session);
        tokio::time::sleep(Duration::from_millis(100)).await;
        let status = Command::new("sh")
            .arg("-c")
            .arg("kill -0 \"$1\" 2>/dev/null")
            .arg("rr-aichat-process-check")
            .arg(pid.to_string())
            .status()
            .await
            .unwrap();
        assert!(
            !status.success(),
            "dropped app-server process {pid} is still running"
        );
        let _ = fs::remove_file(sentinel);
    }

    #[tokio::test]
    async fn oversized_protocol_line_is_rejected() {
        let mut input = vec![b'x'; super::MAX_PROTOCOL_LINE + 1];
        input.push(b'\n');
        let mut reader = BufReader::new(input.as_slice());
        let error = read_protocol_line(&mut reader).await.unwrap_err();
        assert!(error.to_string().contains("exceeded the"));
    }

    #[tokio::test]
    async fn protocol_reader_preserves_jsonl_and_strips_crlf() {
        let mut reader = BufReader::new(&b"{\"id\":1}\r\n"[..]);
        assert_eq!(
            read_protocol_line(&mut reader).await.unwrap(),
            b"{\"id\":1}"
        );
    }

    #[tokio::test]
    async fn unsupported_requests_fail_before_starting_the_app_server() {
        let client = super::CodexAppServerClient {
            global_config: std::sync::Arc::new(parking_lot::RwLock::new(
                crate::config::Config::default(),
            )),
            config: super::CodexAppServerConfig {
                executable: Some("/must-not-be-started".into()),
                ..Default::default()
            },
            model: super::Model::from_config("subscription", &[super::ModelData::new("model")])
                .remove(0),
        };
        let declaration: crate::function::FunctionDeclaration = serde_json::from_value(
            serde_json::json!({"name":"lookup","description":"lookup","parameters":{"type":"object"}})
        ).unwrap();
        let cases = [
            (vec![], Some(vec![declaration]), "function calling"),
            (
                vec![super::Message::new(
                    super::MessageRole::Tool,
                    super::MessageContent::Text("uncorrelated result".into()),
                )],
                None,
                "function-call history",
            ),
            (
                vec![super::Message::new(
                    super::MessageRole::User,
                    super::MessageContent::Array(vec![super::MessageContentPart::ImageUrl {
                        image_url: super::ImageUrl {
                            url: "https://example.invalid/image.png".into(),
                        },
                    }]),
                )],
                None,
                "multimodal input",
            ),
        ];
        let http_client = reqwest::Client::new();
        for (messages, functions, expected) in cases {
            for stream in [false, true] {
                let data = super::ChatCompletionsData {
                    messages: messages.clone(),
                    functions: functions.clone(),
                    temperature: None,
                    top_p: None,
                    stream,
                };
                let error = if stream {
                    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
                    let mut handler =
                        super::SseHandler::new(tx, crate::utils::AbortSignalInner::new());
                    super::Client::chat_completions_streaming_inner(
                        &client,
                        &http_client,
                        &mut handler,
                        data,
                    )
                    .await
                    .unwrap_err()
                } else {
                    super::Client::chat_completions_inner(&client, &http_client, data)
                        .await
                        .unwrap_err()
                };
                assert!(error.to_string().contains(expected), "{error}");
            }
        }
    }
}
