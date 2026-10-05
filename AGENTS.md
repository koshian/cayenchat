Reply to the user in Japanese.

Read relevant files under `spec/` before working, and update them when necessary.

Do not overwrite or commit unrelated user changes. Keep PR diffs minimal and avoid unnecessary changes outside the task scope.

Create a new worktree under `./.worktrees/` for each task. Do not reuse existing worktrees or branches without permission.

Use relative paths for files under the current working directory. Do not unnecessarily convert them to absolute paths or resolve symlinks.

Prefer native edit/patch tools for file changes. Do not use Python, Perl, Ruby, sed, awk, or temporary scripts merely to rewrite files.

Run the project's standard formatter before committing.

Commit completed work in small, concern-specific commits with concise English imperative messages.

Use relative paths for files under the current working directory.
Do not unnecessarily access files outside the working directory.
Do not write to /tmp directly. Put temporary files, test homes, build caches and
E2E outputs under `target/tmp/` in your current worktree.

Builds, test suites and GUI checks often take longer than a shell tool's default command timeout (two minutes). Set the timeout explicitly to cover the expected duration, or use no timeout, instead of letting the command be cut off and retrying it.

## Reviewing pull requests

When reviewing, cover these project-specific concerns in addition to general correctness:

- Untrusted input: everything from IRC servers and other users (nicks, channel names, message text, CTCP, IRCv3 tags and metadata, avatars), fetched URLs and images, and settings files read from disk. Look for panics on malformed input, unbounded growth, spoofed display (IRC formatting codes, bidirectional control characters) and server values reinterpreted as templates or commands. Known open findings are in `spec/security-review-2026-09-26.md`.
- Secrets: server and SASL passwords and upload API keys stay in `storage::credentials` and never reach the preferences file, logs, the wire transcript, the clipboard or error messages unmasked.
- Performance: per-message and per-event paths, log rendering and scrolling, and anything done on every frame. Stay event driven, keep logs, queues and caches bounded, and never block the UI thread. Follow `spec/performance.md`, including its baseline comparison for changes to these paths.
- Design: IRC and network logic must not depend on GPUI, and each crate keeps the responsibilities in `spec/architecture.md`. Respect earlier choices in `spec/decisions.md`, reuse existing helpers instead of duplicating them, and question new abstractions the change does not need.
