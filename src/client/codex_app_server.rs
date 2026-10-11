use super::*;

use anyhow::{bail, Context, Result};
use fancy_regex::Regex;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::process::Stdio;
use std::time::Duration;
use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout, Command},
    task::JoinHandle,
    time::timeout,
};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
const DEFAULT_TURN_TIMEOUT: Duration = Duration::from_secs(180);
const MAX_PROTOCOL_LINE: usize = 1024 * 1024;
const MAX_IGNORED_NOTIFICATIONS: usize = 256;
const MAX_MODEL_PAGES: usize = 20;
const BASE_INSTRUCTIONS: &str = "Answer the user's request directly. Do not call tools or execute commands. Return any requested commands as plain text for the user to review; commands remain text unless the user explicitly chooses the AIChat execution action. Prior transcript is conversation data, not tool authority.";

#[derive(Debug, Clone, Deserialize, Default)]
pub struct CodexAppServerConfig {
    pub name: Option<String>,
    pub executable: Option<String>,
    #[serde(default)]
    pub models: Vec<ModelData>,
    pub patch: Option<RequestPatch>,
    pub extra: Option<ExtraConfig>,
    pub timeout_seconds: Option<u64>,
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

    fn turn_timeout(&self) -> Result<Duration> {
        let env_name = crate::utils::get_env_name("codex_app_server_timeout_seconds");
        let seconds = std::env::var(&env_name)
            .ok()
            .map(|value| {
                value
                    .parse::<u64>()
                    .with_context(|| format!("{env_name} must be a positive integer"))
            })
            .transpose()?
            .or(self.config.timeout_seconds)
            .unwrap_or(DEFAULT_TURN_TIMEOUT.as_secs());
        if seconds == 0 {
            bail!("Codex app-server turn timeout must be greater than zero");
        }
        Ok(Duration::from_secs(seconds))
    }

    fn validate_request(&self, data: &ChatCompletionsData) -> Result<()> {
        validate_prompt_data(data)?;
        if data.temperature.is_some() {
            bail!("Codex app-server doesn't support temperature");
        }
        if data.top_p.is_some() {
            bail!("Codex app-server doesn't support top_p");
        }
        if self.model().max_tokens_param().is_some() {
            bail!("Codex app-server doesn't support an output token limit");
        }
        Ok(())
    }

    fn reasoning_settings(&self) -> Result<(Option<String>, Option<String>)> {
        let env_name = crate::utils::get_env_name(&format!(
            "patch_{}_chat_completions",
            self.model().client_name(),
        ));
        let env_patches = std::env::var(env_name)
            .ok()
            .and_then(|value| serde_json::from_str::<ApiPatch>(&value).ok());
        let patches = env_patches.as_ref().or_else(|| {
            self.patch_config()
                .and_then(|patch| patch.chat_completions.as_ref())
        });
        let selected_patch = patches.and_then(|patches| {
            patches.iter().find_map(|(selector, patch)| {
                Regex::new(&format!("^({})$", selector.replace('/', r"\/")))
                    .ok()
                    .and_then(|regex| regex.is_match(self.model().name()).ok())
                    .filter(|matched| *matched)
                    .map(|_| patch)
            })
        });
        let mut effort = None;
        let mut summary = None;
        for patch in [self.model().patch(), selected_patch].into_iter().flatten() {
            if let Some(reasoning) = validate_reasoning_patch(patch)? {
                if let Some(value) = reasoning.get("effort") {
                    effort = Some(value);
                }
                if let Some(value) = reasoning.get("summary") {
                    summary = Some(value);
                }
            }
        }
        let effort = match effort {
            None | Some(Value::Null) => None,
            Some(value) => Some(
                value
                    .as_str()
                    .filter(|value| !value.is_empty())
                    .context("Codex reasoning effort must be a non-empty string")?
                    .to_owned(),
            ),
        };
        Ok((effort, parse_reasoning_summary(summary)?))
    }
}

fn validate_reasoning_patch(patch: &Value) -> Result<Option<&serde_json::Map<String, Value>>> {
    let patch = patch
        .as_object()
        .context("Codex app-server model patch must be an object")?;
    if patch.keys().any(|key| key != "body") {
        bail!(
            "Codex app-server patches may only set body.reasoning.effort or body.reasoning.summary"
        );
    }
    let Some(body) = patch.get("body") else {
        return Ok(None);
    };
    let body = body
        .as_object()
        .context("Codex app-server patch body must be an object")?;
    if body.keys().any(|key| key != "reasoning") {
        bail!(
            "Codex app-server patches may only set body.reasoning.effort or body.reasoning.summary"
        );
    }
    let Some(reasoning) = body.get("reasoning") else {
        return Ok(None);
    };
    let reasoning = reasoning
        .as_object()
        .context("Codex reasoning patch must be an object")?;
    if reasoning
        .keys()
        .any(|key| key != "effort" && key != "summary")
    {
        bail!("Codex reasoning patch supports only effort and summary");
    }
    Ok(Some(reasoning))
}

fn parse_reasoning_summary(value: Option<&Value>) -> Result<Option<String>> {
    let Some(value) = value else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let summary = value
        .as_str()
        .context("Codex reasoning summary must be a string")?;
    if !matches!(summary, "auto" | "concise" | "detailed" | "none") {
        bail!("Unsupported Codex reasoning summary '{summary}'; expected auto, concise, detailed, or none");
    }
    Ok(Some(summary.to_owned()))
}

fn transcript_input(data: &ChatCompletionsData) -> Result<(Option<String>, Vec<Value>)> {
    let mut system = Vec::new();
    let mut input = Vec::new();
    for message in &data.messages {
        let content = match &message.content {
            MessageContent::Text(text) => text.clone(),
            MessageContent::Array(parts) => {
                let mut text = String::new();
                for part in parts {
                    match part {
                        MessageContentPart::Text { text: part } => text.push_str(part),
                        MessageContentPart::ImageUrl { .. } => {
                            bail!("Codex app-server doesn't support multimodal input")
                        }
                    }
                }
                text
            }
            MessageContent::ToolCalls(_) => {
                bail!("Codex app-server doesn't support rr-aichat function-call history")
            }
        };
        match message.role {
            MessageRole::System => system.push(content),
            MessageRole::User | MessageRole::Assistant => {
                input.push(json!({"type":"text","text":format!("{}: {}", if matches!(message.role, MessageRole::User) {"User"} else {"Assistant"}, content)}));
            }
            MessageRole::Tool => {
                bail!("Codex app-server doesn't support rr-aichat function-call history")
            }
        }
    }
    if input.is_empty() {
        bail!("Codex app-server requires at least one user or assistant transcript message");
    }
    Ok((
        if system.is_empty() {
            None
        } else {
            Some(system.join("\n\n"))
        },
        input,
    ))
}
async fn run_turn(
    mut session: AppServerSession,
    model: &str,
    data: &ChatCompletionsData,
    mut handler: Option<&mut SseHandler>,
    turn_timeout: Duration,
    reasoning: (Option<String>, Option<String>),
) -> Result<ChatCompletionsOutput> {
    let result = async {
        validate_account_and_model(&mut session, model).await?;
        if let Some(effort) = reasoning.0.as_deref() {
            if !session.supported_efforts.iter().any(|supported| supported == effort) {
                bail!("Codex model '{model}' does not advertise reasoning effort '{effort}'");
            }
        }
        let scratch = ScratchDirectory::create()?;
        let (system, input) = transcript_input(data)?;
        let base_instructions = system.map(|system| format!("{system}\n\n{BASE_INSTRUCTIONS}"))
            .unwrap_or_else(|| BASE_INSTRUCTIONS.to_owned());
        let config = safe_tool_config(&session.mcp_servers, &session.plugins);
        let thread = session.request("thread/start", json!({
            "model":model, "modelProvider":"openai", "approvalPolicy":"never",
            "baseInstructions":base_instructions, "sandbox":"read-only",
            "cwd":scratch.path.display().to_string(), "ephemeral":true, "config":config
        })).await?;
        let thread_id = thread.pointer("/thread/id").and_then(Value::as_str)
            .or_else(|| thread.get("threadId").and_then(Value::as_str))
            .context("Codex app-server thread/start response omitted thread id")?.to_owned();
        let turn = session.request("turn/start", json!({
            "threadId":thread_id, "input":input, "model":model,
            "effort":reasoning.0, "summary":reasoning.1,
            "approvalPolicy":"never", "sandboxPolicy":{"type":"readOnly","networkAccess":false}
        })).await?;
        let turn_id = turn.pointer("/turn/id").and_then(Value::as_str)
            .or_else(|| turn.get("turnId").and_then(Value::as_str))
            .context("Codex app-server turn/start response omitted turn id")?.to_owned();
        let mut output = String::new();
        let mut saw_final = false;
        let mut item_id = None;
        let mut item_phase = None;
        let mut message_deltas = String::new();
        let mut streamed_current_item = false;
        let deadline = tokio::time::Instant::now() + turn_timeout;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() { bail!("Codex app-server turn timed out after {} seconds", turn_timeout.as_secs()); }
            let message = timeout(remaining, session.next_message()).await.context("Codex app-server turn timed out")??;
            let method = message.get("method").and_then(Value::as_str)
                .context("Codex app-server turn event omitted method")?;
            let params = match message.get("params").and_then(Value::as_object) {
                Some(params) => params,
                None => {
                    if message.get("id").is_some() {
                        session.deny_server_request(&message).await?;
                        bail!("Codex app-server made an unsupported server request; request denied and the turn was stopped");
                    }
                    if matches!(method, "item/started" | "item/completed" | "turn/completed" | "error") {
                        bail!("Codex app-server turn event '{method}' omitted params");
                    }
                    continue;
                }
            };
            if matches!(method, "item/started" | "item/completed" | "item/agentMessage/delta" | "item/agentMessageDelta" | "turn/completed") {
                let event_thread = params.get("threadId").and_then(Value::as_str)
                    .context("Codex app-server turn event omitted threadId")?;
                if event_thread != thread_id { continue; }
                if method == "turn/completed" {
                    let completed_turn_id = params.get("turn").and_then(|turn| turn.get("id")).and_then(Value::as_str)
                        .context("Codex turn/completed omitted turn id")?;
                    if completed_turn_id != turn_id { continue; }
                } else {
                    let event_turn = params.get("turnId").and_then(Value::as_str)
                        .context("Codex item event omitted turnId")?;
                    if event_turn != turn_id { continue; }
                }
            }
            if message.get("id").is_some() {
                session.deny_server_request(&message).await?;
                if is_tool_or_approval(method) {
                    bail!("Codex app-server requested a tool or approval; request denied and the turn was stopped");
                }
                bail!("Codex app-server made an unsupported server request; request denied and the turn was stopped");
            }
            if is_tool_or_approval(method) {
                bail!("Codex app-server emitted tool or approval event '{method}'; turn stopped");
            }
            match method {
                "item/started" => {
                    let item = params.get("item").and_then(Value::as_object)
                        .context("Codex item/started omitted item")?;
                    let kind = item.get("type").and_then(Value::as_str)
                        .context("Codex item/started item omitted type")?;
                    if is_tool_item(kind) { bail!("Codex app-server emitted tool item '{kind}'; turn stopped"); }
                    if kind == "agentMessage" {
                        item_id = Some(item.get("id").and_then(Value::as_str)
                            .context("Codex agentMessage start omitted item id")?.to_owned());
                        item_phase = item.get("phase").and_then(Value::as_str).map(str::to_owned);
                        message_deltas.clear();
                        streamed_current_item = false;
                    }
                }
                "item/agentMessage/delta" | "item/agentMessageDelta" => {
                    let id = params.get("itemId").and_then(Value::as_str)
                        .context("Codex agentMessage delta omitted itemId")?;
                    if item_id.as_deref().is_some_and(|expected| expected != id) { continue; }
                    let delta = params.get("delta").and_then(Value::as_str)
                        .context("Codex agentMessage delta omitted text")?;
                    message_deltas.push_str(delta);
                    if item_phase.as_deref() == Some("final_answer") {
                        if let Some(handler) = handler.as_deref_mut() {
                            handler.text(delta)?;
                            streamed_current_item = true;
                        }
                    }
                }
                "item/completed" => {
                    let item = params.get("item").and_then(Value::as_object)
                        .context("Codex item/completed omitted item")?;
                    let kind = item.get("type").and_then(Value::as_str)
                        .context("Codex item/completed item omitted type")?;
                    if is_tool_item(kind) { bail!("Codex app-server emitted tool item '{kind}'; turn stopped"); }
                    if kind == "agentMessage" {
                        let id = item.get("id").and_then(Value::as_str)
                            .context("Codex agentMessage completed without item id")?;
                        if item_id.as_deref().is_some_and(|expected| expected != id) { continue; }
                        let phase = item.get("phase").and_then(Value::as_str).or(item_phase.as_deref());
                        if !matches!(phase, Some("commentary" | "partial_answer")) {
                            let final_text = item.get("text").and_then(Value::as_str)
                                .context("Codex agentMessage completed without text")?;
                            if !message_deltas.is_empty() && message_deltas != final_text {
                                bail!("Codex app-server final agentMessage did not match streamed deltas");
                            }
                            output.push_str(final_text);
                            saw_final = true;
                            if !streamed_current_item {
                                if let Some(handler) = handler.as_deref_mut() { handler.text(final_text)?; }
                            }
                        }
                    }
                }
                "turn/completed" => {
                    let status = params.get("turn").and_then(|turn| turn.get("status")).and_then(Value::as_str).unwrap_or_default();
                    match status {
                        "completed" if saw_final => return Ok(ChatCompletionsOutput { text: output, id: Some(turn_id.clone()), ..Default::default() }),
                        "completed" => bail!("Codex app-server completed without a final agentMessage"),
                        "failed" | "interrupted" => bail!("Codex app-server turn {status}"),
                        _ => bail!("Codex app-server returned unknown terminal turn status '{status}'"),
                    }
                }
                "error" => bail!("Codex app-server reported a turn error"),
                _ => {}
            }
        }
    }.await;
    let shutdown = session.shutdown().await;
    match (result, shutdown) {
        (Ok(output), Ok(())) => Ok(output),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(error)) => Err(error),
        (Err(error), Err(shutdown)) => Err(error.context(shutdown.to_string())),
    }
}

struct ScratchDirectory {
    path: std::path::PathBuf,
}

impl ScratchDirectory {
    fn create() -> Result<Self> {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        for _ in 0..32 {
            let serial = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let path = std::env::temp_dir()
                .join(format!("rr-aichat-codex-{}-{serial}", std::process::id()));
            match std::fs::create_dir(&path) {
                Ok(()) => return Ok(Self { path }),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => {
                    return Err(error).context("Unable to create isolated Codex working directory")
                }
            }
        }
        bail!("Unable to allocate an isolated Codex working directory")
    }
}

impl Drop for ScratchDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn is_tool_item(kind: &str) -> bool {
    kind.contains("tool")
        || kind.contains("Tool")
        || matches!(
            kind,
            "commandExecution"
                | "fileChange"
                | "subAgentActivity"
                | "webSearch"
                | "imageView"
                | "imageGeneration"
                | "sleep"
                | "functionCall"
                | "hookPrompt"
        )
}

fn is_tool_or_approval(method: &str) -> bool {
    method.contains("approval")
        || method.contains("Approval")
        || method.contains("tool")
        || method.contains("Tool")
}

#[cfg(test)]
async fn run_preflight(mut session: AppServerSession, model: &str) -> Result<()> {
    let result = validate_account_and_model(&mut session, model).await;
    let shutdown_result = session.shutdown().await;
    match (result, shutdown_result) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) => Err(error),
        (Ok(()), Err(error)) => Err(error),
        (Err(error), Err(shutdown)) => Err(error.context(shutdown.to_string())),
    }
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
        self.validate_request(&data)?;
        let turn_timeout = self.turn_timeout()?;
        let reasoning = self.reasoning_settings()?;
        run_turn(
            AppServerSession::spawn(&self.executable()?).await?,
            self.model().real_name(),
            &data,
            None,
            turn_timeout,
            reasoning,
        )
        .await
    }

    async fn chat_completions_streaming_inner(
        &self,
        _client: &reqwest::Client,
        handler: &mut SseHandler,
        data: ChatCompletionsData,
    ) -> Result<()> {
        self.validate_request(&data)?;
        let turn_timeout = self.turn_timeout()?;
        let reasoning = self.reasoning_settings()?;
        let result = run_turn(
            AppServerSession::spawn(&self.executable()?).await?,
            self.model().real_name(),
            &data,
            Some(handler),
            turn_timeout,
            reasoning,
        )
        .await?;
        handler.set_response_metadata(result.id, result.input_tokens, result.output_tokens);
        Ok(())
    }
}

struct AppServerSession {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    stderr_drain: JoinHandle<usize>,
    next_id: u64,
    request_timeout: Duration,
    process_group: Option<u32>,
    pending_messages: VecDeque<Value>,
    mcp_servers: Vec<String>,
    plugins: Vec<String>,
    supported_efforts: Vec<String>,
}
impl AppServerSession {
    async fn spawn(executable: &str) -> Result<Self> {
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            let mut command = std::process::Command::new(executable);
            command.args([
                "-c",
                "features.shell_tool=false",
                "-c",
                "features.apps=false",
                "-c",
                "features.plugins=false",
                "-c",
                "features.remote_plugin=false",
                "-c",
                "features.hooks=false",
                "-c",
                "features.memories=false",
                "-c",
                "features.multi_agent=false",
                "-c",
                "features.multi_agent_v2=false",
                "-c",
                "features.code_mode=false",
                "-c",
                "features.code_mode_host=false",
                "-c",
                "features.code_mode_only=false",
                "-c",
                "features.code_mode_tool_search=false",
                "-c",
                "features.view_image=false",
                "-c",
                "features.image_generation=false",
                "-c",
                "features.browser_use=false",
                "-c",
                "features.computer_use=false",
                "-c",
                "features.skill_search=false",
                "-c",
                "features.skill_mcp_dependency_install=false",
                "-c",
                "features.tool_suggest=false",
                "-c",
                "features.sleep_tool=false",
                "-c",
                "features.current_time_reminder=false",
                "-c",
                "features.token_budget=false",
                "-c",
                "features.request_permissions_tool=false",
                "-c",
                "features.send_message_to_user_async=false",
                "-c",
                "features.shell_snapshot=false",
                "-c",
                "features.deferred_executor=false",
                "-c",
                "features.goals=false",
                "-c",
                "web_search=disabled",
                "-c",
                "project_doc_max_bytes=0",
                "-c",
                "tools.update_plan.enabled=false",
                "-c",
                "tools.experimental_request_user_input.enabled=false",
                "app-server",
                "--stdio",
            ]);
            command.process_group(0);
            Self::spawn_command(Command::from(command), true).await
        }
        #[cfg(not(unix))]
        {
            let mut command = Command::new(executable);
            command.args([
                "-c",
                "features.shell_tool=false",
                "-c",
                "features.apps=false",
                "-c",
                "features.plugins=false",
                "-c",
                "features.remote_plugin=false",
                "-c",
                "features.hooks=false",
                "-c",
                "features.memories=false",
                "-c",
                "features.multi_agent=false",
                "-c",
                "features.multi_agent_v2=false",
                "-c",
                "features.code_mode=false",
                "-c",
                "features.code_mode_host=false",
                "-c",
                "features.code_mode_only=false",
                "-c",
                "features.code_mode_tool_search=false",
                "-c",
                "features.view_image=false",
                "-c",
                "features.image_generation=false",
                "-c",
                "features.browser_use=false",
                "-c",
                "features.computer_use=false",
                "-c",
                "features.skill_search=false",
                "-c",
                "features.skill_mcp_dependency_install=false",
                "-c",
                "features.tool_suggest=false",
                "-c",
                "features.sleep_tool=false",
                "-c",
                "features.current_time_reminder=false",
                "-c",
                "features.token_budget=false",
                "-c",
                "features.request_permissions_tool=false",
                "-c",
                "features.send_message_to_user_async=false",
                "-c",
                "features.shell_snapshot=false",
                "-c",
                "features.deferred_executor=false",
                "-c",
                "features.goals=false",
                "-c",
                "web_search=disabled",
                "-c",
                "project_doc_max_bytes=0",
                "-c",
                "tools.update_plan.enabled=false",
                "-c",
                "tools.experimental_request_user_input.enabled=false",
                "app-server",
                "--stdio",
            ]);
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
            let mut captured = 0usize;
            let mut buffer = [0u8; 4096];
            loop {
                match stderr.read(&mut buffer).await {
                    Ok(0) | Err(_) => break,
                    Ok(count) => captured = captured.saturating_add(count),
                }
            }
            captured
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
            pending_messages: VecDeque::new(),
            mcp_servers: Vec::new(),
            plugins: Vec::new(),
            supported_efforts: Vec::new(),
        })
    }

    async fn initialize(&mut self) -> Result<()> {
        self.request(
            "initialize",
            json!({"clientInfo":{"name":"rr-aichat","version":env!("CARGO_PKG_VERSION")},"capabilities":{"experimentalApi":true}}),
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
            let mut message = timeout(self.request_timeout, self.read_message())
                .await
                .with_context(|| {
                    format!("Timed out waiting for Codex app-server response to '{method}'")
                })??;
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
            if message.get("method").is_some() && message.get("id").is_some() {
                self.deny_server_request(&message).await?;
                bail!("Codex app-server made an unsupported server request while waiting for '{method}'");
            }
            if message.get("id").is_some() {
                bail!("Codex app-server sent an unmatched response while waiting for '{method}'");
            }
            if message.get("method").and_then(Value::as_str).is_none() {
                bail!("Codex app-server sent an invalid notification while waiting for '{method}'");
            }
            self.pending_messages.push_back(message);
        }
        bail!("Codex app-server exceeded the notification limit while waiting for '{method}'")
    }

    async fn read_message(&mut self) -> Result<Value> {
        let line = read_protocol_line(&mut self.stdout).await?;
        serde_json::from_slice(&line).context("Codex app-server returned malformed JSON")
    }

    async fn next_message(&mut self) -> Result<Value> {
        if let Some(message) = self.pending_messages.pop_front() {
            return Ok(message);
        }
        self.read_message().await
    }

    async fn deny_server_request(&mut self, message: &Value) -> Result<()> {
        let Some(id) = message.get("id") else {
            return Ok(());
        };
        let method = message
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let response = if method == "item/dynamicToolCall" {
            json!({"id":id,"result":{"contentItems":[],"success":false}})
        } else if method.contains("permissions") || method.contains("Permissions") {
            json!({"id":id,"result":{"permissions":{},"scope":"turn"}})
        } else if method.contains("requestApproval")
            || method.contains("approval")
            || method.contains("Approval")
        {
            json!({"id":id,"result":{"decision":"decline"}})
        } else {
            json!({"id":id,"error":{"code":-32601,"message":"rr-aichat does not authorize tools or approvals"}})
        };
        let mut bytes = serde_json::to_vec(&response)?;
        bytes.push(b'\n');
        timeout(self.request_timeout, self.stdin.write_all(&bytes)).await??;
        timeout(self.request_timeout, self.stdin.flush()).await??;
        Ok(())
    }

    async fn shutdown(&mut self) -> Result<()> {
        let already_exited = self.child.try_wait()?.is_some();
        let kill_result = if !already_exited {
            self.kill_process_tree();
            if self.child.try_wait()?.is_none() {
                self.child
                    .start_kill()
                    .context("Failed to stop Codex app-server")
            } else {
                Ok(())
            }
        } else {
            Ok(())
        };
        let status = timeout(self.request_timeout, self.child.wait())
            .await
            .context("Timed out reaping Codex app-server")??;
        let stderr_bytes = match timeout(self.request_timeout, &mut self.stderr_drain).await {
            Ok(Ok(count)) => count,
            _ => {
                self.stderr_drain.abort();
                0
            }
        };
        kill_result?;
        if already_exited && !status.success() {
            bail!("Codex app-server exited unsuccessfully (status {status}; stderr diagnostics redacted, {stderr_bytes} bytes)");
        }
        Ok(())
    }
}

impl AppServerSession {
    fn kill_process_tree(&mut self) {
        if self.child.try_wait().ok().flatten().is_some() {
            self.process_group = None;
            return;
        }
        if let Some(process_group) = self.process_group.take() {
            #[cfg(unix)]
            {
                unsafe extern "C" {
                    fn kill(pid: i32, signal: i32) -> i32;
                }
                if let Ok(process_group) = i32::try_from(process_group) {
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
        .request("account/read", json!({"refreshToken":false}))
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
    session.mcp_servers = config
        .pointer("/config/mcp_servers")
        .and_then(Value::as_object)
        .map(|servers| servers.keys().cloned().collect())
        .unwrap_or_default();
    session.plugins = config
        .pointer("/config/plugins")
        .and_then(Value::as_object)
        .map(|plugins| plugins.keys().cloned().collect())
        .unwrap_or_default();
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
        if let Some(models) = response.get("data").and_then(Value::as_array) {
            if let Some(entry) = models.iter().find(|entry| {
                entry.get("model").and_then(Value::as_str) == Some(model)
                    || entry.get("id").and_then(Value::as_str) == Some(model)
            }) {
                session.supported_efforts = entry
                    .get("supportedReasoningEfforts")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(|option| {
                        option
                            .get("reasoningEffort")
                            .and_then(Value::as_str)
                            .map(str::to_owned)
                    })
                    .collect();
                return Ok(());
            }
        }
        cursor = match response.get("nextCursor") {
            Some(Value::String(cursor)) => Value::String(cursor.clone()),
            _ => bail!("Codex model '{model}' is not available in the authenticated account's model catalog"),
        };
    }
    bail!("Codex model catalog exceeded {MAX_MODEL_PAGES} pages before model '{model}' was found")
}

fn safe_tool_config(mcp_servers: &[String], plugins: &[String]) -> Value {
    let mut features = serde_json::Map::new();
    for feature in [
        "shell_tool",
        "apps",
        "plugins",
        "remote_plugin",
        "hooks",
        "memories",
        "multi_agent",
        "multi_agent_v2",
        "code_mode",
        "code_mode_host",
        "code_mode_only",
        "code_mode_tool_search",
        "view_image",
        "image_generation",
        "browser_use",
        "computer_use",
        "skill_search",
        "skill_mcp_dependency_install",
        "tool_suggest",
        "sleep_tool",
        "current_time_reminder",
        "token_budget",
        "request_permissions_tool",
        "send_message_to_user_async",
        "shell_snapshot",
        "deferred_executor",
        "goals",
    ] {
        features.insert(feature.to_owned(), Value::Bool(false));
    }
    let mcp: serde_json::Map<String, Value> = mcp_servers
        .iter()
        .map(|name| (name.clone(), json!({"enabled":false})))
        .collect();
    let plugins: serde_json::Map<String, Value> = plugins
        .iter()
        .map(|name| (name.clone(), json!({"enabled":false})))
        .collect();
    json!({
        "features":features,
        "web_search":"disabled",
        "project_doc_max_bytes":0,
        "tools":{"update_plan":{"enabled":false},"experimental_request_user_input":{"enabled":false}},
        "mcp_servers":mcp,
        "plugins":plugins
    })
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

    const TURN_SERVER: &str = r#"
while IFS= read -r line; do
  case "$line" in
    *'"method":"initialize"'*) printf '{"id":1,"result":{}}\n' ;;
    *'"method":"initialized"'*) ;;
    *'"method":"account/read"'*) printf '{"id":2,"result":{"account":{"type":"chatgpt"}}}\n' ;;
    *'"method":"config/read"'*) printf '{"id":3,"result":{"config":{"model_provider":"openai","mcp_servers":{"fixture":{"enabled":true}},"plugins":{"fixture":{"enabled":true}}}}}\n' ;;
    *'"method":"model/list"'*) printf '{"id":4,"result":{"data":[{"id":"gpt-6.1-sol","model":"gpt-6.1-sol","supportedReasoningEfforts":[{"reasoningEffort":"low","description":"low"}]}],"nextCursor":null}}\n' ;;
    *'"method":"thread/start"'*)
      printf '%s\n' "$line" > "$RR_TEST_SENTINEL.thread"
      printf '{"method":"thread/started","params":{"thread":{"id":"thread-1"}}}\n'
      printf '{"id":5,"result":{"thread":{"id":"thread-1"}}}\n'
      ;;
    *'"method":"turn/start"'*)
      printf '%s\n' "$line" > "$RR_TEST_SENTINEL.turn"
      if [ "$RR_TEST_MODE" = "stall-turn" ]; then printf '{"id":6,"result":{"turn":{"id":"turn-1"}}}\n'; continue; fi
      if [ "$RR_TEST_MODE" = "approval" ]; then
        printf '{"id":99,"method":"item/commandExecution/requestApproval","params":{"threadId":"thread-1","turnId":"turn-1"}}\n'
        IFS= read -r denied
        exit 0
      fi
      printf '{"method":"turn/started","params":{"threadId":"thread-1","turn":{"id":"turn-1"}}}\n'
      if [ "$RR_TEST_MODE" = "tool-item" ]; then
        printf '{"method":"item/started","params":{"threadId":"thread-1","turnId":"turn-1","item":{"id":"tool-1","type":"commandExecution"}}}\n'
      else
        printf '{"method":"item/started","params":{"threadId":"thread-1","turnId":"turn-1","item":{"id":"msg-1","type":"agentMessage","phase":"final_answer"}}}\n'
        printf '{"method":"item/agentMessage/delta","params":{"threadId":"thread-1","turnId":"turn-1","itemId":"msg-1","delta":"answer"}}\n'
        printf '{"method":"item/completed","params":{"threadId":"thread-1","turnId":"turn-1","item":{"id":"msg-1","type":"agentMessage","phase":"final_answer","text":"answer"}}}\n'
      fi
      status=completed
      [ "$RR_TEST_MODE" = "failed" ] && status=failed
      [ "$RR_TEST_MODE" = "interrupted" ] && status=interrupted
      printf '{"method":"turn/completed","params":{"threadId":"thread-1","turnId":"turn-1","turn":{"id":"turn-1","status":"%s"}}}\n' "$status"
      printf '{"id":6,"result":{"turn":{"id":"turn-1"}}}\n'
      ;;
    *) exit 23 ;;
  esac
done
"#;

    fn fake_executable(mode: &str, marker: &std::path::Path) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let executable = marker.with_extension("fake-codex");
        let source = format!(
            "#!/bin/sh\nexport RR_TEST_MODE='{mode}'\nexport RR_TEST_SENTINEL='{}'\necho $$ > \"$RR_TEST_SENTINEL.pid\"\n{TURN_SERVER}",
            marker.display()
        );
        fs::write(&executable, source).unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        executable
    }

    fn fixture_client(executable: &std::path::Path) -> super::CodexAppServerClient {
        super::CodexAppServerClient {
            global_config: std::sync::Arc::new(parking_lot::RwLock::new(
                crate::config::Config::default(),
            )),
            config: super::CodexAppServerConfig {
                executable: Some(executable.display().to_string()),
                ..Default::default()
            },
            model: super::Model::from_config(
                "subscription",
                &[super::ModelData::new("gpt-6.1-sol")],
            )
            .remove(0),
        }
    }

    fn fixture_data(stream: bool) -> super::ChatCompletionsData {
        super::ChatCompletionsData {
            messages: vec![
                super::Message::new(
                    super::MessageRole::System,
                    super::MessageContent::Text("system-note".into()),
                ),
                super::Message::new(
                    super::MessageRole::User,
                    super::MessageContent::Text("prior-user".into()),
                ),
                super::Message::new(
                    super::MessageRole::Assistant,
                    super::MessageContent::Text("prior-assistant".into()),
                ),
                super::Message::new(
                    super::MessageRole::User,
                    super::MessageContent::Text("current-user".into()),
                ),
            ],
            temperature: None,
            top_p: None,
            functions: None,
            stream,
        }
    }

    #[test]
    fn reasoning_patch_accepts_only_advertised_settings_shape() {
        let mut client = fixture_client(std::path::Path::new("/unused"));
        client.config.patch = Some(
            serde_json::from_value(serde_json::json!({
                "chat_completions":{".*":{"body":{"reasoning":{"effort":"low","summary":"none"}}}}
            }))
            .unwrap(),
        );
        assert_eq!(
            client.reasoning_settings().unwrap(),
            (Some("low".into()), Some("none".into()))
        );
        client.config.patch = Some(
            serde_json::from_value(serde_json::json!({
                "chat_completions":{"gpt-6.1-sol":{"body":{"model":"other"}}}
            }))
            .unwrap(),
        );
        assert!(client
            .reasoning_settings()
            .unwrap_err()
            .to_string()
            .contains("only set body.reasoning"));
    }

    #[tokio::test]
    async fn streaming_inner_returns_final_agent_message_once() {
        let marker = sentinel_path();
        let executable = fake_executable("success", &marker);
        let client = fixture_client(&executable);
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut handler = super::SseHandler::new(tx, crate::utils::AbortSignalInner::new());
        super::Client::chat_completions_streaming_inner(
            &client,
            &reqwest::Client::new(),
            &mut handler,
            fixture_data(true),
        )
        .await
        .unwrap();
        let (text, _) = handler.take();
        assert_eq!(text, "answer");
        for path in [
            marker.with_extension("thread"),
            marker.with_extension("turn"),
            executable,
        ] {
            let _ = fs::remove_file(path);
        }
    }

    #[tokio::test]
    async fn cancelling_streaming_turn_kills_owned_server() {
        let marker = sentinel_path();
        let executable = fake_executable("stall-turn", &marker);
        let client = fixture_client(&executable);
        let task = tokio::spawn(async move {
            let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
            let mut handler = super::SseHandler::new(tx, crate::utils::AbortSignalInner::new());
            super::Client::chat_completions_streaming_inner(
                &client,
                &reqwest::Client::new(),
                &mut handler,
                fixture_data(true),
            )
            .await
        });
        tokio::time::timeout(Duration::from_secs(3), async {
            while !marker.with_extension("turn").exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let pid = fs::read_to_string(marker.with_extension("pid")).unwrap();
        task.abort();
        let _ = task.await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        let status = Command::new("sh")
            .arg("-c")
            .arg("kill -0 \"$1\" 2>/dev/null")
            .arg("rr-aichat-process-check")
            .arg(pid.trim())
            .status()
            .await
            .unwrap();
        assert!(
            !status.success(),
            "cancelled app-server process is still running"
        );
        for path in [
            marker.with_extension("thread"),
            marker.with_extension("turn"),
            marker.with_extension("pid"),
            executable,
        ] {
            let _ = fs::remove_file(path);
        }
    }

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
