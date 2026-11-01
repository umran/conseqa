# The standard realization catalogue

Each `<id>.yaml` here is one entry of the standard catalogue, which `conseqa` embeds at build time (`src/realization/catalogue.rs`). An entry states what one technology, configured a given way, provides in Conseqa's capability vocabulary, what it cannot provide and why, and what it requires of the declaration it realizes and of that declaration's neighbours (`Conseqa_Realization_Profiles_Revision_Specification.md` §7).

A wrong entry misleads every project that uses it, so the rules of §8.3 hold here:

- Every provision, exclusion, and requirement carries a `cite:` of the vendor documentation it rests on, and every cited URL is listed in the entry's `citations`. `tests/realization.rs` checks this.
- An entry with a version-dependent claim states the version range: `postgres.database` is true from 9.5, when `ON CONFLICT DO UPDATE` arrived.
- Every entry is reviewed by someone other than its author before it ships.

Changing anything here changes the standard catalogue, so:

1. bump the entry's own `version` when its claims change, since records and choices name the version they were made against;
2. bump `STANDARD_VERSION` in `src/realization/catalogue.rs`, which every project's `stack.yaml` names;
3. list a new file in `STANDARD_ENTRIES` there. A test fails while a file here is not embedded.

A project that disagrees with an entry copies it into its own catalogue, `.conseqa/realization/catalogue/`, under an id of its own, and says why there.
