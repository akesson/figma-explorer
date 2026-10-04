//! Runs the real `figma-explorer` binary against [`FakeFigma`] in a sealed
//! environment: `env_clear()` plus only the variables the CLI needs, a
//! tempdir cwd (no ancestor `.env`), and a tempdir `HOME`/`XDG_CONFIG_HOME`
//! (no `~/.config/figma-explorer/.env`). Nothing from the developer's shell
//! — tokens, team ids, proxies — reaches the subprocess.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;

use crate::fake_figma::{FakeFigma, Req};

pub const TOKEN: &str = "test-token";
pub const FOLDER: &str = "1001";
pub const TEAM: &str = "2002";
/// "Notifications" — `file:2` after a prefetch (synths follow name order).
/// Carries the comment overlay.
pub const NOTIF: &str = "FxFixtureFile000000001";
/// "404 / No Access" — `file:1` after a prefetch. No comments.
pub const NO_ACCESS: &str = "FxFixtureFile000000002";

pub fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/api")
}

pub struct Harness {
    pub figma: FakeFigma,
    tmp: TempDir,
    env: BTreeMap<String, String>,
}

#[derive(Debug)]
pub struct Run {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
    /// Requests this invocation made, sorted (arrival order is
    /// nondeterministic under the CLI's concurrent fetches).
    pub requests: Vec<Req>,
}

impl Run {
    /// Request paths (with query), sorted.
    pub fn paths(&self) -> Vec<&str> {
        self.requests.iter().map(|r| r.path.as_str()).collect()
    }

    #[track_caller]
    pub fn ok(self) -> Self {
        assert!(
            self.success,
            "command failed\n--- stderr ---\n{}\n--- stdout ---\n{}",
            self.stderr, self.stdout
        );
        self
    }
}

impl Harness {
    /// Fake server over the recorded fixtures, with `FIGMA_PROJECTS_IDS` and
    /// `FIGMA_TEAM_ID` set to the fixture folder/team (as a user's `.env`
    /// would). Remove them with [`Harness::unset`].
    pub fn new() -> Self {
        Self::with_fixtures(&fixtures_dir())
    }

    pub fn with_fixtures(fixtures: &Path) -> Self {
        let figma = FakeFigma::start(fixtures);
        let tmp = TempDir::new().unwrap();
        for d in ["cache", "home", "cwd"] {
            std::fs::create_dir(tmp.path().join(d)).unwrap();
        }
        let mut env = BTreeMap::new();
        let p = |d: &str| tmp.path().join(d).to_string_lossy().into_owned();
        env.insert("FIGMA_TOKEN".into(), TOKEN.into());
        env.insert("FIGMA_EXPLORER_API_BASE".into(), figma.base_url().into());
        env.insert("FIGMA_EXPLORER_CACHE_DIR".into(), p("cache"));
        env.insert("HOME".into(), p("home"));
        env.insert("USERPROFILE".into(), p("home"));
        env.insert("XDG_CONFIG_HOME".into(), p("home"));
        env.insert("FIGMA_PROJECTS_IDS".into(), FOLDER.into());
        env.insert("FIGMA_TEAM_ID".into(), TEAM.into());
        // Windows networking needs it; harmless elsewhere.
        if let Ok(v) = std::env::var("SYSTEMROOT") {
            env.insert("SYSTEMROOT".into(), v);
        }
        Harness { figma, tmp, env }
    }

    pub fn set(&mut self, key: &str, value: &str) -> &mut Self {
        self.env.insert(key.into(), value.into());
        self
    }

    pub fn unset(&mut self, key: &str) -> &mut Self {
        self.env.remove(key);
        self
    }

    pub fn cache_dir(&self) -> PathBuf {
        self.tmp.path().join("cache")
    }

    /// The subprocess's working directory (where `assets` writes by default).
    pub fn cwd(&self) -> PathBuf {
        self.tmp.path().join("cwd")
    }

    /// Run the binary with `args`.
    pub fn run(&self, args: &[&str]) -> Run {
        self.figma.drain();
        let out = Command::new(env!("CARGO_BIN_EXE_figma-explorer"))
            .args(args)
            .env_clear()
            .envs(&self.env)
            .current_dir(self.cwd())
            .output()
            .unwrap();
        let mut requests = self.figma.drain();
        requests.sort_by(|a, b| a.path.cmp(&b.path));
        Run {
            success: out.status.success(),
            stdout: String::from_utf8(out.stdout).unwrap(),
            stderr: String::from_utf8(out.stderr).unwrap(),
            requests,
        }
    }

    /// `cache prefetch` over the fixture folder; the usual starting state.
    pub fn prefetched(&self) -> Run {
        let run = self.run(&["cache", "prefetch"]).ok();
        assert_authenticated(&run);
        run
    }

    /// A file's cache entry path, e.g. `entry(NOTIF, "comments.json")`.
    pub fn entry(&self, file_key: &str, ext: &str) -> PathBuf {
        self.cache_dir()
            .join("files")
            .join(format!("{file_key}.{ext}"))
    }

    pub fn meta(&self, file_key: &str) -> serde_json::Value {
        serde_json::from_str(&std::fs::read_to_string(self.entry(file_key, "meta.json")).unwrap())
            .unwrap()
    }

    /// Rewrite fields of a file's `.meta.json` (e.g. push
    /// `version_checked_at_epoch` into the past to make it due for a probe).
    pub fn patch_meta(&self, file_key: &str, fields: &[(&str, serde_json::Value)]) {
        let mut meta = self.meta(file_key);
        for (k, v) in fields {
            meta[*k] = v.clone();
        }
        std::fs::write(
            self.entry(file_key, "meta.json"),
            serde_json::to_string_pretty(&meta).unwrap(),
        )
        .unwrap();
    }

    /// Make every cached file due for a version probe, as if the last check
    /// was more than `VERSION_CHECK_SECS` ago.
    pub fn age_version_checks(&self) {
        for key in [NOTIF, NO_ACCESS] {
            self.patch_meta(key, &[("version_checked_at_epoch", 0.into())]);
        }
    }

    /// Replace run-specific values (tempdir, server port) with stable tokens
    /// so output can be snapshotted.
    pub fn redact(&self, s: &str) -> String {
        s.replace(self.figma.base_url(), "[FIGMA]")
            .replace(&*self.tmp.path().to_string_lossy(), "[TMP]")
    }
}

/// Every API request carries the token; image downloads (`/img/*`, which
/// stand in for S3 URLs) carry none.
#[track_caller]
pub fn assert_authenticated(run: &Run) {
    for r in &run.requests {
        if r.path.starts_with("/img/") {
            assert_eq!(r.token, None, "{} must not send the token", r.path);
        } else {
            assert_eq!(
                r.token.as_deref(),
                Some(TOKEN),
                "{} lacks the token",
                r.path
            );
        }
    }
}
