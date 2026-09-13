//! The token, and where it comes from.
//!
//! Resolution order is fixed by `spec/00-overview.md` §5:
//! `OMAGHY_TOKEN` → `GH_TOKEN` → `gh auth token` → [`AuthError::Missing`],
//! whose message already names `gh auth login`.
//!
//! We never persist a token of our own and never write to `gh`'s config.
//! `gh auth token` reads gh's keyring and costs ~43ms — cheap enough to do at
//! startup, which is why there is no OAuth flow here.

use omaghy_model::AuthError;
use std::fmt;
use std::process::Command;

/// A GitHub token.
///
/// The `Debug` impl is written by hand and redacts the secret. This is the
/// whole reason the newtype exists: a token that reaches a log is a leaked
/// token, and `#[derive(Debug)]` on any struct that transitively holds one
/// would print it. There is deliberately no `Display`.
#[derive(Clone)]
pub struct Token(String);

impl Token {
    /// Trims surrounding whitespace — `gh auth token` emits a trailing
    /// newline, and a token pasted into a shell profile often carries one too.
    ///
    /// An empty or whitespace-only value is not a token; it is an unset
    /// variable that happens to exist, which is a common shell-profile
    /// accident and must not shadow the rest of the chain.
    pub fn new(raw: impl AsRef<str>) -> Option<Self> {
        let t = raw.as_ref().trim();
        (!t.is_empty()).then(|| Self(t.to_owned()))
    }

    /// The secret itself. Named so that every call site reads as a decision.
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// The `Authorization` header value.
    pub fn header_value(&self) -> String {
        format!("Bearer {}", self.0)
    }
}

impl fmt::Debug for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Token(<redacted>)")
    }
}

/// Which link of the chain produced the token.
///
/// Worth keeping: `omaghy doctor` should be able to say *where* the token came
/// from, and "it works but not from where you think" is a real support case.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenSource {
    OmaghyToken,
    GhToken,
    GhCli,
}

impl TokenSource {
    pub fn describe(self) -> &'static str {
        match self {
            Self::OmaghyToken => "$OMAGHY_TOKEN",
            Self::GhToken => "$GH_TOKEN",
            Self::GhCli => "`gh auth token`",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ResolvedToken {
    pub token: Token,
    pub source: TokenSource,
}

/// The two pieces of the outside world the chain touches.
///
/// Injectable so the resolution order can be tested without environment
/// variables (which are process-global and race under a threaded test runner)
/// and without spawning `gh`.
pub trait TokenEnvironment: fmt::Debug + Send + Sync {
    fn var(&self, key: &str) -> Option<String>;

    /// Run `gh auth token`, returning its stdout.
    fn gh_auth_token(&self) -> Result<String, AuthError>;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SystemEnvironment;

impl TokenEnvironment for SystemEnvironment {
    fn var(&self, key: &str) -> Option<String> {
        std::env::var(key).ok()
    }

    fn gh_auth_token(&self) -> Result<String, AuthError> {
        let out = Command::new("gh")
            .args(["auth", "token"])
            .output()
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    AuthError::HelperFailed("`gh` is not installed".to_owned())
                } else {
                    AuthError::HelperFailed(e.to_string())
                }
            })?;

        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).into_owned())
        } else {
            // gh's own message is the useful one ("not logged in to any hosts"),
            // so pass it through rather than inventing a replacement.
            let stderr = String::from_utf8_lossy(&out.stderr).trim().to_owned();
            Err(AuthError::HelperFailed(if stderr.is_empty() {
                format!("exited with {}", out.status)
            } else {
                stderr
            }))
        }
    }
}

/// Resolve a token from the ambient environment.
///
/// Synchronous and blocking: it shells out. Call it once at startup, not from
/// a request path.
pub fn resolve_token() -> Result<ResolvedToken, AuthError> {
    resolve_token_in(&SystemEnvironment)
}

/// The chain, with the outside world injected.
///
/// A failing `gh` is not fatal on its own — it is only fatal if nothing else
/// produced a token — but its message is kept and returned instead of the
/// blander [`AuthError::Missing`], because "gh is not installed" is far more
/// actionable than "no token found".
pub fn resolve_token_in(env: &dyn TokenEnvironment) -> Result<ResolvedToken, AuthError> {
    for (key, source) in [
        ("OMAGHY_TOKEN", TokenSource::OmaghyToken),
        ("GH_TOKEN", TokenSource::GhToken),
    ] {
        if let Some(token) = env.var(key).and_then(Token::new) {
            return Ok(ResolvedToken { token, source });
        }
    }

    match env.gh_auth_token() {
        Ok(raw) => Token::new(raw)
            .map(|token| ResolvedToken {
                token,
                source: TokenSource::GhCli,
            })
            .ok_or(AuthError::Missing),
        Err(helper_failed) => Err(helper_failed),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[derive(Debug, Default)]
    struct FakeEnv {
        vars: HashMap<String, String>,
        gh: Option<Result<String, AuthError>>,
    }

    impl FakeEnv {
        fn with(mut self, k: &str, v: &str) -> Self {
            self.vars.insert(k.to_owned(), v.to_owned());
            self
        }
        fn gh(mut self, r: Result<String, AuthError>) -> Self {
            self.gh = Some(r);
            self
        }
    }

    impl TokenEnvironment for FakeEnv {
        fn var(&self, key: &str) -> Option<String> {
            self.vars.get(key).cloned()
        }
        fn gh_auth_token(&self) -> Result<String, AuthError> {
            self.gh
                .clone()
                .unwrap_or(Err(AuthError::HelperFailed("not stubbed".to_owned())))
        }
    }

    #[test]
    fn a_token_never_prints_itself() {
        let t = Token::new("ghp_supersecretvalue").unwrap();
        let printed = format!("{t:?}");
        assert!(
            !printed.contains("supersecret"),
            "leaked in Debug: {printed}"
        );

        // And it stays redacted when nested inside a derived Debug.
        let nested = ResolvedToken {
            token: t,
            source: TokenSource::GhCli,
        };
        let printed = format!("{nested:?}");
        assert!(
            !printed.contains("supersecret"),
            "leaked in Debug: {printed}"
        );
    }

    #[test]
    fn omaghy_token_wins_over_everything() {
        let env = FakeEnv::default()
            .with("OMAGHY_TOKEN", "from-omaghy")
            .with("GH_TOKEN", "from-gh-env")
            .gh(Ok("from-gh-cli".to_owned()));
        let r = resolve_token_in(&env).unwrap();
        assert_eq!(r.source, TokenSource::OmaghyToken);
        assert_eq!(r.token.expose(), "from-omaghy");
    }

    #[test]
    fn gh_token_is_second() {
        let env = FakeEnv::default()
            .with("GH_TOKEN", "from-gh-env")
            .gh(Ok("from-gh-cli".to_owned()));
        let r = resolve_token_in(&env).unwrap();
        assert_eq!(r.source, TokenSource::GhToken);
    }

    #[test]
    fn the_cli_is_the_last_resort() {
        let env = FakeEnv::default().gh(Ok("gho_fromkeyring\n".to_owned()));
        let r = resolve_token_in(&env).unwrap();
        assert_eq!(r.source, TokenSource::GhCli);
        // The trailing newline `gh` emits must not reach a header.
        assert_eq!(r.token.expose(), "gho_fromkeyring");
    }

    #[test]
    fn an_empty_variable_does_not_shadow_the_rest_of_the_chain() {
        // `export GH_TOKEN=` in a shell profile is common, and treating it as
        // a token turns the whole chain into a 401.
        let env = FakeEnv::default()
            .with("OMAGHY_TOKEN", "")
            .with("GH_TOKEN", "   ")
            .gh(Ok("gho_fromkeyring".to_owned()));
        let r = resolve_token_in(&env).unwrap();
        assert_eq!(r.source, TokenSource::GhCli);
    }

    #[test]
    fn nothing_anywhere_names_the_command_that_fixes_it() {
        let env = FakeEnv::default().gh(Ok(String::new()));
        let e = resolve_token_in(&env).unwrap_err();
        assert_eq!(e, AuthError::Missing);
        assert!(e.to_string().contains("gh auth login"));
    }

    #[test]
    fn a_broken_helper_reports_itself_rather_than_being_swallowed() {
        let env = FakeEnv::default().gh(Err(AuthError::HelperFailed(
            "`gh` is not installed".to_owned(),
        )));
        let e = resolve_token_in(&env).unwrap_err();
        assert!(matches!(e, AuthError::HelperFailed(ref m) if m.contains("not installed")));
    }
}
