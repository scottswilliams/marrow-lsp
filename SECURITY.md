# Security Policy

## Supported Versions

marrow-lsp is pre-release software. Until the v0.1 release, security fixes are handled on the main
development line and may ship without backports or patch releases.

After v0.1, this policy will be updated with the supported release line and any maintenance
expectations.

## Reporting A Vulnerability

Do not open a public GitHub issue or discussion for a suspected vulnerability.

Email reports to williamssscott@gmail.com. Include enough detail to reproduce or assess the issue:

- the affected marrow-lsp version, commit, or branch;
- the editor, extension, LSP, MCP, DAP, fixture, project files, or commands involved;
- the expected behavior and the observed behavior;
- any crash output, diagnostics, logs, or proof-of-concept steps that are safe to share.

Useful reports include editor, language-server, MCP, debugger, workspace-file handling, generated
artifact, and process-launch issues that could affect confidentiality, integrity, availability,
project isolation, or safe handling of untrusted inputs.

## Response Expectations

You should receive a human response after the report has been reviewed. Follow-up may ask for a
smaller reproduction or clarification. Fix timing depends on severity, reproducibility, and the
pre-release state of the affected surface; no hard response or disclosure SLA is promised for v0.1
pre-release work.
