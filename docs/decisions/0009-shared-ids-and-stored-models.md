# 0009: Shared node ids, and L1–L4 models stored in a compact binary form

**Status:** accepted (agent decision, 2026-11-18). **Issues:** #64 (`diff_medium`, §4: 150 ms), #72 (memory).

## Context

§4 gives the architectural diff of two indexed commits on the medium repository 150 ms. Building both L4 models from stored facts (DR 0008 decision 5) takes about 4 s on medium. Storing the models in the index is the only way under the budget, but loading one stored medium L4 model (31k nodes, 29k edges) as JSON took about 0.8 s:

- about 0.5 s to deserialise, mostly parsing and allocating ids;
- about 0.3 s to insert into the model's maps with every node and edge checked.

Even a trusted bulk build of the maps took 78 ms, and a plain clone of the model 105 ms. The cost was the ids themselves: a `NodeId` was a level, a system key and a path of `String`s, and a model holds each id many times (map key, parent, child sets, edge ends, evidence).

## Options considered

1. **Faster JSON, or a binary encoding of the same structures.** Rejected on its own. The 105 ms clone shows that allocating the ids, whatever the encoding, is already over half the budget for two models.
2. **Fully interned ids:** `u32` handles into a per-model table, with arena storage. This is the strongest option for memory (#72), but it changes every API that takes or returns a `NodeId` across the engine, and ids from two models could no longer be compared directly, which the diff relies on. Deferred: #72 measures whether it is needed for the §4 memory budget.
3. **Shared ids plus a compact stored form. Chosen.**

## Decision

- **A `NodeId` is a reference-counted handle (`Arc`)** to its parts. Cloning it is a count, its API is unchanged, and ids from any two models still compare by value. Measured on medium's L4 model: cloning the model fell from 105 ms to 20 ms, and a bulk build from parts from 78 ms to about 60 ms.
- **`model::codec`** encodes a model to bytes and back:
  - every string once, in a table;
  - every id once, by index;
  - nodes in id order, and edges as id indices.
  
  Decoding shares one `Arc` per id and builds the maps in bulk with `Model::from_trusted_parts`, without checking each insert. Decoding never panics: an out-of-range index or a truncated buffer is an error, and the store's checksums (DR 0005) catch damage first.
- **`bp index --level 4` also stores the L1–L4 model,** under its own key kind and the versions of every analyser that built it. `bp model --rev --level 3|4` and `bp diff` load it when it is there, and otherwise build the model from facts as before.
- **JSON stays the external format** (`docs/model-json.md`). The binary form is internal to the index and versioned with it.

## Revisit when

- #72 shows the medium repository open over the 500 MB budget: intern ids (option 2).
- Loading two stored medium models still misses `diff_medium`: load them on two threads, or diff the encoded forms without building models.
