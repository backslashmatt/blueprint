# `bp`, the command-line front end

`bp` is the headless front end to the Blueprint engine. Everything the app can show, `bp` can print, so it is what tests and benchmarks drive.

## Commands

| Command | Effect |
|---|---|
| `bp model <dir>` | Prints the L1–L2 model of a directory as text |
| `bp model <dir> --json` | Prints it as JSON, in the form described in [model-json.md](model-json.md) |
| `bp model <dir> --level 1` | Prints L1 systems and their edges only (`--level 2`, the default, adds L2 modules) |
| `bp --version`, `bp -V` | Prints `bp <version>` |
| `bp --help`, `bp -h`, `bp help`, `bp` | Prints usage |

`bp model` reads the files under `<dir>` and runs every analyser. It never runs anything from the directory (§5.5), never follows symbolic links, and skips `.git`, `.hg`, `.svn`, `target` and `node_modules`.

Reading a commit (`--rev`) and persisting the index (`bp index`) need git access, which arrives with #24. Diffs (`bp diff`) arrive in M2.

## Output

- The text form is one line per node, then one per edge, tab-separated. A node line is `id`, `kind`, `name`, `confidence`, then `visibility` if known. An edge line is `from -> to`, `depends-on`, `confidence`, then its dependency kinds.
- The output is deterministic. The same files always print the same bytes, so tests snapshot it.
- Anything an analyser could not fully understand (a malformed manifest, an unresolved module) is printed to stderr as `warning: <path>: <message>`. It does not change the exit code.

## Exit codes

| Code | Meaning |
|---|---|
| 0 | Success, possibly with warnings |
| 1 | The command could not do its job, for example `<dir>` cannot be read |
| 2 | Usage error: an unknown command, option or value |
