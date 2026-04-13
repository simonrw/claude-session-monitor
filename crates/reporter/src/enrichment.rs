use std::path::PathBuf;
use std::process::Command;

pub struct Enrichment {
    pub hostname: Option<String>,
    pub git_branch: Option<String>,
    pub git_remote: Option<String>,
    pub cwd: String,
}

pub fn gather(raw_cwd: &str) -> Enrichment {
    let hostname = hostname::get().ok().and_then(|h| h.into_string().ok());

    let canonical = std::fs::canonicalize(raw_cwd).unwrap_or_else(|_| PathBuf::from(raw_cwd));
    let cwd = canonical.to_string_lossy().into_owned();

    let git_branch = detect_git_branch(raw_cwd);
    let git_remote = detect_git_remote(raw_cwd);

    Enrichment {
        hostname,
        git_branch,
        git_remote,
        cwd,
    }
}

fn run_command(program: &str, args: &[&str], dir: &str) -> Option<String> {
    let output = Command::new(program)
        .args(args)
        .current_dir(dir)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let s = String::from_utf8(output.stdout).ok()?;
    let trimmed = s.trim().to_owned();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

fn detect_git_branch(dir: &str) -> Option<String> {
    let branch = run_command("git", &["rev-parse", "--abbrev-ref", "HEAD"], dir)?;
    if branch == "HEAD" {
        // Detached HEAD — try jj current-bookmark
        run_command("jj", &["current-bookmark"], dir)
    } else {
        Some(branch)
    }
}

fn detect_git_remote(dir: &str) -> Option<String> {
    run_command("git", &["remote", "get-url", "origin"], dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gather_on_real_git_repo_has_hostname_and_branch() {
        let manifest_dir = std::env::var("CARGO_MANIFEST_DIR")
            .expect("CARGO_MANIFEST_DIR always set during tests");
        let enrichment = gather(&manifest_dir);
        assert!(enrichment.hostname.is_some(), "hostname should be Some");
        let branch = enrichment.git_branch.as_deref().unwrap_or("");
        assert!(
            !branch.is_empty(),
            "git_branch should be non-empty for a git repo"
        );
    }

    #[test]
    fn gather_on_nonexistent_path_has_no_git_info() {
        let enrichment = gather("/nonexistent/path/that/does/not/exist");
        assert!(
            enrichment.hostname.is_some(),
            "hostname should still be Some"
        );
        assert!(
            enrichment.git_branch.is_none(),
            "git_branch should be None for nonexistent path"
        );
        assert!(
            enrichment.git_remote.is_none(),
            "git_remote should be None for nonexistent path"
        );
    }
}
