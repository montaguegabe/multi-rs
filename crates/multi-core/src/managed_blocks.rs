//! `# BEGIN multi-managed: <name>` ... `# END multi-managed: <name>` blocks
//! inside files multi shares with users (for example `.gitignore`).

use std::path::Path;

use crate::error::{Error, Result};
use crate::fsx;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedBlock {
    pub name: &'static str,
    pub comment_prefix: &'static str,
}

impl ManagedBlock {
    pub const fn new(name: &'static str) -> Self {
        ManagedBlock {
            name,
            comment_prefix: "#",
        }
    }

    pub fn begin_marker(&self) -> String {
        format!("{} BEGIN multi-managed: {}", self.comment_prefix, self.name)
    }

    pub fn end_marker(&self) -> String {
        format!("{} END multi-managed: {}", self.comment_prefix, self.name)
    }
}

fn find_block_bounds(lines: &[&str], block: &ManagedBlock) -> Result<Option<(usize, usize)>> {
    let begin_marker = block.begin_marker();
    let end_marker = block.end_marker();
    let begins: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, l)| **l == begin_marker)
        .map(|(i, _)| i)
        .collect();
    let ends: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, l)| **l == end_marker)
        .map(|(i, _)| i)
        .collect();
    if begins.is_empty() && ends.is_empty() {
        return Ok(None);
    }
    if begins.len() != 1 || ends.len() != 1 || begins[0] > ends[0] {
        return Err(Error::Config(format!(
            "Managed block '{}' is malformed.",
            block.name
        )));
    }
    Ok(Some((begins[0], ends[0])))
}

pub fn has_managed_block(text: &str, block: &ManagedBlock) -> Result<bool> {
    let lines: Vec<&str> = text.lines().collect();
    Ok(find_block_bounds(&lines, block)?.is_some())
}

pub fn get_managed_block_lines(text: &str, block: &ManagedBlock) -> Result<Vec<String>> {
    let lines: Vec<&str> = text.lines().collect();
    Ok(match find_block_bounds(&lines, block)? {
        Some((begin, end)) => lines[begin + 1..end]
            .iter()
            .map(|l| l.to_string())
            .collect(),
        None => Vec::new(),
    })
}

/// Replace (or append) the block with `lines`; an empty `lines` removes it.
pub fn replace_managed_block_in_text(
    text: &str,
    block: &ManagedBlock,
    lines: &[String],
) -> Result<String> {
    let existing: Vec<&str> = text.lines().collect();
    let (prefix, mut suffix): (Vec<&str>, Vec<&str>) = match find_block_bounds(&existing, block)? {
        None => (existing.clone(), Vec::new()),
        Some((begin, end)) => {
            let prefix = existing[..begin].to_vec();
            let mut suffix = existing[end + 1..].to_vec();
            if !prefix.is_empty()
                && !suffix.is_empty()
                && prefix.last() == Some(&"")
                && suffix.first() == Some(&"")
            {
                suffix.remove(0);
            }
            (prefix, suffix)
        }
    };

    let mut result: Vec<String> = prefix.iter().map(|l| l.to_string()).collect();
    if lines.is_empty() {
        result.extend(suffix.drain(..).map(str::to_string));
    } else {
        if !result.is_empty() && result.last().map(String::as_str) != Some("") {
            result.push(String::new());
        }
        result.push(block.begin_marker());
        result.extend(lines.iter().cloned());
        result.push(block.end_marker());
        if !suffix.is_empty() {
            if !suffix[0].is_empty() {
                result.push(String::new());
            }
            result.extend(suffix.iter().map(|l| l.to_string()));
        }
    }

    if result.is_empty() {
        return Ok(String::new());
    }
    let joined = result.join("\n");
    Ok(format!("{}\n", joined.trim_end_matches('\n')))
}

/// Replace the block inside the file at `path`, creating or deleting the file
/// as needed.
pub fn replace_managed_block(path: &Path, block: &ManagedBlock, lines: &[String]) -> Result<()> {
    let existing = fsx::read_optional(path)?.unwrap_or_default();
    let next = replace_managed_block_in_text(&existing, block, lines)?;
    if next.is_empty() {
        if path.exists() {
            fsx::remove_file(path)?;
        }
        return Ok(());
    }
    fsx::write(path, next)
}

#[cfg(test)]
mod tests {
    use super::*;

    const BLOCK: ManagedBlock = ManagedBlock::new("test");

    fn lines(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn appends_block_after_blank_line() {
        let text = replace_managed_block_in_text("keep\n", &BLOCK, &lines(&["a", "b"])).unwrap();
        assert_eq!(
            text,
            "keep\n\n# BEGIN multi-managed: test\na\nb\n# END multi-managed: test\n"
        );
    }

    #[test]
    fn replaces_and_removes_block() {
        let initial = "keep\n\n# BEGIN multi-managed: test\na\n# END multi-managed: test\n\ntail\n";
        let replaced = replace_managed_block_in_text(initial, &BLOCK, &lines(&["c"])).unwrap();
        assert_eq!(
            replaced,
            "keep\n\n# BEGIN multi-managed: test\nc\n# END multi-managed: test\n\ntail\n"
        );
        let removed = replace_managed_block_in_text(initial, &BLOCK, &[]).unwrap();
        assert_eq!(removed, "keep\n\ntail\n");
        assert_eq!(
            replace_managed_block_in_text(
                "# BEGIN multi-managed: test\n# END multi-managed: test\n",
                &BLOCK,
                &[]
            )
            .unwrap(),
            ""
        );
    }

    #[test]
    fn malformed_block_is_an_error() {
        assert!(replace_managed_block_in_text("# END multi-managed: test\n", &BLOCK, &[]).is_err());
    }
}
