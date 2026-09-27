# Security

## Reporting a vulnerability

Please report security problems privately through GitHub: open the repository's
**Security** tab and choose **Report a vulnerability**. Do not open a public
issue for anything that could be used against someone else's machine.

Include what you did, what happened, and the Swarmo version or commit. You
should get a reply within a week. Once a fix is released, the advisory is
published with credit to you, unless you would rather not be named.

Swarmo is pre-1.0. Only the latest release and `main` get security fixes.

## What a workspace can do

A Swarmo workspace is a folder of files that is meant to be committed and
shared. Opening one you did not write is closer to running code than to opening
a document, so it helps to know exactly what those files can make happen.

### Nothing runs when you open a workspace

Opening a workspace, browsing it, and importing a Postman collection or a cURL
command only read files. Scripts and commands run only when you send a request
or start a load test.

### Scripts run in a sandbox, but can reach the network

Pre-request, post-response and virtual-user scripts run in an embedded QuickJS
engine, not in Node. They have no filesystem, no process or shell access, no
environment variables and no module loader. Request scripts are limited to 64 MB
of memory and 5 seconds.

Scripts can, however, **read every variable in the active environment,
secrets included, and send HTTP requests to any host**. That is what makes
token fetches and chained requests possible. It also means a script in an
untrusted workspace could send your secrets somewhere else when you press Send.
Read the scripts in a workspace you did not write before you run it, and do not
select an environment holding real credentials while you do. Postman
collections bring their scripts with them on import.

### Command-sourced auth runs a shell command, and asks first

The *Command token* auth type runs a local command (for example
`gcloud auth print-identity-token`) and uses its output as a bearer token. The
command runs in your shell with your permissions, so Swarmo will not run it
until you have approved that exact command for that workspace. A changed command
counts as a new one and needs approving again. Approvals are stored in the
app's config directory on your machine, never in the workspace, so a workspace
cannot arrive with its own commands already approved. A load test asks for every
command it needs before it starts. The CLI asks on every run, and takes `--yes`
as approval, so only pass `--yes` for workspaces you trust.

### Load tests ask before sending traffic

Before a load test runs, Swarmo lists the hosts it is about to send load to
and waits for you to confirm them. The CLI refuses to start without `--yes` when
it is not attached to a terminal. Sending load to systems you do not own or
have no permission to test is misuse, not a vulnerability in Swarmo.

### Secrets are kept out of git, not encrypted

A variable marked secret is stored in `.swarmo/secrets.env.json` inside the
workspace, not in the environment file you commit. `.swarmo/` is gitignored
when the workspace is created. That file is **plain text**: anyone who can read
your disk can read it. Swarmo does not use the OS keychain.

### History keeps what was sent

Every send is recorded in `.swarmo/history.json`, also gitignored.
`Authorization`, `Proxy-Authorization`, `Cookie` and `X-Api-Key` header values
are redacted before they are written. Request and response **bodies are stored
as they were sent**, cut off after a size limit, so a credential in a body
(a login form, say) ends up on disk. You can clear history from the History
view.

### TLS verification can be turned off

Each request has a *Verify TLS* setting, and Settings has a default for new
requests. With it off, Swarmo accepts any certificate, which is useful against
local servers with self-signed certificates and unsafe anywhere else.
