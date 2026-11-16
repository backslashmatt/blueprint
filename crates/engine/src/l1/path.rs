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

/// One element of a glob segment.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Tok {
    Char(char),
    /// `?`
    Any,
    /// `*`
    Star,
    /// `[..]` or `[!..]`/`[^..]`: inclusive ranges, possibly negated.
    Class(bool, Vec<(char, char)>),
}

/// A directory glob: `*`, `?` and `[..]` classes within a segment, `**` for
/// any number of segments. Matching is polynomial and never recurses.
#[derive(Debug, Clone)]
pub struct Glob {
    segs: Vec<Vec<Tok>>,
    literal: bool,
}

/// The most alternatives one `{..}` pattern may expand to.
const MAX_BRACE_EXPANSIONS: usize = 64;

fn tokens(seg: &str) -> Vec<Tok> {
    let c: Vec<char> = seg.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < c.len() {
        match c[i] {
            '*' => out.push(Tok::Star),
            '?' => out.push(Tok::Any),
            '[' => {
                if let Some(end) = (i + 2..c.len()).find(|&j| c[j] == ']') {
                    let mut body = &c[i + 1..end];
                    let neg = matches!(body.first(), Some('!' | '^'));
                    if neg {
                        body = &body[1..];
                    }
                    let mut ranges = Vec::new();
                    let mut k = 0;
                    while k < body.len() {
                        if k + 2 < body.len() && body[k + 1] == '-' {
                            ranges.push((body[k], body[k + 2]));
                            k += 3;
                        } else {
                            ranges.push((body[k], body[k]));
                            k += 1;
                        }
                    }
                    out.push(Tok::Class(neg, ranges));
                    i = end;
                } else {
                    out.push(Tok::Char('['));
                }
            }
            ch => out.push(Tok::Char(ch)),
        }
        i += 1;
    }
    out
}

/// Expands `{a,b}` alternatives (nested allowed), up to
/// [`MAX_BRACE_EXPANSIONS`] results. `None` if there would be more.
pub fn expand_braces(pattern: &str) -> Option<Vec<String>> {
    let mut done = Vec::new();
    let mut todo = vec![pattern.to_owned()];
    while let Some(p) = todo.pop() {
        // The first `{` whose matching `}` has a top-level comma.
        let chars: Vec<char> = p.chars().collect();
        let mut found = None;
        'open: for (i, &ch) in chars.iter().enumerate() {
            if ch != '{' {
                continue;
            }
            let (mut depth, mut commas) = (0, Vec::new());
            for (j, &d) in chars.iter().enumerate().skip(i) {
                match d {
                    '{' => depth += 1,
                    '}' => {
                        depth -= 1;
                        if depth == 0 {
                            if !commas.is_empty() {
                                found = Some((i, j, commas));
                                break 'open;
                            }
                            break;
                        }
                    }
                    ',' if depth == 1 => commas.push(j),
                    _ => {}
                }
            }
        }
        match found {
            None => done.push(p),
            Some((open, close, commas)) => {
                let head: String = chars[..open].iter().collect();
                let tail: String = chars[close + 1..].iter().collect();
                let mut start = open + 1;
                for end in commas.into_iter().chain([close]) {
                    let alt: String = chars[start..end].iter().collect();
                    todo.push(format!("{head}{alt}{tail}"));
                    start = end + 1;
                }
            }
        }
        if done.len() + todo.len() > MAX_BRACE_EXPANSIONS {
            return None;
        }
    }
    done.sort();
    Some(done)
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
        let mut out: Vec<Vec<Tok>> = Vec::new();
        for s in segs {
            if s == "**" && out.last().is_some_and(|l| l == &[Tok::Star, Tok::Star]) {
                continue;
            }
            out.push(tokens(&s));
        }
        Ok(Some(Glob {
            segs: out,
            literal: !pattern.contains(['*', '?', '[']),
        }))
    }

    /// Compiles a pattern that may contain `{a,b}` alternatives, as npm,
    /// yarn and pnpm accept.
    pub fn compile_all(pattern: &str) -> Result<Vec<Glob>, String> {
        if pattern.len() > MAX_PATTERN_BYTES {
            return Err(format!(
                "pattern of {} bytes ignored as too long",
                pattern.len()
            ));
        }
        let alts = expand_braces(pattern)
            .ok_or_else(|| format!("pattern {pattern:?} expands to too many alternatives"))?;
        let mut out = Vec::new();
        for a in alts {
            out.extend(Glob::compile(&a)?);
        }
        Ok(out)
    }

    /// Whether the pattern has no wildcards and names exactly `rel`.
    pub fn is_literal(&self, rel: &[String]) -> bool {
        self.literal
            && self.segs.len() == rel.len()
            && self.segs.iter().zip(rel).all(|(p, r)| {
                p.iter()
                    .map(|t| match t {
                        Tok::Char(c) => Some(*c),
                        _ => None,
                    })
                    .eq(r.chars().map(Some))
            })
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
            let double = pat[pi] == [Tok::Star, Tok::Star];
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

fn tok_matches(t: &Tok, c: char) -> bool {
    match t {
        Tok::Char(x) => *x == c,
        Tok::Any => true,
        Tok::Star => false,
        Tok::Class(neg, ranges) => ranges.iter().any(|(a, b)| (*a..=*b).contains(&c)) != *neg,
    }
}

/// Matches one segment by iterative backtracking on `*`: O(pattern × text),
/// no recursion.
fn segment_matches(p: &[Tok], s: &[char]) -> bool {
    let (mut pi, mut si) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while si < s.len() {
        if pi < p.len() && p[pi] != Tok::Star && tok_matches(&p[pi], s[si]) {
            pi += 1;
            si += 1;
        } else if pi < p.len() && p[pi] == Tok::Star {
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
    p[pi..].iter().all(|t| *t == Tok::Star)
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
        assert!(g("q/[ab]").matches(&segs("q/a")));
        assert!(!g("q/[!ab]").matches(&segs("q/a")));
        assert!(g("v[0-9]").matches(&segs("v7")));
    }

    #[test]
    fn braces_expand_with_a_limit() {
        assert_eq!(expand_braces("p/{a,b}").unwrap(), vec!["p/a", "p/b"]);
        assert_eq!(
            expand_braces("{x,{y,z}}/w").unwrap(),
            vec!["x/w", "y/w", "z/w"]
        );
        assert_eq!(expand_braces("plain").unwrap(), vec!["plain"]);
        assert!(
            expand_braces(&"{a,b}".repeat(10)).is_none(),
            "2^10 alternatives is too many"
        );
        let all = Glob::compile_all("p/{a,b}").unwrap();
        assert!(all.iter().any(|g| g.matches(&segs("p/b"))));
    }
}
