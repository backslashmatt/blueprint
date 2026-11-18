# `bp`, the command-line front end

`bp` is the headless front end to the Blueprint engine. Everything the app can show, `bp` can print, so it is what tests and benchmarks drive.

## Commands

| Command | Effect |
|---|---|
| `bp model <dir>` | Prints the L1–L2 model of a directory as text |
| `bp model <dir> --json` | Prints it as JSON, in the form described in [model-json.md](model-json.md) |
| `bp model <dir> --level 1` | Prints L1 systems and their edges only (`--level 2`, the default, adds L2 modules) |
| `bp model <dir> --level 3`, `--level 4` | Adds L3 components (types, groups of functions, and one component per file no analyser reads), then L4 functions and methods (DR 0008) |
| `bp model <dir> --rev <rev>` | Prints the model of commit `<rev>` of the git repository at `<dir>`, not of its working tree. `<rev>` is a branch, tag, full or abbreviated commit id, `HEAD`, or any rev-parse expression such as `main~2` |
| `bp diff <repo> <base> <head> [--json]` | Prints the architectural diff of two commits |
| `bp index <repo> [--rev <rev>] [--level 2\|4]` | Analyses commit `<rev>` (default `HEAD`) and stores its L1–L2 model in the index, so that `bp model --rev` reopens it without analysing. With `--level 4` it also parses every Rust file and stores its facts |
| `bp --version`, `bp -V` | Prints `bp <version>` |
| `bp --help`, `bp -h`, `bp help`, `bp` | Prints usage |

`bp model` reads the files under `<dir>` and runs every analyser. It never runs anything from the directory (§5.5), never follows symbolic links, and skips `.git`, `.hg`, `.svn`, `target` and `node_modules`.

With `--rev`, `bp model` reads the commit's files from git's object database through `gix`. It never checks anything out, never runs the `git` binary, hooks or filters, and ignores the working tree. Every file in the commit is read, including any committed `target/` or `node_modules/`. Symbolic links are not followed. Submodules, and paths that are not UTF-8, are reported as warnings.

`bp model --rev` reads the model from the index when the commit's tree is already there and the level is 1 or 2. Otherwise it analyses the commit without storing it. It never creates an index. `bp index` analyses and stores it, then prints `<commit> indexed (<n> nodes)`, or `<commit> already indexed (<n> nodes)`.

At levels 3 and 4 the model is read from the index when `bp index --level 4` stored it for the commit's tree. Otherwise it is built, but each Rust file's facts (its items and references, DR 0008) are taken from the index when it holds them for the file's blob, so only files whose content is new are parsed. `bp index --level 4` stores them: it builds the L1–L4 model, stores the facts of every file it had to parse and the whole model in a compact binary form (DR 0009), and prints `<commit> indexed to level 4 (<n> nodes at L1–L2; <p> files parsed, <r> reused)`. Facts are keyed by blob, so a file unchanged between commits is parsed once. `bp index --level 4` exits with code 1, as at level 2, if any file cannot be read or the facts cannot be stored. If a blob is missing locally but its facts are in the index, `bp model --rev` still warns that the file cannot be read, and uses the stored facts for it.

If some of a commit's files cannot be read, for example in a partial clone, each one is reported as a warning. The model is then never stored, and `bp index` exits with code 1.

The index lives in the platform cache, `~/.cache/blueprint/index/` on Linux. Set `BLUEPRINT_CACHE_DIR` to use another directory. Nothing is ever written inside the repository. The index can be deleted at any time. A damaged index is dropped with a warning and rebuilt, never fatal. Models are keyed by the commit's tree id, so commits with the same files share one (DR 0007), and facts by the file's blob id and the analyser's name and version (DR 0008). Entries written by an older version of the analysers are dropped when the index is compacted.

`bp diff <repo> <base> <head>` prints the architectural diff of two commits: findings first (new dependencies across systems or modules, new dependency cycles), then changed nodes, then added and removed edges. Text lines are tab-separated: `finding`, `added`, `removed`, `moved`, `renamed`, `surface`, `contained`, `changed`, `+edge` or `-edge`, then the ids. `--json` prints the format in [diff-json.md](diff-json.md). Two identical commits print nothing. It exits 0 whether or not anything changed.

## Output

- The text form is one line per node, then one per edge, tab-separated. A node line is `id`, `kind`, `name`, `confidence`, then `visibility` if known. An edge line is `from -> to`, `depends-on`, `confidence`, then its dependency kinds.
- The output is deterministic. The same files always print the same bytes, so tests snapshot it.
- Anything an analyser could not fully understand (a malformed manifest, an unresolved module) is printed to stderr as `warning: <path>: <message>`. It does not change the exit code.

## Exit codes

| Code | Meaning |
|---|---|
| 0 | Success, possibly with warnings |
| 1 | The command could not do its job, for example `<dir>` cannot be read, is not a git repository, `<rev>` does not name a commit, or `bp index` cannot read every file or write the index |
| 2 | Usage error: an unknown command, option or value |
