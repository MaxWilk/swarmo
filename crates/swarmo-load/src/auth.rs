//! Auth tokens produced by running a local command.
//!
//! The credentials people test against expire — a `gcloud auth
//! print-identity-token` is good for an hour — and pasting a fresh one in
//! every hour is the kind of friction that stops a tool being used. This runs
//! the command instead, caches the result in memory, and refreshes it when the
//! server says it is stale.
//!
//! Two things matter more than the happy path:
//!
//! * **Single flight.** At a thousand requests a second, the moment a token
//!   expires every request in flight fails at once. Without coordination each
//!   would spawn its own `gcloud`, which is a fork bomb rather than a refresh.
//!   One caller runs the command; everyone else waits for it and takes the
//!   result.
//! * **Approval.** Workspaces are meant to be committed and shared, so a
//!   request file that says "run this command" is remote code execution unless
//!   the command is allowed first. That gate lives in the caller — the desktop
//!   app's `AppState`, or the CLI's confirmation prompt. This module never
//!   checks it, and must never be reached without it.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use tokio::sync::Mutex as AsyncMutex;

/// How long a command may run before it is given up on.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(15);

/// Refresh this long before a token's own expiry, so a request in flight does
/// not arrive just after it lapses.
const EXPIRY_MARGIN: Duration = Duration::from_secs(60);

/// Longest stderr kept on a failure, so one chatty command cannot fill the UI.
const MAX_STDERR: usize = 500;

/// A token, and what is known about how long it is good for.
#[derive(Debug, Clone)]
pub struct Token {
    pub value: String,
    /// Bumped on every refresh. A caller whose request failed passes the epoch
    /// it used, which is how a refresh already done by someone else is
    /// recognised without running the command again.
    pub epoch: u64,
}

#[derive(Debug, Clone)]
struct Cached {
    value: String,
    expires_at: Option<SystemTime>,
    epoch: u64,
    /// When this token was fetched, so a rejection straight after a refresh
    /// is not answered with yet another refresh.
    fetched_at: Instant,
    /// Whether this token was itself the product of a refresh. The first
    /// refresh after a plain fetch must always run — that is the expiry case
    /// the cache exists for — so only a refresh *of a refresh* is rate-limited.
    refreshed: bool,
}

/// The least time between two refreshes of one command.
///
/// A token the server rejects on sight — wrong audience, missing scope —
/// would otherwise have every request trigger a fresh run of the command,
/// serialising every worker behind it and inflating each latency by the
/// command's own runtime. Within this window the rejection is simply
/// recorded as the failure it is.
const REFRESH_COOLDOWN: Duration = Duration::from_secs(5);

impl Cached {
    fn is_stale(&self) -> bool {
        match self.expires_at {
            Some(at) => SystemTime::now() + EXPIRY_MARGIN >= at,
            None => false,
        }
    }
}

/// Tokens produced by commands, keyed by the exact command text.
#[derive(Default)]
pub struct TokenCache {
    entries: Mutex<HashMap<String, Cached>>,
    /// One lock per command, so a refresh of one does not block another.
    locks: Mutex<HashMap<String, Arc<AsyncMutex<()>>>>,
}

impl TokenCache {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock_for(&self, command: &str) -> Arc<AsyncMutex<()>> {
        let mut locks = self.locks.lock().unwrap_or_else(|e| e.into_inner());
        locks.entry(command.to_string()).or_default().clone()
    }

    fn cached(&self, command: &str) -> Option<Cached> {
        self.entries
            .lock()
            .ok()
            .and_then(|e| e.get(command).cloned())
    }

    /// The current token, running the command if there is not a usable one.
    pub async fn get(&self, command: &str) -> Result<Token, String> {
        if let Some(c) = self.cached(command) {
            if !c.is_stale() {
                return Ok(Token {
                    value: c.value,
                    epoch: c.epoch,
                });
            }
        }
        self.run_and_store(command, None).await
    }

    /// Fetch a new token after `stale_epoch` was rejected.
    ///
    /// If the cache has already moved past that epoch someone else refreshed
    /// while this caller was failing, so their token is returned rather than
    /// running the command a second time.
    pub async fn refresh(&self, command: &str, stale_epoch: u64) -> Result<Token, String> {
        self.run_and_store(command, Some(stale_epoch)).await
    }

    async fn run_and_store(
        &self,
        command: &str,
        stale_epoch: Option<u64>,
    ) -> Result<Token, String> {
        let lock = self.lock_for(command);
        let _guard = lock.lock().await;

        // Re-check under the lock: whoever held it before us may have done the
        // work already. This is what turns a thundering herd into one run.
        if let Some(c) = self.cached(command) {
            let superseded = match stale_epoch {
                Some(stale) => c.epoch > stale,
                None => !c.is_stale(),
            };
            if superseded {
                return Ok(Token {
                    value: c.value,
                    epoch: c.epoch,
                });
            }
            // The newest token came from a refresh moments ago and is already
            // being rejected: refreshing again would not change the answer.
            if stale_epoch.is_some() && c.refreshed && c.fetched_at.elapsed() < REFRESH_COOLDOWN {
                return Err(format!(
                    "token refreshed {:.1}s ago is still rejected; not refreshing again yet",
                    c.fetched_at.elapsed().as_secs_f64()
                ));
            }
        }

        let value = run_command(command).await?;
        let expires_at = jwt_expiry(&value);

        let mut entries = self.entries.lock().map_err(|_| "token cache is poisoned")?;
        let epoch = entries.get(command).map(|c| c.epoch + 1).unwrap_or(1);
        entries.insert(
            command.to_string(),
            Cached {
                value: value.clone(),
                expires_at,
                epoch,
                fetched_at: Instant::now(),
                refreshed: stale_epoch.is_some(),
            },
        );
        Ok(Token { value, epoch })
    }

    /// Forget every cached token. Used when a workspace closes.
    pub fn clear(&self) {
        if let Ok(mut e) = self.entries.lock() {
            e.clear();
        }
    }
}

/// Run a command through the platform shell and return its trimmed stdout.
///
/// The shell matters: on Windows `gcloud` is a `.cmd`, which cannot be executed
/// directly, and everywhere the user expects their own PATH and aliases to
/// apply the way they would in a terminal.
pub async fn run_command(command: &str) -> Result<String, String> {
    // cmd.exe does not follow the `\"` escaping `arg` would apply, so any
    // quote in the command would reach it mangled. Passed raw instead, with
    // `/S` so cmd strips exactly the outer pair of quotes added here.
    #[cfg(windows)]
    let mut cmd = {
        let mut c = tokio::process::Command::new("cmd");
        c.raw_arg(format!("/S /C \"{command}\""));
        c
    };
    #[cfg(not(windows))]
    let mut cmd = {
        let mut c = tokio::process::Command::new("sh");
        c.arg("-c").arg(command);
        c
    };
    cmd.stdin(std::process::Stdio::null());
    // A command that times out is abandoned by dropping its future; without
    // this the process would be left running behind the run.
    cmd.kill_on_drop(true);

    let output = match tokio::time::timeout(COMMAND_TIMEOUT, cmd.output()).await {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => return Err(format!("The command could not be started: {e}")),
        Err(_) => {
            return Err(format!(
                "The command did not finish within {} seconds.",
                COMMAND_TIMEOUT.as_secs()
            ))
        }
    };

    if !output.status.success() {
        let mut stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        stderr.truncate(MAX_STDERR);
        let code = output
            .status
            .code()
            .map(|c| c.to_string())
            .unwrap_or_else(|| "unknown".into());
        return Err(if stderr.is_empty() {
            format!("The command failed (exit code {code}).")
        } else {
            format!("The command failed (exit code {code}): {stderr}")
        });
    }

    let token = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if token.is_empty() {
        return Err("The command printed nothing, so there is no token to send.".to_string());
    }
    Ok(token)
}

/// Read the `exp` claim from a JWT, if the token is one.
///
/// No signature is checked and none should be: this is used to decide when to
/// fetch a fresh token, not to decide whether to trust anything.
pub fn jwt_expiry(token: &str) -> Option<SystemTime> {
    let mut parts = token.split('.');
    let (_, payload, signature) = (parts.next()?, parts.next()?, parts.next()?);
    if signature.is_empty() || parts.next().is_some() {
        return None;
    }
    let json = base64url_decode(payload)?;
    let value: serde_json::Value = serde_json::from_slice(&json).ok()?;
    let exp = value.get("exp")?.as_u64()?;
    Some(SystemTime::UNIX_EPOCH + Duration::from_secs(exp))
}

/// Decode unpadded base64url, which is what JWT segments use.
fn base64url_decode(input: &str) -> Option<Vec<u8>> {
    let mut bits = 0u32;
    let mut nbits = 0;
    let mut out = Vec::with_capacity(input.len() * 3 / 4);
    for c in input.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'-' => 62,
            b'_' => 63,
            b'=' => break,
            _ => return None,
        } as u32;
        bits = (bits << 6) | v;
        nbits += 6;
        if nbits >= 8 {
            nbits -= 8;
            out.push((bits >> nbits) as u8);
        }
    }
    Some(out)
}

/// Show a token without revealing it: enough to tell two apart, not enough to
/// use.
pub fn mask(token: &str) -> String {
    let chars: Vec<char> = token.chars().collect();
    if chars.len() <= 12 {
        return "•".repeat(chars.len().max(4));
    }
    let head: String = chars[..4].iter().collect();
    let tail: String = chars[chars.len() - 4..].iter().collect();
    format!("{head}…{tail}")
}

/// The live token for one auth command, shared by every virtual user.
///
/// A load run resolves the token once before it starts; this is what lets a
/// run that outlives its credentials keep going, without each virtual user
/// discovering the expiry separately.
pub struct AuthTokenState {
    pub command: String,
    /// Header (HTTP) or metadata key (gRPC) the token is sent in.
    pub header_name: String,
    /// Text placed before the token, e.g. `"Bearer "`.
    pub prefix: String,
    live: Arc<LiveToken>,
}

/// The token itself, which every step using one command shares — however
/// each of those steps chooses to send it.
struct LiveToken {
    cache: Arc<TokenCache>,
    current: std::sync::RwLock<Token>,
    /// How many times this token has actually been refreshed mid-run.
    refreshes: std::sync::atomic::AtomicU64,
}

impl AuthTokenState {
    pub fn new(
        command: String,
        header_name: String,
        prefix: String,
        cache: Arc<TokenCache>,
        initial: Token,
    ) -> Self {
        Self {
            command,
            header_name,
            prefix,
            live: Arc::new(LiveToken {
                cache,
                current: std::sync::RwLock::new(initial),
                refreshes: std::sync::atomic::AtomicU64::new(0),
            }),
        }
    }

    /// The same live token, sent in another header or with another prefix.
    ///
    /// Two requests can run one command yet send its output differently; a
    /// refresh made through either must still reach both.
    pub fn with_header(&self, header_name: String, prefix: String) -> Self {
        Self {
            command: self.command.clone(),
            header_name,
            prefix,
            live: self.live.clone(),
        }
    }

    /// Whether `other` sends the very same token, however it presents it.
    pub fn shares_token_with(&self, other: &AuthTokenState) -> bool {
        Arc::ptr_eq(&self.live, &other.live)
    }

    /// The header value to send, and the epoch it came from.
    ///
    /// The epoch travels with the request so that a rejection can say *which*
    /// token was refused — by the time it comes back, another virtual user may
    /// already have replaced it.
    pub fn header_value(&self) -> (String, u64) {
        let t = self
            .live
            .current
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        (format!("{}{}", self.prefix, t.value), t.epoch)
    }

    /// Replace the token that `stale_epoch` refers to.
    ///
    /// Coordinated by the cache, so a thousand virtual users failing at once
    /// still run the command once between them.
    pub async fn refresh(&self, stale_epoch: u64) -> Result<(), String> {
        let fresh = self.live.cache.refresh(&self.command, stale_epoch).await?;
        let mut guard = self.live.current.write().unwrap_or_else(|e| e.into_inner());
        // Another user may have installed a newer token while this one was
        // waiting; never move the shared token backwards.
        if fresh.epoch > guard.epoch {
            *guard = fresh;
            self.live
                .refreshes
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        Ok(())
    }

    pub fn refresh_count(&self) -> u64 {
        self.live
            .refreshes
            .load(std::sync::atomic::Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A command whose output differs every time it actually runs.
    ///
    /// Uses the clock at its finest available resolution rather than a random
    /// number: Windows seeds `%RANDOM%` from the time in whole seconds, so
    /// repeated runs inside one second return the *same* number — which would
    /// make a single-flight test pass without single flight.
    fn changing_command() -> String {
        if cfg!(windows) {
            "echo %TIME%".to_string()
        } else {
            "date +%s%N".to_string()
        }
    }

    /// A shell command that prints a token, on either platform.
    fn echo(text: &str) -> String {
        if cfg!(windows) {
            format!("echo {text}")
        } else {
            format!("echo '{text}'")
        }
    }

    #[tokio::test]
    async fn stdout_becomes_the_token_without_its_newline() {
        let out = run_command(&echo("abc123")).await.unwrap();
        assert_eq!(out, "abc123");
    }

    #[tokio::test]
    async fn quotes_in_the_command_reach_the_shell_intact() {
        // `arg` escaped quotes as `\"`, which cmd.exe passes through
        // literally, so on Windows this printed `\"a b\"`.
        let out = run_command(r#"echo "a b""#).await.unwrap();
        let expected = if cfg!(windows) { r#""a b""# } else { "a b" };
        assert_eq!(out, expected);
    }

    #[tokio::test]
    async fn a_command_that_prints_nothing_is_an_error() {
        // A blank token would be sent as "Bearer " and fail confusingly at the
        // server; failing here says what actually went wrong.
        let cmd = if cfg!(windows) { "cd ." } else { "true" };
        let err = run_command(cmd).await.unwrap_err();
        assert!(err.contains("printed nothing"), "{err}");
    }

    #[tokio::test]
    async fn a_failing_command_reports_its_stderr() {
        let cmd = if cfg!(windows) {
            "echo boom 1>&2 && exit /b 3"
        } else {
            "echo boom >&2; exit 3"
        };
        let err = run_command(cmd).await.unwrap_err();
        assert!(err.contains("boom"), "{err}");
        assert!(err.contains('3'), "{err}");
    }

    #[tokio::test]
    async fn a_command_that_never_finishes_is_given_up_on() {
        // Shorter than COMMAND_TIMEOUT would make this test slow; instead check
        // the timeout path directly with a tiny budget.
        let sleep = if cfg!(windows) {
            "ping -n 30 127.0.0.1 >nul"
        } else {
            "sleep 30"
        };
        let started = std::time::Instant::now();
        let result = tokio::time::timeout(Duration::from_millis(300), run_command(sleep)).await;
        assert!(result.is_err(), "the command returned early");
        assert!(started.elapsed() < COMMAND_TIMEOUT);
    }

    #[tokio::test]
    async fn a_cached_token_is_reused() {
        let cache = TokenCache::new();
        let a = cache.get(&echo("tok")).await.unwrap();
        let b = cache.get(&echo("tok")).await.unwrap();
        assert_eq!(a.value, b.value);
        assert_eq!(a.epoch, b.epoch, "a cache hit must not bump the epoch");
    }

    #[tokio::test]
    async fn a_refresh_supersedes_the_token_that_failed() {
        let cache = TokenCache::new();
        let first = cache.get(&echo("tok")).await.unwrap();
        let second = cache.refresh(&echo("tok"), first.epoch).await.unwrap();
        assert!(second.epoch > first.epoch);
    }

    #[tokio::test]
    async fn a_refresh_for_an_epoch_already_replaced_does_not_run_again() {
        let cache = TokenCache::new();
        let first = cache.get(&echo("tok")).await.unwrap();
        let second = cache.refresh(&echo("tok"), first.epoch).await.unwrap();

        // A second caller that failed with the *original* token arrives late;
        // the work is already done, so it must take the new token as is.
        let third = cache.refresh(&echo("tok"), first.epoch).await.unwrap();
        assert_eq!(third.epoch, second.epoch);
    }

    #[tokio::test]
    async fn a_token_rejected_straight_after_a_refresh_is_not_refreshed_again() {
        // A token the server rejects on sight would otherwise have every
        // request run the command afresh, back to back, for the whole run.
        let cache = TokenCache::new();
        let first = cache.get(&echo("tok")).await.unwrap();
        let second = cache.refresh(&echo("tok"), first.epoch).await.unwrap();
        assert!(second.epoch > first.epoch, "the first refresh always runs");

        // Rejected again immediately: within the cooldown, no third run.
        let third = cache.refresh(&echo("tok"), second.epoch).await;
        assert!(
            third.is_err(),
            "a refresh of a fresh refresh must be refused"
        );
        assert_eq!(cache.get(&echo("tok")).await.unwrap().epoch, second.epoch);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn fifty_simultaneous_refreshes_run_the_command_once() {
        // The case that matters: when a token expires under load, every
        // request in flight fails at the same moment. Without coordination
        // each would spawn its own process.
        //
        // The command prints a different value every time it runs, so fifty
        // callers ending up with the same token is only possible if it ran
        // once.
        let command = changing_command();
        let cache = Arc::new(TokenCache::new());
        let first = cache.get(&command).await.unwrap();

        let mut tasks = Vec::new();
        for _ in 0..50 {
            let cache = cache.clone();
            let command = command.clone();
            let epoch = first.epoch;
            tasks.push(tokio::spawn(
                async move { cache.refresh(&command, epoch).await },
            ));
        }
        let mut tokens = Vec::new();
        for t in tasks {
            tokens.push(t.await.unwrap().unwrap());
        }

        let distinct: std::collections::HashSet<&str> =
            tokens.iter().map(|t| t.value.as_str()).collect();
        assert_eq!(
            distinct.len(),
            1,
            "the command ran {} times instead of once",
            distinct.len()
        );
        let epochs: std::collections::HashSet<u64> = tokens.iter().map(|t| t.epoch).collect();
        assert_eq!(epochs.len(), 1, "callers disagreed about the epoch");

        // And it did refresh rather than handing back the token that failed.
        assert!(tokens[0].epoch > first.epoch);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn simultaneous_first_fetches_also_run_the_command_once() {
        // The same herd forms at the start of a load run, before any token
        // exists at all.
        let command = changing_command();
        let cache = Arc::new(TokenCache::new());

        let mut tasks = Vec::new();
        for _ in 0..25 {
            let cache = cache.clone();
            let command = command.clone();
            tasks.push(tokio::spawn(async move { cache.get(&command).await }));
        }
        let mut values = Vec::new();
        for t in tasks {
            values.push(t.await.unwrap().unwrap().value);
        }
        let distinct: std::collections::HashSet<&str> = values.iter().map(|s| s.as_str()).collect();
        assert_eq!(distinct.len(), 1, "the command ran more than once");
    }

    #[test]
    fn a_jwt_expiry_is_read_without_verifying_anything() {
        // {"exp":1700000000} base64url, with a signature segment present.
        let token = "eyJhbGciOiJSUzI1NiJ9.eyJleHAiOjE3MDAwMDAwMDB9.notarealsignature";
        let exp = jwt_expiry(token).expect("should parse");
        assert_eq!(
            exp.duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_secs(),
            1_700_000_000
        );
    }

    #[test]
    fn an_opaque_token_simply_has_no_known_expiry() {
        assert!(jwt_expiry("ya29.a0AfH6SM-opaque-google-token").is_none());
        assert!(jwt_expiry("a.b").is_none());
        assert!(jwt_expiry("a.b.c.d").is_none());
        // Malformed base64 is not an error, just an unknown expiry.
        assert!(jwt_expiry("aaa.!!!!.bbb").is_none());
        // A JWT with no exp claim.
        assert!(jwt_expiry("eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJhIn0.sig").is_none());
    }

    #[test]
    fn a_token_is_masked_rather_than_shown() {
        assert_eq!(mask("eyJhbGciOiJSUzI1NiJ9xxxx"), "eyJh…xxxx");
        // A short token reveals nothing at all rather than most of itself.
        assert_eq!(mask("short"), "•••••");
        assert!(!mask("supersecrettoken").contains("secret"));
    }
}
