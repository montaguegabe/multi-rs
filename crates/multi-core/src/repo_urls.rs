//! Repository URL helpers: slugs, short local names, GitHub parsing.

/// Strip a trailing `/` and `.git` for comparison purposes.
pub fn normalize_repo_url(url: &str) -> String {
    let trimmed = url.trim_end_matches('/');
    trimmed.strip_suffix(".git").unwrap_or(trimmed).to_string()
}

/// Final path component of a repository URL.
pub fn derive_repo_slug_from_url(url: &str) -> String {
    normalize_repo_url(url)
        .rsplit('/')
        .next()
        .unwrap_or_default()
        .to_string()
}

/// Shortened local name for slugs prefixed with the workspace name
/// (`t-ide-cli` inside `t-ide/` -> `cli`). Also tries the workspace name
/// without a `-workspace` suffix.
pub fn derive_short_local_name(slug: &str, workspace_name: &str) -> Option<String> {
    if workspace_name.is_empty() {
        return None;
    }
    let mut candidates = vec![workspace_name.to_string()];
    if let Some(trimmed) = workspace_name.strip_suffix("-workspace") {
        if !trimmed.is_empty() {
            candidates.push(trimmed.to_string());
        }
    }
    for candidate in candidates {
        let prefix = format!("{candidate}-");
        if let Some(short) = slug.strip_prefix(&prefix) {
            if !short.is_empty() {
                return Some(short.to_string());
            }
        }
    }
    None
}

/// Explicit local `name` to write to `multi.json` for a URL, if any.
pub fn derive_explicit_local_name(url: &str, workspace_name: &str) -> Option<String> {
    derive_short_local_name(&derive_repo_slug_from_url(url), workspace_name)
}

/// Parse `OWNER/REPO` from HTTPS, SSH and scp-style GitHub URLs.
pub fn parse_github_repo_slug(url: &str) -> Option<String> {
    let normalized = normalize_repo_url(url);
    let path = if let Some(rest) = normalized.strip_prefix("git@github.com:") {
        rest.to_string()
    } else if let Some(rest) = normalized.strip_prefix("ssh://git@github.com/") {
        rest.to_string()
    } else {
        let rest = normalized
            .strip_prefix("https://")
            .or_else(|| normalized.strip_prefix("http://"))?;
        let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
        let host = host.split('@').next_back().unwrap_or(host);
        if host != "github.com" {
            return None;
        }
        path.to_string()
    };
    let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
    if parts.len() != 2 {
        return None;
    }
    Some(format!("{}/{}", parts[0], parts[1]))
}

/// Browser URL for a GitHub remote, or `None` for non-GitHub remotes.
pub fn to_github_web_url(remote_url: &str) -> Option<String> {
    let trimmed = remote_url.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Some(slug) = trimmed.strip_prefix("git@github.com:") {
        let slug = slug.strip_suffix(".git").unwrap_or(slug);
        return (!slug.is_empty()).then(|| format!("https://github.com/{slug}"));
    }
    if let Some(slug) = trimmed.strip_prefix("ssh://git@github.com/") {
        let slug = slug.strip_suffix(".git").unwrap_or(slug);
        return (!slug.is_empty()).then(|| format!("https://github.com/{slug}"));
    }
    if trimmed.starts_with("https://github.com/") || trimmed.starts_with("http://github.com/") {
        let https = trimmed.replacen("http://", "https://", 1);
        return Some(https.strip_suffix(".git").unwrap_or(&https).to_string());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs_and_short_names() {
        assert_eq!(
            derive_repo_slug_from_url("https://github.com/org/t-ide-cli.git"),
            "t-ide-cli"
        );
        assert_eq!(
            derive_short_local_name("t-ide-cli", "t-ide"),
            Some("cli".into())
        );
        assert_eq!(
            derive_short_local_name("t-ide-cli", "t-ide-workspace"),
            Some("cli".into())
        );
        assert_eq!(derive_short_local_name("other", "t-ide"), None);
        assert_eq!(derive_short_local_name("t-ide-", "t-ide"), None);
    }

    #[test]
    fn github_slugs() {
        assert_eq!(
            parse_github_repo_slug("git@github.com:org/repo.git"),
            Some("org/repo".into())
        );
        assert_eq!(
            parse_github_repo_slug("ssh://git@github.com/org/repo"),
            Some("org/repo".into())
        );
        assert_eq!(
            parse_github_repo_slug("https://github.com/org/repo/"),
            Some("org/repo".into())
        );
        assert_eq!(parse_github_repo_slug("https://gitlab.com/org/repo"), None);
        assert_eq!(parse_github_repo_slug("https://github.com/org"), None);
        assert_eq!(
            to_github_web_url("git@github.com:org/repo.git"),
            Some("https://github.com/org/repo".into())
        );
        assert_eq!(to_github_web_url("https://example.com/x"), None);
    }
}
