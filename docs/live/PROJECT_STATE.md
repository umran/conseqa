# Project State

A Conseqa project lives in the repository it describes, in a `.conseqa/` directory committed with the code. The architecture model is versioned, reviewed, branched, and merged like any other source, so the repository is the source of truth for both the code and the architecture it claims to implement. Nothing about a project is kept anywhere else on the machine.

Implementation: [`src/confluence/store.rs`](../../src/confluence/store.rs).

## Layout

```text
.conseqa/
  project.yaml                    format, dsl, name, prompt, run policy
  spec/
    services/<id>.yaml            one file per entity
    schemas/<id>.yaml
    data_models/<id>.yaml
    topics/<id>.yaml
    state_machines/<id>.yaml
    operations/<id>.yaml          interface, sketch, program, requirements, stage
    runtime/
      topics/<topic id>.yaml
      subscriptions/<operation id>.yaml   that operation's inputs → runtime
      outboxes/<operation id>.yaml
      execution_pools/<id>.yaml
      routers/<id>.yaml
      storage_layouts/<id>.yaml
  intent/
    obligations.yaml              the prompt's correctness obligations
    proposals.yaml                requirement proposals and their provenance
  .gitignore                      ignores local/
  local/                          this machine's engine state (not committed)
```

Everything except `local/` is committed. The committed state is the authoring workspace (`WorkspaceState`), not the assembled `Model`: an operation may be planned with no program yet, and the prompt, its obligations, and the provenance of each requirement are part of the design record. `conseqa-confluence export` assembles the canonical single-file `conseqa.yaml` from it when every operation has a program.

Empty collections have no directory and no file. `intent/` files exist only when they hold something.

## File format

- **One file per entity.** The file name is the entity's id: letters, digits, `_`, `-`, and `.` (not leading) are kept, and every other byte is written `%XX`, so any id is a file name and ordinary ids read as themselves (`spec/operations/operation.create_order.yaml`). A file name that is not an id's canonical encoding is refused, so two files can never claim one id. Two ids that differ only in letter case are refused at commit, since their files would collide on a case-insensitive filesystem.
- **YAML, canonical.** The engine writes each entity as YAML with every enum as a one-key map (`equals: [a, b]`) rather than a YAML tag (`!equals`): the tag form does not survive the buffering through which internally tagged enums, such as sketch steps, are read back.
- **Only what changed is rewritten.** A commit rewrites the files of the entities it changed and deletes the files of the ones it removed, nothing else. Reformatting a file by hand is not a change to the model. A comment written into a file survives until the engine next rewrites that entity.
- **Strict directories.** `spec/` and `spec/runtime/` may hold only their named directories, and those only `<id>.yaml` files; anything else is refused by name rather than ignored, because a misspelled directory would otherwise make its entities silently vanish. Dotfiles are ignored.

## How the engine uses the files

**The files are authoritative.** Opening a project loads the model from them. Edits made outside the engine — by hand, by `git checkout`, `git pull`, or a merge — are adopted as a revision of their own: before every commit, and before every MCP tool call that reads or changes the model. Adopting a change works like a commit: tasks that observed a changed symbol are invalidated, and every interactive session moves to the new head (a session reads its pinned snapshot, so it would otherwise keep reading what the files no longer say). Change detection is a hash of each file's path, size, modification time, and inode, taken on every request; only when it moves are the files read and compared with the head.

**Changes still go through the commit gate.** Agents change the model through `submit_patch`, which validates every patch, enforces write scopes, and refuses stale work. Editing the files directly bypasses that gate; the analysis of the adopted revision then reports what is wrong.

**A broken file blocks writes.** When the files no longer load — invalid YAML, an unknown field, an unexpected entry — every tool call reports the file and the error, and every commit is refused until the file loads again. The engine never writes over an edit in progress.

**One writer per project.** An open project holds an exclusive lock on `local/lock`, which records the holder's pid. A second process opening it for writing is refused by name; reading it (`status`, `export`) takes no lock.

**Crash safety.** A commit first writes its file changes to `local/pending.json`, then writes each file atomically (a temporary sibling renamed into place), then updates the head pointer and removes the journal. If writing fails, the engine restores the files it had changed and keeps its old head. If the process dies midway, the next open completes the commit from the journal, so the files never hold half of one.

**Revisions are local.** The engine numbers its commits per machine (`local/state.json` holds the head revision and a digest of the files it names). No committed file records a revision, so two branches that each change the model merge like ordinary text; git is the history. A reopened project continues its numbering when its files are unchanged, takes the next number when they changed while it was closed, and starts from 0 on a machine that has never opened it. The model's `revision` is opaque (`CONSEQA_DSL_SEMANTICS.md` §2), so nothing depends on these numbers agreeing across machines.

## `local/`

| File | Holds |
|---|---|
| `lock` | The exclusive writer lock and its holder's pid. |
| `state.json` | The head revision and the digest of the files it names. |
| `pending.json` | A commit's file changes while they are being written. |
| `commits.jsonl` | The commit log: commits through the gate and adopted outside changes. |
| `tasks.jsonl` | Task records and state changes. Tasks still active when a process ended are invalidated on the next open (§84). |
| `dependency_requests.json` | Dependency requests filed by tasks, with their resolutions. |
| `design/` | The last design run's finalized `conseqa.yaml`, verification report, and manifest. |

All of it can be deleted while no engine has the project open; nothing in it is part of the design.

## Versioning

`project.yaml` records the store `format` (currently 9) and the `dsl` contract version the project is authored in. A project of another format or DSL version is refused by name before anything else is read. A `dsl` bump forces a format bump, never conversely. Formats 1 to 8 were the central per-project databases under `~/.conseqa/projects`; they are not read. To bring an older design forward, re-author it, or adopt an exported `conseqa.yaml` with `conseqa-confluence init --model`.

## Opening a project

| Surface | How a project is named |
|---|---|
| MCP `open_project` | `path`: the repository, or any directory inside it. Without a path, the active project is reopened at its head; with none active, the project the server was started in. |
| MCP `create_project` | `path`: the directory to create `.conseqa/` in, normally the repository root. Refused if it, or a directory above it, already holds a project. `name` defaults to the directory's name. |
| `conseqa-confluence stdio` | The project at or above the directory the app starts the server in opens on the first call that needs a project, with no `open_project`. |
| `conseqa-confluence init / serve / status / export` | `--project`, default `.`. `serve` creates the project when none exists; `status` and `export` read without locking. |
| `conseqa-harness design` | `--project`, default `.`. An existing project resumes from its files; `--prompt` or `--model` creates one. |

A server hosts any number of projects at once, keyed by directory. Worker agents of a design run keep their scratch configuration in the system temporary directory, outside every repository, so that no worker picks up a repository's own agent settings.

## Machine configuration

Settings of the machine's server, not of any project, stay in `~/.conseqa/`: the global HTTP server's API key (`api-key`), the flags `stdio` appends to its command line (`stdio.args`), and the System One key and decision log written by `scripts/desktop-system-one.sh`. None of it is project state, and the key files must never be committed.

## Tests

- [`src/confluence/store.rs`](../../src/confluence/store.rs) unit tests: id encoding, case collisions, every fixture and a varied sketch with intent round-tripping through the files, the writer lock, nested projects, outside edits versus reformatting, crash completion, format refusal, and strict directories.
- [`tests/confluence_occ.rs`](../../tests/confluence_occ.rs): restart recovery; an outside edit invalidating exactly the tasks that observed it; an interactive session moving to the adopted head; a broken file refusing commits until fixed.
- [`tests/confluence_mcp.rs`](../../tests/confluence_mcp.rs) and [`tests/confluence_stdio.rs`](../../tests/confluence_stdio.rs): projects created and opened by path over HTTP and stdio, commits landing in each project's own files, and a stdio server started inside a repository opening its project unasked.
