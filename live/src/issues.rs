//! Turn a failing report into GitHub issues, **one per broken component**.
//!
//! Deduped: a component with an open `[live] <component>` issue gets a new
//! comment instead of a second issue, so a week-long outage is one thread rather
//! than seven.
//!
//! Issue talk goes through `gh` so this stays a thin wrapper over the CLI the
//! workflow is already authenticated for. Everything above it — deciding what
//! broke and what to say — is here, in Rust.

use std::collections::BTreeMap;
use std::io::Write;
use std::process::{Command, Stdio};

use crate::report::Check;

/// File or update one issue per failing component. `repo` is `owner/name`.
pub fn file(
    checks: &[Check],
    url: &str,
    repo: &str,
    label: &str,
    crate_rev: Option<&str>,
) -> Result<usize, String> {
    let mut by_component: BTreeMap<String, Vec<&Check>> = BTreeMap::new();
    for check in checks.iter().filter(|c| !c.ok) {
        by_component
            .entry(check.component.clone())
            .or_default()
            .push(check);
    }
    if by_component.is_empty() {
        return Ok(0);
    }

    // The label has to exist before `gh issue create --label` accepts it. `--force`
    // makes this idempotent; a permission failure here is not fatal.
    let _ = Command::new("gh")
        .args([
            "label",
            "create",
            label,
            "--repo",
            repo,
            "--color",
            "B60205",
            "--description",
            "live acceptance suite failure",
            "--force",
        ])
        .output();

    let open = list_open(repo, label)?;
    let mut filed = 0;
    for (component, failed) in by_component {
        let title = format!("[live] {component}: {}", summarize(&failed[0].detail));
        let body = body(component.as_str(), &failed, url, crate_rev);
        let prefix = format!("[live] {component}");
        let existing = open
            .iter()
            .find(|(_, t)| t.starts_with(&prefix))
            .map(|(n, _)| *n);
        match existing {
            Some(number) => comment(repo, number, &body)?,
            None => create(repo, label, &title, &body)?,
        }
        filed += 1;
    }
    Ok(filed)
}

/// Open issues carrying `label`, as `(number, title)`.
fn list_open(repo: &str, label: &str) -> Result<Vec<(u64, String)>, String> {
    let out = Command::new("gh")
        .args([
            "issue",
            "list",
            "--repo",
            repo,
            "--label",
            label,
            "--state",
            "open",
            "--limit",
            "200",
            "--json",
            "number,title",
        ])
        .output()
        .map_err(|e| format!("cannot run gh: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "gh issue list failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let value: serde_json::Value =
        serde_json::from_slice(&out.stdout).map_err(|e| format!("gh issue list: {e}"))?;
    let mut found = Vec::new();
    if let Some(items) = value.as_array() {
        for item in items {
            let number = item
                .get("number")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0);
            let title = item
                .get("title")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string();
            found.push((number, title));
        }
    }
    Ok(found)
}

fn create(repo: &str, label: &str, title: &str, body: &str) -> Result<(), String> {
    run_with_body(
        &[
            "issue",
            "create",
            "--repo",
            repo,
            "--label",
            label,
            "--title",
            title,
            "--body-file",
            "-",
        ],
        body,
        "gh issue create",
    )
}

fn comment(repo: &str, number: u64, body: &str) -> Result<(), String> {
    run_with_body(
        &[
            "issue",
            "comment",
            &number.to_string(),
            "--repo",
            repo,
            "--body-file",
            "-",
        ],
        body,
        "gh issue comment",
    )
}

fn run_with_body(args: &[&str], body: &str, what: &str) -> Result<(), String> {
    let mut child = Command::new("gh")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot run gh: {e}"))?;
    child
        .stdin
        .take()
        .ok_or_else(|| "gh: no stdin".to_string())?
        .write_all(body.as_bytes())
        .map_err(|e| format!("gh: {e}"))?;
    let out = child
        .wait_with_output()
        .map_err(|e| format!("{what}: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!(
            "{what} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

fn summarize(detail: &str) -> String {
    let one = detail.lines().next().unwrap_or("").trim();
    if one.chars().count() <= 80 {
        one.to_string()
    } else {
        let mut s: String = one.chars().take(77).collect();
        s.push_str("...");
        s
    }
}

fn body(component: &str, failed: &[&Check], url: &str, crate_rev: Option<&str>) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "The scheduled live acceptance suite failed on component `{component}`.\n\n"
    ));
    out.push_str(&format!("- **Track:** {url}\n"));
    if let Some(rev) = crate_rev {
        out.push_str(&format!("- **Crate under test:** `{rev}`\n"));
    }
    if let Some(run) = run_url() {
        out.push_str(&format!("- **Run:** {run}\n"));
    }
    out.push_str("\n### Failing checks\n\n");
    for check in failed {
        out.push_str(&format!("- **{}** — {}\n", check.name, check.detail));
        if !check.evidence.is_empty() {
            out.push_str("  <details><summary>evidence</summary>\n\n  ```\n");
            for (k, v) in &check.evidence {
                out.push_str(&format!("  {k} = {v}\n"));
            }
            out.push_str("  ```\n\n  </details>\n");
        }
    }
    out.push_str(
        "\n_Each scheduled run updates this thread in place while the component stays broken._\n",
    );
    out
}

fn run_url() -> Option<String> {
    let server = std::env::var("GITHUB_SERVER_URL").ok()?;
    let repo = std::env::var("GITHUB_REPOSITORY").ok()?;
    let run = std::env::var("GITHUB_RUN_ID").ok()?;
    Some(format!("{server}/{repo}/actions/runs/{run}"))
}
