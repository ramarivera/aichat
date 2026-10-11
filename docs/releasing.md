# Publishing rr-aichat 1.0.0

Requested outcome: release this fork as the `rr-aichat` crate and executable at version `1.0.0`, credit the original source in the README, verify the release package, then commit and push. The initial request left crates.io publication to the maintainer. Ramiro subsequently confirmed Cargo was logged in and authorized publication from this clone.

## Preparation checklist

- [x] Rename the package and executable and update affected scripts and metadata.
- [x] Document the fork, original author, upstream repository and source revision; preserve both license files.
- [x] Verify the packaged build and exercise the CLI without a provider account.
- [ ] Commit and push the prepared release to the fork.
- [ ] Publish rr-aichat 1.0.0 and verify its crates.io record.

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
