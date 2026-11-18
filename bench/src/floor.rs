//! The I/O floor: how long it takes just to read every file of a tree. No
//! index can be faster than this, so it bounds the §4 first-index budgets
//! from below and shows how much of each budget is left for analysis.

use std::fs;
use std::io;
use std::path::Path;

/// What a full read of a tree found.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TreeStats {
    /// Regular files read (symlinks are not followed).
    pub files: u64,
    /// Total bytes read.
    pub bytes: u64,
    /// Lines in text files. A file is binary if it contains a NUL byte.
    pub text_lines: u64,
}

/// Reads every regular file under `root`, skipping `.git` directories.
pub fn read_tree(root: &Path) -> io::Result<TreeStats> {
    let mut stats = TreeStats::default();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            if kind.is_dir() {
                if entry.file_name() != ".git" {
                    stack.push(entry.path());
                }
            } else if kind.is_file() {
                let data = fs::read(entry.path())?;
                stats.files += 1;
                stats.bytes += data.len() as u64;
                if !data.contains(&0) {
                    stats.text_lines += count_lines(&data);
                }
            }
        }
    }
    Ok(stats)
}

/// Counts lines the way `git grep -c ''` does: a final line without a
/// trailing newline still counts.
fn count_lines(data: &[u8]) -> u64 {
    let newlines = data.iter().filter(|&&b| b == b'\n').count() as u64;
    match data.last() {
        Some(b'\n') | None => newlines,
        Some(_) => newlines + 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_files_bytes_and_text_lines_and_skips_git() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("a.rs"), "fn a() {}\nfn b() {}\n").unwrap();
        fs::create_dir(root.join("sub")).unwrap();
        fs::write(root.join("sub/b.txt"), "no trailing newline").unwrap();
        fs::write(root.join("sub/bin"), [1u8, 0, 2, b'\n']).unwrap();
        fs::write(root.join("empty"), "").unwrap();
        fs::create_dir(root.join(".git")).unwrap();
        fs::write(root.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();

        let stats = read_tree(root).unwrap();
        assert_eq!(stats.files, 4);
        assert_eq!(stats.bytes, 20 + 19 + 4);
        assert_eq!(stats.text_lines, 2 + 1);
    }

    #[test]
    fn missing_root_is_an_error_not_a_panic() {
        assert!(read_tree(Path::new("/definitely/not/here")).is_err());
    }
}
