# Security Policy

## Reporting a vulnerability

Please report security issues **privately**. Do not open a public issue, pull request or discussion for them.

Use GitHub private vulnerability reporting: open the repository's **Security** tab and choose **Report a vulnerability**, or go to
<https://github.com/harishsubramanian2577/mcp-a2a-bridge-policy/security/advisories/new>.

Please include:
- the affected version (the `version` in `Cargo.toml`) and the policy configuration, with secrets removed;
- the steps or requests that reproduce the issue, and what you expected instead;
- the impact you see, for example session takeover or data from another API instance.

Reports are acknowledged within 5 business days. Fixes are released as a new policy version, and the advisory is published once a fixed version is available.

## Supported versions

| Version | Supported |
|---|---|
| 1.3.x | Yes |
| 1.1.x | No. Upgrade to 1.3.0, in which an ended or unknown session gets `404` and can't be revived ([#5](https://github.com/harishsubramanian2577/mcp-a2a-bridge-policy/issues/5)). |
| 1.0.x | No. Upgrade to 1.1.0, which fixes the session isolation findings in [#12](https://github.com/harishsubramanian2577/mcp-a2a-bridge-policy/issues/12). |
