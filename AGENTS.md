# Scrutivue working agreements

- Keep all application-facing text in English: labels, menus, tooltips, accessibility text, dialogs, help, errors and CLI messages. Write new code comments and documentation in English as well. Existing historical validation notes do not need translation unless they are part of the requested change.
- Preserve user work, including uncommitted and untracked feature changes. Keep shared-file edits narrow; do not revert unrelated work or commit unless asked.
- Keep the exact Redlamp LibRaw fork pinned by `config/libraw.json` (commit `f4956c404fba0c6035ce9e06353d4b4ffe068fc7`, with its recorded SHA-256). Do not substitute upstream LibRaw, change the pin, or modify the neighboring Redlamp checkout without explicit permission.
- Do not overwrite, replace or remove repository-root application binaries, including the legacy `lumen-diff.exe`, without an explicit user request. Build outputs belong under `target/`; ship the matching ONNX Runtime library with them. Do not commit generated EXE/DLL files.
- Prefer a best-effort release build after meaningful implementation changes when time and the environment allow. It need not block every small change; report skipped or failed validation honestly, and avoid redundant concurrent release builds. An explicit request not to build takes precedence.
- When the user asks for subagents or delegated/parallel agent work, delegate suitable independent tasks with clear ownership and integrate their results. Do not require delegation for ordinary tasks where it was not requested; follow any narrower harness or session restrictions.
- Keep filesystem access, decoding, shell integration and expensive image processing off the UI thread. Source images must remain unchanged.
