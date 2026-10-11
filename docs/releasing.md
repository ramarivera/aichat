# Publishing rr-aichat 1.0.0

Requested outcome: release this fork as the `rr-aichat` crate and executable at version `1.0.0`, credit the original source in the README, verify the release package, then commit and push. The initial request left crates.io publication to the maintainer. Ramiro subsequently confirmed Cargo was logged in and authorized publication from this clone.

## Preparation checklist

- [x] Rename the package and executable and update affected scripts and metadata.
- [x] Document the fork, original author, upstream repository and source revision; preserve both license files.
- [x] Verify the packaged build and exercise the CLI without a provider account.
- [x] Commit and push the prepared release to the fork.
- [x] Publish rr-aichat 1.0.0 and verify its crates.io record.

## Maintainer commands

Run these from the repository root after preparation is complete:

```nu
cd ~/dev/aichat
$env.CARGO_TARGET_DIR = ($env.HOME | path join "dev" "agentic-scratchpads" "aichat" "main" "target")
cargo login
cargo publish --locked
```

Use a crates.io API token that can create and publish `rr-aichat`. Never commit the token. Publication requires the crate name to remain available and your account to satisfy crates.io's current requirements.

## Tag-triggered publishing

The existing GitHub Actions release workflow is retained. A pushed version tag such as `v1.0.0` builds `rr-aichat` archives, then publishes to crates.io using the repository's `CRATES_IO_API_TOKEN` secret. No version tag is pushed during preparation.

Choose one publishing route for each version. If you publish 1.0.0 manually, pushing `v1.0.0` afterward also invokes the publish job, which will fail because crates.io versions cannot be overwritten.

## Verification

From the repository root, with `CARGO_TARGET_DIR` set as above:

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

Ramiro authorized a dummy release to test the publishing workflow after configuring `CRATES_IO_API_TOKEN`. The requested `1.0.0.1` isn't valid SemVer; this test uses the patch version `1.0.1`.

- [x] Prepare and verify package version 1.0.1 without changing CLI functionality.
- [ ] Commit the release and push `v1.0.1`.
- [ ] Verify the tag-triggered cross-platform builds and GitHub release archives.
- [ ] Verify GitHub Actions publishes 1.0.1 to crates.io and install the registry package.

Do not publish 1.0.1 locally. Publication must come from the retained GitHub Actions workflow to prove that route works.

## Unreleased 1.1.0 adapter work

Requested outcome is a native Responses adapter plus safe Codex inference through the existing ChatGPT subscription login: both adapters, real subscription inference, verified source/package, then one tag-driven publication and a fresh registry installation.

Status is incomplete. The native Responses adapter and safe Codex stdio/account/model preflight are implemented. Installed Codex 0.162.0 has no verified complete mechanism to disable all agent tools. The subscription client therefore refuses before `thread/start` or `turn/start`; read-only sandboxing and denied approvals aren't substitutes.

- Source version is prepared as 1.1.0, not published.
- Fixture tests and actual Nushell wrappers exercised streaming/nonstreaming Responses, reviewed command output and history isolation.
- The installed Codex returned the safety refusal in both CLI modes after account/model preflight. No inference turn was sent.
- All 53 tests passed, including the regression that requires streamed text before upstream ID/usage metadata. Clippy passed with warnings denied, and Cargo/rustfmt checks passed. Final verification used raw Cargo subprocesses after the Bash command route exercised a stale executable.
- `cargo package --locked --allow-dirty` verified the 89-file archive. A separate installation from that final local package reported `rr-aichat 1.1.0`; both CLI streaming modes and HTTP proxy event ordering passed fixture smoke checks. This isn't a registry installation.
- The ambient Responses credential was rejected by the real API probe. Successful Responses inference verification is fixture-only; no token was printed.
- Bash/zsh syntax checks and Nushell completion/Alt+E binding loading passed. Live editor interaction, Fish, PowerShell and other platform builds weren't exercised.
- Real subscription inference, Codex streaming/history and the full handoff's release acceptance remain blocked.
- Don't push `v1.1.0`, dispatch its publishing workflow or claim both adapters work until the safety contract and real inference acceptance are met.

Retain the GitHub Actions tag route for eventual publication. Don't publish locally, and don't push a duplicate tag for a published version.

`cargo fmt --all --check` doesn't traverse client modules registered through `register_client!`; also run `rustfmt --edition 2021 --check src/client/openai_responses.rs src/client/codex_app_server.rs` for these additions.
