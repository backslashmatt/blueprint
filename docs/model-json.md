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
      "evidence": []
    }
  ]
}
```

- **`id`**: `L<level>:<ecosystem>:<manifest directory>#<path within the system>`, for example `L2:cargo:crates/engine#git/tree`.
  - In segments, `%`, `/`, `#` and `:` are escaped as `%25`, `%2F`, `%23` and `%3A`, so two locations never share an id.
  - The id comes from location, never content, so renaming a package keeps its id.
  - Moving a directory changes the id. Detecting a move is the diff's job (M2).
- **`kind`**: `system` or `unowned` at L1, `module` or `directory` at L2, `component` at L3, and `function` at L4. The level is the one encoded in the `id`. L2 is flat within a system: the nested module `git/tree` is its own L2 node.
- **`source`**: the repository-relative file or directory that defines the node, `/`-separated and normalised.
- **`parent`**: the node one level up, in the same system; `null` only at L1.
- **`confidence`**: `exact`, `inferred` or `unknown` (§3.2).
- **Edge `kind`**: `depends-on`, `calls`, `implements`, `extends`, `reads`, `writes` or `exposes`. There is at most one edge per `(from, to, kind)`.
- **Edge `evidence`**: for a rolled-up edge, the `{from, to, kind}` keys of the edges exactly one level down that it stands for.

Loading JSON re-applies every rule that `Model::add_node` and `Model::add_edge` enforce, so a malformed or tampered model is rejected rather than loaded.
