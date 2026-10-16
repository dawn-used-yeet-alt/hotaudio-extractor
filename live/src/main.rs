//! `hotaudio-live-tests` — the scheduled acceptance suite for the hotaudio.net
//! extractor.
//!
//! One live session, every function of the crate and the `hotaudio-download`
//! binary exercised against the real site, and a machine-readable report. CI
//! installs Cloudflare WARP for egress, runs this, and turns a failing report
//! into one GitHub issue per broken component.
//!
//! The library and the binary are both built from this repository: `hotaudio-rs`
//! is a path dependency (`..`) and the workflow builds the CLI from the same
//! checkout, so a run always exercises the current source, never a release.
//!
//! The suite is deliberate about traffic: the page is fetched once and fed back
//! to the CLI as a file wherever the CLI allows it, and the online run's
//! artifacts (audio, keys, container) are reused by the offline, cached and
//! streaming checks instead of being refetched.

mod checks;
mod issues;
mod report;

use std::path::PathBuf;
use std::process::ExitCode;

use checks::{Ctx, Runner};
use report::Report;

/// The track every run uses. Overridable with `--url` / `HOTAUDIO_LIVE_URL`.
const DEFAULT_URL: &str =
    "https://hotaudio.net/u/Lurkydip/Welcome-to-FuckBunny-FreeUse-Cruise-Lines";
const DEFAULT_REPO: &str = "dawn-used-yeet-alt/hotaudio-extractor";
const DEFAULT_LABEL: &str = "live-failure";

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::from(2)
        }
    }
}

fn run() -> Result<ExitCode, String> {
    let cfg = Config::parse()?;
    if cfg.list {
        println!("components:");
        for component in report::comp::ALL {
            println!("  {component}");
        }
        return Ok(ExitCode::SUCCESS);
    }

    // `--file-existing` only reports on a run that already happened: the live
    // suite is never re-run just to file issues.
    if let Some(path) = &cfg.file_existing {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        let report: Report =
            serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        let n = issues::file(
            &report.checks,
            &report.url,
            &cfg.repo,
            &cfg.label,
            report.crate_rev.as_deref(),
        )?;
        println!(
            "filed/updated {n} issue(s) on {} ({} failing)",
            cfg.repo, report.failed
        );
        // A reporting tool: success means the issues were filed, not that the
        // suite passed. The caller holds the report and already knows its verdict.
        return Ok(ExitCode::SUCCESS);
    }

    std::fs::create_dir_all(&cfg.workdir)
        .map_err(|e| format!("cannot create {}: {e}", cfg.workdir.display()))?;

    let ctx = Ctx {
        url: cfg.url.clone(),
        api_base: hotaudio::HOTAUDIO_API_BASE.to_string(),
        cli: cfg.cli.clone(),
        workdir: cfg.workdir.clone(),
    };

    let started = hotaudio::clock::unix_secs();
    let mut runner = Runner::new();

    // The page is fetched exactly once; later steps that accept a saved page use
    // this file instead of the URL.
    let page_path = cfg.workdir.join("page.html");
    let live = hotaudio::block_on(checks::run_live(&mut runner, &ctx));
    let page_saved = live.as_ref().and_then(|live| {
        std::fs::write(&page_path, &live.html)
            .ok()
            .map(|()| page_path.clone())
    });

    if cfg.heavy {
        if let Some(live) = &live {
            if let Some(artifacts) = checks::run_cli_online(&mut runner, &ctx, live) {
                checks::run_offline(&mut runner, &ctx, &artifacts);
                checks::run_cached(&mut runner, &ctx, &artifacts);
                if cfg.stream {
                    let page = page_saved.as_deref().unwrap_or(&page_path);
                    checks::run_streaming(&mut runner, &ctx, &artifacts, page);
                }
            }
        } else {
            runner.ok(
                report::comp::CLI,
                "online run",
                "skipped: the page was not reachable",
                Vec::new(),
            );
        }
        if cfg.keysonly {
            checks::run_keys_only(&mut runner, &ctx);
        }
    }

    checks::run_cli_surface(&mut runner, &ctx, page_saved.as_deref());

    let report = Report {
        url: cfg.url.clone(),
        cli: ctx.cli.to_string_lossy().into_owned(),
        crate_rev: env("HOTAUDIO_REV"),
        started_unix: started as u64,
        duration_secs: hotaudio::clock::unix_secs() - started,
        requests: runner.requests.clone(),
        passed: runner.passed(),
        failed: runner.failed(),
        checks: runner.checks.clone(),
    };

    print_summary(&report);

    if let Some(path) = &cfg.report {
        let json = serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?;
        std::fs::write(path, json).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
        println!("\nreport: {}", path.display());
    }

    let failed = report.failed > 0;
    if failed && cfg.file_issues {
        match issues::file(
            &report.checks,
            &report.url,
            &cfg.repo,
            &cfg.label,
            report.crate_rev.as_deref(),
        ) {
            Ok(n) => println!("filed/updated {n} issue(s) on {}", cfg.repo),
            Err(e) => eprintln!("warning: could not file issues: {e}"),
        }
    }

    Ok(if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

fn print_summary(report: &Report) {
    println!("hotaudio live acceptance · {}", report.url);
    if let Some(rev) = &report.crate_rev {
        println!("  crate: {rev}");
    }
    println!(
        "  {} passed · {} failed · {:.1}s",
        report.passed, report.failed, report.duration_secs
    );
    println!(
        "  requests: {} page · {} listen · {} container · {} range",
        report.requests.page,
        report.requests.listen,
        report.requests.container,
        report.requests.range
    );
    let mut last: &str = "";
    for check in &report.checks {
        if check.component != last {
            println!();
            last = &check.component;
        }
        let mark = if check.ok { "ok  " } else { "FAIL" };
        println!(
            "  {mark} [{}] {} — {}",
            check.component, check.name, check.detail
        );
    }
    println!();
}

/// The workdir: `$RUNNER_TEMP/hotaudio-live` in CI, else a per-process temp dir.
fn default_workdir() -> PathBuf {
    if let Ok(runner) = std::env::var("RUNNER_TEMP") {
        return PathBuf::from(runner).join("hotaudio-live");
    }
    std::env::temp_dir().join(format!("hotaudio-live-{}", std::process::id()))
}

struct Config {
    url: String,
    cli: PathBuf,
    workdir: PathBuf,
    report: Option<PathBuf>,
    file_existing: Option<PathBuf>,
    heavy: bool,
    stream: bool,
    keysonly: bool,
    file_issues: bool,
    repo: String,
    label: String,
    list: bool,
}

impl Config {
    fn parse() -> Result<Self, String> {
        let mut cfg = Self {
            url: env("HOTAUDIO_LIVE_URL").unwrap_or_else(|| DEFAULT_URL.to_string()),
            cli: env("HOTAUDIO_CLI")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("target/release/hotaudio-download")),
            workdir: default_workdir(),
            report: None,
            file_existing: None,
            heavy: true,
            stream: true,
            keysonly: true,
            file_issues: false,
            repo: DEFAULT_REPO.to_string(),
            label: DEFAULT_LABEL.to_string(),
            list: false,
        };
        let mut args = std::env::args().skip(1);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--url" => cfg.url = take(&mut args, "--url")?,
                "--cli" => cfg.cli = PathBuf::from(take(&mut args, "--cli")?),
                "--workdir" => cfg.workdir = PathBuf::from(take(&mut args, "--workdir")?),
                "--report" => cfg.report = Some(PathBuf::from(take(&mut args, "--report")?)),
                "--file-existing" => {
                    cfg.file_existing = Some(PathBuf::from(take(&mut args, "--file-existing")?));
                }
                "--repo" => cfg.repo = take(&mut args, "--repo")?,
                "--label" => cfg.label = take(&mut args, "--label")?,
                "--skip-heavy" => cfg.heavy = false,
                "--no-stream" => cfg.stream = false,
                "--skip-keys-only" => cfg.keysonly = false,
                "--file-issues" => cfg.file_issues = true,
                "--list" => cfg.list = true,
                "-h" | "--help" => {
                    print_help();
                    std::process::exit(0);
                }
                other => return Err(format!("unknown option {other:?} (try --help)")),
            }
        }
        // The CLI is spawned with the workdir as its cwd, so a relative program
        // path would resolve against the wrong directory. Pin it to absolute.
        if cfg.cli.is_relative() {
            cfg.cli = std::env::current_dir()
                .map_err(|e| format!("cannot resolve the current directory: {e}"))?
                .join(&cfg.cli);
        }
        Ok(cfg)
    }
}

/// An environment variable, treating an empty value as unset — CI passes an
/// unset repo variable through as the empty string.
fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

fn take(args: &mut impl Iterator<Item = String>, flag: &str) -> Result<String, String> {
    args.next().ok_or_else(|| format!("{flag} needs a value"))
}

fn print_help() {
    println!(
        "hotaudio-live-tests — live acceptance suite for hotaudio.net\n\
         \n\
         Usage: hotaudio-live-tests [OPTIONS]\n\
         \n\
         Options:\n\
         \x20 --url <URL>        Track page (default: $HOTAUDIO_LIVE_URL or the built-in track)\n\
         \x20 --cli <PATH>       hotaudio-download binary (default: $HOTAUDIO_CLI or\n\
         \x20                    target/release/hotaudio-download)\n\
         \x20 --workdir <DIR>    Scratch directory (default: $RUNNER_TEMP/hotaudio-live)\n\
         \x20 --report <FILE>    Write the JSON report here\n\
         \x20 --file-existing <FILE>  File issues from an existing report and exit\n\
         \x20 --skip-heavy       Skip the online run, paging, offline/cached/streaming\n\
         \x20 --no-stream        Skip the ranged streaming check\n\
         \x20 --skip-keys-only   Skip the --keys-only check\n\
         \x20 --file-issues      File/update one issue per broken component (needs gh auth)\n\
         \x20 --repo <OWNER/NAME> Issue target (default: {DEFAULT_REPO})\n\
         \x20 --label <LABEL>    Issue label (default: {DEFAULT_LABEL})\n\
         \x20 --list             List the component ids and exit\n\
         \x20 -h, --help         Show this help"
    );
}
