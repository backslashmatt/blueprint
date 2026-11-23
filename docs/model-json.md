# The model's JSON form

`bp model --json` (M1, #29) prints a `Model`. Nodes and edges are sorted, so the same model always serialises to the same bytes; snapshot tests rely on this. The example below is checked by a test (`crates/model/src/graph.rs`), so it is the real output:

```json
{
  "nodes": [
    {
      "id": "L1:cargo:crates/cli#",
      "kind": "system",
      "name": "cli",
      "source": "crates/cli/Cargo.toml",
      "parent": null,
      "confidence": "exact"
    },
    {
      "id": "L1:cargo:crates/engine#",
      "kind": "system",
      "name": "engine",
      "source": "crates/engine/Cargo.toml",
      "parent": null,
      "confidence": "exact"
    }
  ],
  "edges": [
    {
      "from": "L1:cargo:crates/cli#",
      "to": "L1:cargo:crates/engine#",
      "kind": "depends-on",
      "confidence": "exact",
      "evidence": [],
      "dependency": [
        "normal"
      ]
    }
  ]
}
```

- **`id`**: `L<level>:<ecosystem>:<manifest directory>#<path within the system>`, for example `L2:cargo:crates/engine#git/tree`. The ecosystem is one of `cargo`, `npm`, `go`, `dotnet`, `nx`, `bazel`, `python` and `unowned`.
  - In segments, `%`, `/`, `#` and `:` are escaped as `%25`, `%2F`, `%23` and `%3A`, so two locations never share an id.
  - The id comes from location, never content, so renaming a package keeps its id.
  - Moving a directory changes the id. Detecting a move is the diff's job (M2).
- **`kind`**: `system` or `unowned` at L1, `module` or `directory` at L2, `component` at L3, and `function` at L4. The level is the one encoded in the `id`. L2 is flat within a system: the nested module `git/tree` is its own L2 node. In an npm or Nx package each TypeScript or JavaScript file is a module named by its path without the extension, and an `index.*` file is its directory's (DR 0012). In a .NET project each namespace its C# files declare is a module (`App.Core`), and the parts of a `partial` type are one component (DR 0013). In a Python project each `.py` or `.pyi` file is a module named by its dotted path below the import root (`src/` in the src layout, otherwise the project's directory), and a package's `__init__` is the package (`pkg.sub`) (DR 0014). In a Go module each package (a directory of `.go` files) is a module named by its path in the module (`internal/store`), and a type's methods sit under it whichever file of the package declares them (DR 0015).
- **`source`**: the repository-relative file or directory that defines the node, `/`-separated and normalised.
- **`parent`**: the node one level up, in the same system; `null` only at L1.
- **`confidence`**: `exact`, `inferred` or `unknown` (§3.2).
- **`visibility`** (optional): `public`, `restricted` or `private`, where the language defines it. For a Rust module, these are `pub`, `pub(crate)`/`pub(super)`/`pub(in ..)`, and no `pub`. For TypeScript and JavaScript, `export` is public and anything else private; a class member is public unless `private` or `#name` (private) or `protected` (restricted) (DR 0012). For Python, a name with a leading `_` is private, except a dunder, and any other public (DR 0014). For Go, an exported (upper-case) name is public and any other restricted, and a package under `internal/` is restricted (DR 0015). It is omitted when unknown or not applicable, as for systems and directories.
- **`code`** (optional, L3 and L4 only): where the node's code is, and fingerprints of it for the diff (DR 0008):
  - `lines`: the first and last line in `source`, 1-based and inclusive, starting at the first outer attribute;
  - `body_line` (optional): the line the body starts on, so lines before it are signature;
  - `signature_hash`: a hash of the tokens outside the body, including attributes and, for a method, its `impl` header;
  - `body_hash` (optional): a hash of the body's tokens. It is absent for items with no body, such as structs and required trait methods.

  Both hashes ignore whitespace, comments and the item's own name, so a rename keeps them. `code` is absent for synthetic components (`(functions)`, `impl <Type>`) and for files no analyser reads.
- **Edge `kind`**: `depends-on`, `calls`, `implements`, `extends`, `reads`, `writes` or `exposes`. There is at most one edge per `(from, to, kind)`.
- **Edge `evidence`**: for a rolled-up edge, the `{from, to, kind}` keys of the edges exactly one level down that it stands for.
- **Edge `dependency`** (optional): for `depends-on`, why the dependency exists: `normal`, `build`, or both. It is omitted when the analyser does not distinguish.

The optional fields can be absent in older documents, which still load.

Loading JSON re-applies every rule that `Model::add_node` and `Model::add_edge` enforce, so a malformed or tampered model is rejected rather than loaded.
