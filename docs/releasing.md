# Releasing rr-aichat

Release preparation, publishing procedures and verification receipts for the independently maintained `rr-aichat` crate and executable. Preserve upstream attribution and both license files.

## Preparation checklist

- [x] Rename the package and executable and update affected scripts and metadata.
- [x] Document the fork, original author, upstream repository and source revision; preserve both license files.
- [x] Verify the packaged build and exercise the CLI without a provider account.
- [x] Commit and push the prepared release to the fork.
- [x] Publish rr-aichat 1.0.0 and verify its crates.io record.

## Maintainer commands

Run these from the repository root after preparation is complete:

```nu
cargo login
cargo publish --locked
```

Use a crates.io API token that can create and publish `rr-aichat`. Never commit the token. Publication requires the crate name to remain available and your account to satisfy crates.io's current requirements.

## Tag-triggered publishing

The existing GitHub Actions release workflow is retained. A pushed version tag such as `v1.0.0` builds `rr-aichat` archives, then publishes to crates.io using the repository's `CRATES_IO_API_TOKEN` secret. No version tag is pushed during preparation.

Choose one publishing route for each version. If you publish 1.0.0 manually, pushing `v1.0.0` afterward also invokes the publish job, which will fail because crates.io versions cannot be overwritten.

## Verification

From the repository root:

```nu
cargo test --locked --all
cargo clippy --locked --all --all-targets -- -D warnings
cargo fmt --all --check
cargo publish --dry-run --locked
```

The dry run verifies the archive without uploading it. Use the publishing commands only with authorization for that release. An existing Cargo login is sufficient; there is no need to log in again or inspect the stored token.

Verified locally for 1.0.0:

- All 21 existing tests passed, Clippy passed with warnings denied, and formatting passed.
- `cargo publish --dry-run --locked --allow-dirty` built the archive successfully without an upload.
- The packaged binary reported `rr-aichat 1.0.0`; help, model listing and an offline prompt dry run worked.
- Configuration used the separate `rr-aichat` directory, accepted `RR_AICHAT_CONFIG_DIR` and ignored upstream's `AICHAT_CONFIG_DIR`.
- The local HTTP server returned the model catalog at `/v1/models`.
- The archive contained the renamed executable target, embedded assets and unchanged MIT and Apache license files.
- Bash and Zsh syntax checks passed; Nushell loaded the completion and integration scripts. Fish and PowerShell weren't installed on the verification host.

## Release receipt

- Published [rr-aichat 1.0.0](https://crates.io/crates/rr-aichat/1.0.0) on 2026-10-11 from this clone.
- Published source commit: [`0ffb854`](https://github.com/ramarivera/aichat/commit/0ffb854), pushed to `origin/main`.
- `cargo publish --locked` verified and uploaded the package, then confirmed registry availability.
- `cargo info rr-aichat@1.0.0 --registry crates-io`, run outside this repository, confirmed the registry version, licenses and fork repository URL.
- A fresh `cargo install rr-aichat --version 1.0.0 --locked` into a scratch installation root built the registry release; its executable reported `rr-aichat 1.0.0`.
- No version tag was pushed.

## Automation verification release 1.0.1

Version 1.0.1 tested the retained publishing workflow with the repository's `CRATES_IO_API_TOKEN` secret.

- [x] Prepare and verify package version 1.0.1 without changing CLI functionality.
- [ ] Commit the release and push `v1.0.1`.
- [ ] Verify the tag-triggered cross-platform builds and GitHub release archives.
- [ ] Verify GitHub Actions publishes 1.0.1 to crates.io and install the registry package.

Do not publish 1.0.1 locally. Publication must come from the retained GitHub Actions workflow to prove that route works.

## 1.1.0 adapter release

The Codex client supports one-shot subscription inference with available tool suppression. Returned commands remain text until AIChat's explicit execution action. A universal Codex no-tools guarantee isn't claimed.

The Responses client uses stateless local history and API credentials. The Codex client uses existing ChatGPT authentication, a fresh ephemeral thread and exactly one turn per request. Reasoning settings are optional; unsupported capabilities and unexpected tools fail explicitly.

- Real Codex 0.162.0 subscription inference passed.
- Native chat passed with streaming and nonstreaming transport.
- Nushell and zsh command generation returned shell-specific suggestions without executing them. The command/code-only CLI paths use nonstreaming transport.
- The PTY fixture exercised the actual execution UI. Before the review key, no command ran; `q` left the sentinel absent, while `e` created it with the expected contents.
- Responses real-account verification remains fixture-only. The earlier ambient API credential was rejected; no token was printed.
- Live Alt+E editor interaction, Fish, PowerShell and other platform runtime behavior weren't exercised.

- All 65 tests passed, Clippy passed with warnings denied, and Cargo/rustfmt checks passed. The explicit client-file formatting check covers modules registered through macros.
- `cargo package --locked --allow-dirty` verified the revised archive. The packaged binary completed real subscription chat in both modes and served native Chat Completions JSON/SSE successfully.
- The packaged proxy rejected an unsupported `max_tokens` limit with HTTP 400. Streaming deltas assembled to `PROXY_STREAM_OK`, followed by a successful terminal chunk and `[DONE]`.
- Source and package verification used separate Cargo target directories so an extracted package couldn't replace the checkout's executable.

Source version is 1.1.0. Publication uses exactly one GitHub Actions version tag, not local `cargo publish`. Don't push a duplicate tag for a published version.

`cargo fmt --all --check` doesn't traverse client modules registered through `register_client!`; also run `rustfmt --edition 2021 --check src/client/openai_responses.rs src/client/codex_app_server.rs` for these additions.


### Published 1.1.0 receipt

- Released source [`9375a08fd5fdcca35e2ef58ef83e4c33d9abb4d4`](https://github.com/ramarivera/aichat/commit/9375a08fd5fdcca35e2ef58ef83e4c33d9abb4d4), pushed to `main`.
- Pushed `v1.1.0` exactly once. [GitHub Actions run 38152869206](https://github.com/ramarivera/aichat/actions/runs/38152869206) completed successfully, including all ten archive targets and the crates.io publish job. No local publication or workflow dispatch was used.
- [GitHub release v1.1.0](https://github.com/ramarivera/aichat/releases/tag/v1.1.0) contains ten uploaded archives. Cross-platform builds succeeded; non-Linux runtime behavior wasn't exercised.
- `cargo info rr-aichat@1.1.0 --registry crates-io`, run outside the checkout, confirmed [registry version 1.1.0](https://crates.io/crates/rr-aichat/1.1.0). The downloaded crate's `.cargo_vcs_info.json` matches the released source commit.
- A fresh `cargo install rr-aichat --version 1.1.0 --locked` reported `rr-aichat 1.1.0`.
- The registry-installed binary completed real subscription inference in both streaming modes.


## 1.1.1 account-preflight release

The Codex adapter now requests `account/read` with `refreshToken: false`. It no longer asks for an unconditional token refresh on every invocation. Existing subscription-only account validation remains in place, and token renewal stays with Codex.

The rebuilt source binary completed real native ChatGPT subscription inference. All 65 tests, Cargo formatting, explicit client rustfmt checks and Clippy with warnings denied passed. Forced token-expiration recovery wasn't exercised.

Published through exactly one `v1.1.1` tag using the existing GitHub Actions workflow. No local upload, manual workflow dispatch or duplicate tag push was used.

This patch doesn't prevent Codex's global instruction loading.

### Published 1.1.1 receipt

- Released source [`1f65ff3e0fa3a4bcfa4a66e10eef181696a9c4bf`](https://github.com/ramarivera/aichat/commit/1f65ff3e0fa3a4bcfa4a66e10eef181696a9c4bf) includes the native auth fix from `213f436`.
- [GitHub Actions run 38167344904](https://github.com/ramarivera/aichat/actions/runs/38167344904) completed all ten archive targets and crates.io publication successfully. [GitHub release v1.1.1](https://github.com/ramarivera/aichat/releases/tag/v1.1.1) contains ten assets.
- `cargo info rr-aichat@1.1.1 --registry crates-io` confirmed [registry version 1.1.1](https://crates.io/crates/rr-aichat/1.1.1). Its `.cargo_vcs_info.json` matches the released source above; its Codex account preflight sends `refreshToken: false`.
- A fresh `cargo install rr-aichat --version 1.1.1 --locked` reported `rr-aichat 1.1.1`.
- The registry-installed binary completed real native ChatGPT subscription inference with streaming and nonstreaming transport, with the native client explicitly selected.
- Cross-platform builds succeeded; non-Linux runtime and forced token-expiration recovery weren't exercised.
