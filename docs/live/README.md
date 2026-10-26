# Live Documentation

This directory is the repository's curated source corpus for current documentation generation. A documentation-generating agent may rely on the documents listed under **Live corpus** as descriptions of the current implementation. Revision specifications elsewhere in the repository are historical design records unless their content has been reconciled into this directory.

When sources disagree, use this precedence:

1. [`CONSEQA_DSL_SEMANTICS.md`](CONSEQA_DSL_SEMANTICS.md), the normative DSL 6 semantic contract.
2. The current Rust implementation and tests referenced by the live documents.
3. The remaining explanatory documents in this directory.

## Live corpus

| Document | Role | Currency check |
|---|---|---|
| [`CONSEQA_DSL_SEMANTICS.md`](CONSEQA_DSL_SEMANTICS.md) | Normative DSL and V1 verifier semantics. | Declares DSL contract version 6 and names itself as the single authoritative semantics document. Embedded into the MCP `dsl_guide` by `src/design/mcp.rs`. |
| [`SERIALIZATION_AND_ORDERING_PRIMITIVES_MAP.md`](SERIALIZATION_AND_ORDERING_PRIMITIVES_MAP.md) | Implementation-oriented map of serialization and ordering vocabulary, validation, proof logic, diagnostics, and tests. | Written against the DSL 6 implementation on the current branch. |
| [`TRANSACTION_SERIALIZATION_GRAPH.md`](TRANSACTION_SERIALIZATION_GRAPH.md) | Detailed explanation of the current static transaction-conflict and serialization-graph analysis. | Explicitly revised for DSL 6 atomic conditional mutations. |
| [`PROJECT_STATE.md`](PROJECT_STATE.md) | How a project's state lives in its repository's `.conseqa/` directory: layout, file format, outside edits, locking, crash safety, and the CLI and MCP surfaces that open projects. | Written against the format 9 project store in `src/design/store.rs`. |
| [`CONSEQA_VIZ.md`](CONSEQA_VIZ.md) | User-facing guide to the current self-contained visualization. | Matches the current report overlay, pages, panels, library renderer, and design MCP export surface. |
| [`VIZ_FRONTEND.md`](VIZ_FRONTEND.md) | Developer guide for the current React/Vite visualization frontend and embedded bundle workflow. | Matches the current `viz/` source tree and build commands. |

## Historical design records outside the live corpus

The following documents remain at the repository root for design history. They are not safe standalone sources for current documentation because they are proposed revisions, describe earlier DSL contracts, or have been consolidated into the normative semantics.

| Document | Classification |
|---|---|
| `CONSEQA_AGENT_DESIGN_HARNESS_IMPLEMENTATION_SPEC.md` | Historical implementation plan tied to an earlier baseline; consult current harness and design code instead. |
| `CONSEQA_INLINE_EXECUTION_AND_TYPED_BINDINGS_REVISION.md` | Implemented revision whose semantic result is consolidated into the live DSL contract. |
| `Concise_Conseqa_Outbox_Semantics_Revision.md` | Historical concise outbox proposal; superseded by the live contract and current code. |
| `Conseqa_Asynchronous_Effect_Execution_Revision_Specification.md` | Implemented feature specification; current semantics are consolidated into the live contract. |
| `Conseqa_Atomic_Mutation_and_Serialization_Primitives_Revision_Specification_DSL_v6.md` | DSL 6 design and acceptance record. Useful for rationale, but the live contract is normative and the implementation/tests are authoritative for current behavior. |
| `Conseqa_External_Boundary_Guarantees_and_Decision_Vocabulary_Revision_Specification.md` | Earlier DSL revision record; consolidated into the live contract. |
| `Conseqa_Hierarchical_Semantics_Refactor_Specification.md` | Earlier architecture proposal; consolidated into the live contract and current L0/L1 implementation. |
| `Conseqa_Outbox_Semantics_Revision_Specification.md` | Earlier outbox revision record; consolidated into the live contract. |
| `Conseqa_Retryable_Error_Propagation_Revision_Specification.md` | Implemented DSL 5 revision; DSL 5 documents are incompatible with DSL 6, while the resulting semantics are consolidated into the live contract. |
| `Conseqa_Serialization_Semantics_Revision.md` | Superseded serialization proposal using removed operation-level primitives such as `invocation_lock`. |
| `Conseqa_System_One_Orchestration_Revision_Specification.md` | Historical orchestration implementation plan tied to DSL 4; consult current code and tests for the active behavior. |
| `Conseqa_Transaction_Consistency_and_Ordering_Revision__DSL_v4.md` | DSL 4 design record; its explicit version protocol was replaced by DSL 6 atomic conditional mutations. |
| `Patch_Specification_Topic_Subscription_Runtime_Grouping_and_Ordering_Scope.md` | Historical proposed patch; current runtime semantics are consolidated into the live contract. |

## Documentation-agent boundary

A future documentation-generating agent should:

- load this directory as its documentation corpus;
- treat root-level revision specifications as optional historical context only;
- verify code-level details through the source and test links in the live documents;
- avoid reviving retired vocabulary such as `invocation_lock`, `execution_handoff`, `validate_version`, `bump_version`, or transaction step `write`;
- preserve the separation between application semantics, runtime topology, declared guarantees, requirements, and analyzer-derived proof evidence.
