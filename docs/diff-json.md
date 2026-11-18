# `bp diff --json`

`bp diff <repo> <base> <head> --json` prints the architectural diff of two commits (REQUIREMENTS §3.3, DR 0008 decision 6) as one JSON object. Both models are built to L4, with each Rust file's facts taken from the index where it holds them. Ids are in the form described in [model-json.md](model-json.md). Every array is sorted, so the same two commits always print the same bytes.

```json
{
  "findings": [
    { "finding": "new-dependency", "edge": { "from": "L2:cargo:#b", "to": "L2:cargo:#a", "kind": "depends-on" } },
    { "finding": "new-cycle", "level": 2, "nodes": ["L2:cargo:#a", "L2:cargo:#b"],
      "new_edges": [{ "from": "L2:cargo:#b", "to": "L2:cargo:#a", "kind": "depends-on" }] }
  ],
  "nodes": [
    { "change": "added", "id": "…" },
    { "change": "moved", "from": "…", "to": "…" }
  ],
  "added_edges": [{ "from": "…", "to": "…", "kind": "calls" }],
  "removed_edges": [],
  "weights": [{ "id": "L4:cargo:#a/(functions)/f", "signature": 2, "body": 2 }]
}
```

## Findings

| `finding` | Meaning |
|---|---|
| `new-dependency` | An edge between two systems (L1) or two modules (L2) that only head has: a new dependency across a boundary. |
| `new-cycle` | A `depends-on` cycle at L1 or L2 (`level`), a strongly connected group of `nodes`, that includes `new_edges` only head has. A cycle that already existed in base, with no new edge, is not reported. |

## Node changes

| `change` | Meaning |
|---|---|
| `added`, `removed` | The node is only in head, or only in base. |
| `moved` | A removed and an added node of the same kind with the same body, under different parents. |
| `renamed` | The same, under the same parent with a different name. |
| `surface` | A node visible outside its parent (`pub` or restricted) whose signature or visibility changed. |
| `contained` | Only the body changed, and no edge from or to the node was added or removed. |
| `changed` | Any other change: a private signature, a body change that added or removed an edge, or a change in confidence or source. |

A node whose lines moved but whose signature and body did not change is not reported. Moves and renames are paired greedily in id order. A body edited as it moved is reported as `removed` plus `added`. A node with no body, such as a struct or an enum, is paired by its whole definition instead: the same kind, name and signature in another parent is `moved`. Such a node cannot be told apart from a new one when renamed, so a renamed struct is `removed` plus `added`.

## Change weight

`weights` lists every node with changed lines, sorted by id. Its lines are lines removed from base plus lines added in head, found by a line diff (`imara-diff`, Histogram) of each file whose content differs.

- Each removed line counts for the innermost base item whose span holds it, and each added line for the innermost head item.
- A line before the item's body, or the line the item starts on, is a `signature` line. Any other line is a `body` line.
- Lines that no item holds, such as `use` lines, count as `body` for the file's own node: its module, or the component of a file no analyser reads.
- Each node's numbers include its descendants', rolled up through each model's parents. A node only in base is weighed through base's parents.

Both models are read from the index when `bp index --level 4` stored them (DR 0009). Otherwise they are built, with each Rust file's facts taken from the index where it holds them.
