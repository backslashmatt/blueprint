//! Repository paths and glob patterns as manifests write them.

use blueprint_model::RepoPath;

/// A directory as path segments.
pub type Dir = Vec<String>;

/// Patterns longer than this, in bytes or segments, are rejected, so a hostile
/// manifest cannot make matching expensive.
pub const MAX_PATTERN_BYTES: usize = 1024;
/// See [`MAX_PATTERN_BYTES`].
pub const MAX_PATTERN_SEGMENTS: usize = 64;

/// Segments to a [`RepoPath`]. Segments come from parsed paths or from
/// [`normalise`], so this cannot fail; it falls back to the root anyway.
pub fn repo_path(dir: &[String]) -> RepoPath {
    RepoPath::parse(&dir.join("/")).unwrap_or_else(|_| RepoPath::root())
}

/// Joins a relative path written in a manifest onto `base`, resolving `.`
/// and `..`, accepting `\` as a separator. `None` if it is absolute or
/// climbs above the repository root.
pub fn normalise(base: &[String], rel: &str) -> Option<Dir> {
    let rel = rel.replace('\\', "/");
    if rel.starts_with('/') || rel.contains(':') {
        return None;
    }
    let mut out: Dir = base.to_vec();
    for seg in rel.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                out.pop()?;
            }
            s => out.push(s.to_owned()),
        }
    }
    Some(out)
}

/// A directory glob: `*` and `?` within a segment, `**` for any number of
/// segments. Matching is polynomial and never recurses.
#[derive(Debug, Clone)]
pub struct Glob {
    segs: Vec<Vec<char>>,
    literal: bool,
}

impl Glob {
    /// Compiles a pattern relative to its root. `Ok(None)` if it escapes the
    /// repository; `Err` with a diagnostic message if it is too long.
    pub fn compile(pattern: &str) -> Result<Option<Glob>, String> {
        let Some(segs) = normalise(&[], pattern) else {
            return Ok(None);
        };
        if pattern.len() > MAX_PATTERN_BYTES || segs.len() > MAX_PATTERN_SEGMENTS {
            return Err(format!(
                "pattern of {} bytes ignored as too long",
                pattern.len()
            ));
        }
        // Runs of `**` match the same paths as one.
        let mut out: Vec<Vec<char>> = Vec::new();
        for s in segs {
            if s == "**" && out.last().is_some_and(|l| l == &['*', '*']) {
                continue;
            }
            out.push(s.chars().collect());
        }
        Ok(Some(Glob {
            segs: out,
            literal: !pattern.contains(['*', '?', '[']),
        }))
    }

    /// Whether the pattern has no wildcards and names exactly `rel`.
    pub fn is_literal(&self, rel: &[String]) -> bool {
        self.literal
            && self.segs.len() == rel.len()
            && self
                .segs
                .iter()
                .zip(rel)
                .all(|(p, r)| p.iter().copied().eq(r.chars()))
    }

    /// Whether the relative directory `rel` matches. Dynamic programming over
    /// (pattern position, path position).
    pub fn matches(&self, rel: &[String]) -> bool {
        let pat = &self.segs;
        let dir: Vec<Vec<char>> = rel.iter().map(|d| d.chars().collect()).collect();
        let (np, nd) = (pat.len(), dir.len());
        // ok[pi][di]: pat[pi..] matches dir[di..].
        let mut ok = vec![vec![false; nd + 1]; np + 1];
        ok[np][nd] = true;
        for pi in (0..np).rev() {
            let double = pat[pi] == ['*', '*'];
            for di in (0..=nd).rev() {
                ok[pi][di] = if double {
                    ok[pi + 1][di] || (di < nd && ok[pi][di + 1])
                } else {
                    di < nd && ok[pi + 1][di + 1] && segment_matches(&pat[pi], &dir[di])
                };
            }
        }
        ok[0][0]
    }
}

/// Matches one segment against `*` and `?` by iterative backtracking:
/// O(pattern × text), no recursion.
fn segment_matches(p: &[char], s: &[char]) -> bool {
    let (mut pi, mut si) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while si < s.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == s[si]) {
            pi += 1;
            si += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some((pi, si));
            pi += 1;
        } else if let Some((sp, ss)) = star {
            pi = sp + 1;
            si = ss + 1;
            star = Some((sp, ss + 1));
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|&c| c == '*')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn segs(p: &str) -> Vec<String> {
        p.split('/').map(str::to_owned).collect()
    }

    #[test]
    fn globs_match_like_cargo_and_npm() {
        let g = |p: &str| Glob::compile(p).unwrap().unwrap();
        assert!(g("crates/*").matches(&segs("crates/a")));
        assert!(!g("crates/*").matches(&segs("crates/a/b")));
        assert!(g("packages/**").matches(&segs("packages/a/b")));
        assert!(g("**/x").matches(&segs("x")));
        assert!(g("a?c").matches(&segs("abc")));
        assert!(g("./apps/web").is_literal(&segs("apps/web")));
        assert!(!g("apps/*").is_literal(&segs("apps/web")));
        assert!(Glob::compile(&"*".repeat(2000)).is_err());
        assert!(Glob::compile("../outside").unwrap().is_none());
    }
}
