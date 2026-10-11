# rr-aichat

[![CI](https://github.com/ramarivera/aichat/actions/workflows/ci.yaml/badge.svg)](https://github.com/ramarivera/aichat/actions/workflows/ci.yaml)
[![Crates](https://img.shields.io/crates/v/rr-aichat.svg)](https://crates.io/crates/rr-aichat)

rr-aichat is Ramiro Rivera's independently versioned fork of [AIChat](https://github.com/sigoden/aichat). It provides an LLM CLI, shell assistant, chat REPL, RAG, tools and agents.

## Attribution

The original source was written by sigoden and the [AIChat contributors](https://github.com/sigoden/aichat/graphs/contributors). This fork starts from upstream commit [`82976d3`](https://github.com/sigoden/aichat/commit/82976d349ad97ac9aae0655ad631dace5e2a6385), whose package version is 0.30.0.

The features described below and the screenshots come from upstream AIChat. Its companion tool and agent repository is [sigoden/llm-functions](https://github.com/sigoden/llm-functions).

Upstream license files (`LICENSE-APACHE`, `LICENSE-MIT`) and copyright are preserved; rr-aichat remains under the same MIT OR Apache-2.0 terms.

### Versioning

Version 1.0.0 is this fork's first release, not an upstream AIChat 1.0.0 release. Future releases use this fork's own version numbers.

### What this fork changes

Version 1.0.0 renames the package and executable to `rr-aichat`. It uses `RR_AICHAT_*` environment variables and a separate `rr-aichat` configuration directory, such as `~/.config/rr-aichat` on Linux. Provider API-key names remain unchanged.

The upstream wiki still uses `aichat`, `AICHAT_*` and the `aichat` configuration directory. Substitute this fork's names when following those guides. Model synchronization still uses upstream's model catalog by default.

### 1.1.0 adapters

The fork adds native OpenAI Responses and one-shot Codex subscription clients. Responses uses API credentials; Codex delegates authentication and inference to the existing Codex ChatGPT login.

#### OpenAI Responses

```yaml
model: responses:gpt-4.1
function_calling: false
clients:
  - type: openai-responses
    name: responses
    api_base: https://api.openai.com/v1
    timeout_seconds: 180
```

Set `RESPONSES_API_KEY` or `OPENAI_API_KEY` in your environment. Credentials resolve in this order: the uppercased client-name `*_API_KEY` variable, the client's configured `api_key`, then `OPENAI_API_KEY`. OpenAI Platform API usage is separately billed; a ChatGPT subscription doesn't pay for these calls.

```nu
rr-aichat --model responses:gpt-4.1 "Reply with hello"
rr-aichat --model responses:gpt-4.1 --no-stream "Reply with hello"
with-env {RR_AICHAT_SHELL: "nu"} {
  rr-aichat --model responses:gpt-4.1 --role "%shell%" "Suggest a command without running it"
}
```

`api_base` is the API root, not the `/responses` URL. Compatible endpoints can omit credentials. Client-specific base URL, organization and timeout overrides use `RR_AICHAT_<CLIENT>_API_BASE`, `RR_AICHAT_<CLIENT>_ORGANIZATION_ID` and `RR_AICHAT_<CLIENT>_TIMEOUT_SECONDS`. Timeout is 1 to 3600 seconds, default 180; existing proxy and connect-timeout settings still apply.

The adapter supports text history, system instructions, streaming, function calls/results, response IDs and token usage. Reasoning remains separate from final text. Use existing `patch.chat_completions` body settings for a reasoning-capable model, for example `reasoning: {effort: low}`. URL/header patches are supported, so only configure trusted endpoints.

Every request sends the complete local history once and uses `store: false`. It never chains `previous_response_id` or a server conversation. This doesn't override a provider's logging policy. Patches can't change input, instructions, model, tools, stream, storage or server conversation/background fields. Images, audio, embeddings, reranking and server built-in tools aren't supported by this adapter.

The local HTTP proxy remains a Chat Completions API. Responses streaming adds a final usage chunk with `upstream_response_id`; the proxy's completion ID stays stable across its chunks. An incomplete/failed upstream response produces an error, not a successful `stop` result.

#### Codex subscription

```yaml
model: subscription:gpt-6.1-sol
function_calling: false
temperature: null
top_p: null
clients:
  - type: codex-app-server
    name: subscription
    executable: codex
    timeout_seconds: 180
    patch:
      chat_completions:
        'gpt-6.1-sol':
          body:
            reasoning: {effort: low, summary: none}
```

Use your existing `codex login` with "Sign in with ChatGPT". Codex 0.162.0 is the minimum tested version. The stdio client checks the managed account, the effective OpenAI provider and the requested model. Missing login, API-key accounts, other providers and unavailable models produce errors. It never reads stored tokens or falls back to separately billed API usage. Unreleased source checks the cached account with `account/read` and `refreshToken: false`, rather than forcing a refresh on every invocation. Token renewal remains Codex's responsibility; published 1.1.0 still requests a proactive refresh.

Each request creates a fresh ephemeral thread in an isolated working directory and starts exactly one turn. The complete local transcript is supplied once, with system instructions separate from user/assistant history. AIChat owns any saved history; the adapter doesn't resume Codex threads. Final answer deltas stream when their phase is known; commentary and reasoning aren't returned as answer text. Failed/interrupted turns, malformed protocol messages, timeouts and unexpected tools stop the request. Cancellation stops the owned subprocess.

Codex can still load global instructions from its existing `CODEX_HOME`. An isolated working directory and custom system instructions don't exclude those files.

The client disables available shell, web, MCP, plugin, hook, memory and agent features, requests a read-only sandbox without network access, denies approval/tool requests and asks for text-only answers. Managed requirements still apply. This is a command-review boundary, **not proof that every Codex tool is absent**. The [0.162.0 protocol](https://github.com/openai/codex/blob/rust-v0.162.0/codex-rs/app-server-protocol/src/protocol/v2/thread.rs) has no universal tool-disable field; its [tool registration](https://github.com/openai/codex/blob/rust-v0.162.0/codex-rs/core/src/tools/spec_plan.rs) includes independently registered built-ins.

Only reasoning effort and summary patches are supported. Effort must appear in the selected model's catalog; summary accepts `auto`, `concise`, `detailed` or `none`. Temperature, `top_p`, output token limits, images, rr-aichat function calls/history, embeddings and reranking aren't supported. Executable and turn-timeout overrides are `RR_AICHAT_CODEX_APP_SERVER_EXECUTABLE` and `RR_AICHAT_CODEX_APP_SERVER_TIMEOUT_SECONDS`. The default turn timeout is 180 seconds; zero is invalid. Subscription usage follows the Codex account plan.

```nu
rr-aichat --model subscription:gpt-6.1-sol "Reply with hello"
rr-aichat --model subscription:gpt-6.1-sol --no-stream "Reply with hello"
with-env {RR_AICHAT_SHELL: "nu"} {
  rr-aichat --model subscription:gpt-6.1-sol --execute "Suggest a command for review"
}
```

Returned commands remain text. On a terminal, `--execute` shows the command and waits for AIChat's existing execution action, `e` or Enter. `q` quits without execution. With nonterminal stdout it only prints the suggestion. Nushell Alt+E fills the editor without submitting it, and `RR_AICHAT_SHELL` selects Nushell or zsh. The existing `--execute`/code-only paths use nonstreaming transport even when normal chat streaming is enabled.


## Install

### Cargo, after publication

```nu
cargo install rr-aichat --locked
```

### From source

```nu
git clone https://github.com/ramarivera/aichat
cd aichat
cargo install --path . --locked
```

Upstream's Homebrew, pacman, Scoop and Termux packages install AIChat, not this fork.

### Publishing

See [docs/releasing.md](docs/releasing.md) for `cargo login` / `cargo publish` instructions.

Pushing a new `vX.Y.Z` tag starts the release workflow. Commit and push the matching package version first; stable tags publish to crates.io after all platform builds pass.

Manual dispatch can recover a missed trigger for an existing unpublished tag. This command recovered `v1.0.1` before it was published:

```nu
gh workflow run release.yaml --repo ramarivera/aichat --ref main -f tag=v1.0.1
```

The workflow checks that the tag matches the package version. Don't dispatch or push a stable tag for a version that's already published.

Tag publishing passed end to end with [`v1.0.3`](https://github.com/ramarivera/aichat/actions/runs/38142275933) on 2026-10-11. The push-triggered run built all 10 platform archives and published [`rr-aichat 1.0.3`](https://crates.io/crates/rr-aichat/1.0.3) from commit [`6d2f426`](https://github.com/ramarivera/aichat/commit/6d2f426). The Linux archive matched GitHub's SHA-256 digest; its executable and a fresh registry installation passed version and offline prompt checks. The cause of the earlier missed tag triggers wasn't confirmed.

## Features

### Multi-Providers

Integrate seamlessly with over 20 leading LLM providers through a unified interface. Supported providers include OpenAI, Claude, Gemini (Google AI Studio), Ollama, Groq, Azure-OpenAI, VertexAI, Bedrock, Github Models, Mistral, Deepseek, AI21, XAI Grok, Cohere, Perplexity, Cloudflare, OpenRouter, Ernie, Qianwen, Moonshot, ZhipuAI, MiniMax, Deepinfra, VoyageAI, any OpenAI-Compatible API provider.

### CMD Mode

Explore powerful command-line functionalities with rr-aichat's CMD mode.

![aichat-cmd](https://github.com/user-attachments/assets/6c58c549-1564-43cf-b772-e1c9fe91d19c)

### REPL Mode

Experience an interactive Chat-REPL with features like tab autocompletion, multi-line input support, history search, configurable keybindings, and custom REPL prompts.

![aichat-repl](https://github.com/user-attachments/assets/218fab08-cdae-4c3b-bcf8-39b6651f1362)

### Shell Assistant

Elevate your command-line efficiency. Describe your tasks in natural language, and let rr-aichat transform them into precise shell commands. rr-aichat intelligently adjusts to your OS and shell environment.

![aichat-execute](https://github.com/user-attachments/assets/0c77e901-0da2-4151-aefc-a2af96bbb004)

### Multi-Form Input

Accept diverse input forms such as stdin, local files and directories, and remote URLs, allowing flexibility in data handling.

| Input             | CMD                                     | REPL                             |
| ----------------- | --------------------------------------- | -------------------------------- |
| CMD               | `rr-aichat hello`                       |                                  |
| STDIN             | `cat data.txt \| rr-aichat`             |                                  |
| Last Reply        |                                         | `.file %%`                       |
| Local files       | `rr-aichat -f image.png -f data.txt`    | `.file image.png data.txt`       |
| Local directories | `rr-aichat -f dir/`                     | `.file dir/`                     |
| Remote URLs       | `rr-aichat -f https://example.com`      | `.file https://example.com`      |
| External commands | ```rr-aichat -f '`git diff`'```         | ```.file `git diff` ```          |
| Combine Inputs    | `rr-aichat -f dir/ -f data.txt explain` | `.file dir/ data.txt -- explain` |

### Role

Customize roles to tailor LLM behavior, enhancing interaction efficiency and boosting productivity.

![aichat-role](https://github.com/user-attachments/assets/023df6d2-409c-40bd-ac93-4174fd72f030)

> The role consists of a prompt and model configuration.

### Session

Maintain context-aware conversations through sessions, ensuring continuity in interactions.

![aichat-session](https://github.com/user-attachments/assets/56583566-0f43-435f-95b3-730ae55df031)

> The left side uses a session, while the right side does not use a session.

### Macro

Streamline repetitive tasks by combining a series of REPL commands into a custom macro.

![aichat-macro](https://github.com/user-attachments/assets/23c2a08f-5bd7-4bf3-817c-c484aa74a651)

### RAG

Integrate external documents into your LLM conversations for more accurate and contextually relevant responses.

![aichat-rag](https://github.com/user-attachments/assets/359f0cb8-ee37-432f-a89f-96a2ebab01f6)

### Function Calling

Function calling supercharges LLMs by connecting them to external tools and data sources. This unlocks a world of possibilities, enabling LLMs to go beyond their core capabilities and tackle a wider range of tasks.

The upstream project maintains a companion repository [https://github.com/sigoden/llm-functions](https://github.com/sigoden/llm-functions) to help you make the most of this feature.

#### AI Tools & MCP

Integrate external tools to automate tasks, retrieve information, and perform actions directly within your workflow.

![aichat-tool](https://github.com/user-attachments/assets/7459a111-7258-4ef0-a2dd-624d0f1b4f92)

#### AI Agents (CLI version of OpenAI GPTs)

AI Agent = Instructions (Prompt) + Tools (Function Callings) + Documents (RAG).

![aichat-agent](https://github.com/user-attachments/assets/0b7e687d-e642-4e8a-b1c1-d2d9b2da2b6b)

### Local Server Capabilities

rr-aichat includes a lightweight built-in HTTP server for easy deployment.

```
$ rr-aichat --serve
Chat Completions API: http://127.0.0.1:8000/v1/chat/completions
Embeddings API:       http://127.0.0.1:8000/v1/embeddings
Rerank API:           http://127.0.0.1:8000/v1/rerank
LLM Playground:       http://127.0.0.1:8000/playground
LLM Arena:            http://127.0.0.1:8000/arena?num=2
```

#### Proxy LLM APIs

The LLM Arena is a web-based platform where you can compare different LLMs side-by-side. 

Test with curl:

```sh
curl -X POST -H "Content-Type: application/json" -d '{
  "model":"claude:claude-3-5-sonnet-20240620",
  "messages":[{"role":"user","content":"hello"}], 
  "stream":true
}' http://127.0.0.1:8000/v1/chat/completions
```

#### LLM Playground

A web application to interact with supported LLMs directly from your browser.

![aichat-llm-playground](https://github.com/user-attachments/assets/aab1e124-1274-4452-b703-ef15cda55439)

#### LLM Arena

A web platform to compare different LLMs side-by-side.

![aichat-llm-arena](https://github.com/user-attachments/assets/edabba53-a1ef-4817-9153-38542ffbfec6)

## Custom Themes

rr-aichat supports custom dark and light themes, which highlight response text and code blocks.

![aichat-themes](https://github.com/sigoden/aichat/assets/4012553/29fa8b79-031e-405d-9caa-70d24fa0acf8)

## Upstream documentation

These guides are maintained by the upstream AIChat project. Remember to substitute `rr-aichat` / `RR_AICHAT_*` / the `rr-aichat` config directory as noted above.

- [Chat-REPL Guide](https://github.com/sigoden/aichat/wiki/Chat-REPL-Guide)
- [Command-Line Guide](https://github.com/sigoden/aichat/wiki/Command-Line-Guide)
- [Role Guide](https://github.com/sigoden/aichat/wiki/Role-Guide)
- [Macro Guide](https://github.com/sigoden/aichat/wiki/Macro-Guide)
- [RAG Guide](https://github.com/sigoden/aichat/wiki/RAG-Guide)
- [Environment Variables](https://github.com/sigoden/aichat/wiki/Environment-Variables)
- [Configuration Guide](https://github.com/sigoden/aichat/wiki/Configuration-Guide)
- [Custom Theme](https://github.com/sigoden/aichat/wiki/Custom-Theme)
- [Custom REPL Prompt](https://github.com/sigoden/aichat/wiki/Custom-REPL-Prompt)
- [FAQ](https://github.com/sigoden/aichat/wiki/FAQ)

## Release notes

### 1.0.3

- Release to verify automatic version-tag builds and crates.io publishing.
- No CLI functionality changes.

### 1.0.2, tagged but unpublished

- Pushed `v1.0.2` to test automatic publishing. No Actions run was created, and the version remains unpublished.
- No CLI functionality changes.

### 1.0.1

- Published through manual GitHub Actions recovery after tag pushes created no run.
- No CLI functionality changes.

### 1.0.0

- First release of the rr-aichat fork.
- Renamed the package, executable, shell completions and integrations.
- Separate configuration directory and environment-variable prefix.
- Preserved the upstream LLM features and licenses.
- Updated the async macro dependencies and redundant source expressions for Rust 1.99 Clippy compatibility.
- Replaced the yanked `slab` lockfile version with a compatible patch release.

## License

Copyright (c) 2023-2025 aichat-developers.

This project is made available under the terms of either the MIT License or the Apache License 2.0, at your option.

See the LICENSE-APACHE and LICENSE-MIT files for license details.
