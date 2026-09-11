// Declared facts, said the way a reader would say them: a short label for
// a badge, and one sentence on what the fact implies for retries,
// duplicates, and proofs. The DSL's enum names are precise but opaque;
// these are what they mean (CONSEQA_DSL_SEMANTICS.md §8, §9, §13, §17).

import type {
  DeliverySemantics,
  ExternalIdempotency,
  ExternalIdentity,
  ExternalResultReplay,
  IdempotencyGuarantee,
  Input,
  MemberAssignment,
  GroupingKey,
  MemberConcurrency,
  OrderingSemantics,
  OutboxRoutingKey,
  ResultType,
  SubscriptionRoutingKey,
  Topic,
} from "../types/model";
import { pathText } from "./ids";

export type Tone = "success" | "warning" | "neutral" | "info";

export interface Explanation {
  /** Badge text. */
  label: string;
  tone: Tone;
  /** What the fact implies, in one or two sentences. */
  summary: string;
}

/** A transaction's commit deduplication: whether a retry commits again or
 *  recovers the first commit. */
export function commitGuarantee(guarantee: IdempotencyGuarantee): Explanation {
  switch (guarantee.kind) {
    case "deduplicated_by":
      return {
        label: "keyed commit",
        tone: "success",
        summary:
          "Commits are deduplicated by the key: at most one commit per key value. A retry that " +
          "re-encounters this transaction recovers the prior commit — and the results and effect " +
          "intents it established — instead of executing again.",
      };
    case "not_deduplicated":
      return {
        label: "no keyed commit",
        tone: "warning",
        summary:
          "Every attempt commits. A retry is safe only if the body is naturally replayable — " +
          "re-execution reproduces the same logical state — which is not established for writes " +
          "that depend on reads, for inserts and deletes, or for state transitions.",
      };
    case "unspecified":
      return {
        label: "commit deduplication unspecified",
        tone: "warning",
        summary:
          "No fact says whether a retried attempt commits again or recovers the first commit, so " +
          "nothing about replay can be proven through this transaction.",
      };
  }
}

/** Whether an artifact (result, effect intent) established by a transaction
 *  survives for a retry to find. */
export function artifactRetention(guarantee: IdempotencyGuarantee): Explanation {
  switch (guarantee.kind) {
    case "deduplicated_by":
      return {
        label: "durably retained",
        tone: "success",
        summary:
          "Established inside a keyed commit, so it is retained with that commit and recovered " +
          "exactly by any retry that re-encounters the transaction.",
      };
    case "not_deduplicated":
      return {
        label: "not retained across attempts",
        tone: "warning",
        summary:
          "Established by a transaction without a keyed commit, so a retry cannot recover it; it " +
          "can only be reconstructed by re-executing the transaction, which requires natural " +
          "replayability and a replay-deterministic derivation.",
      };
    case "unspecified":
      return {
        label: "retention unknown",
        tone: "warning",
        summary:
          "The establishing transaction declares no commit deduplication fact, so whether a " +
          "retry recovers or reconstructs this artifact is unknown.",
      };
  }
}

export function isolation(level: "unspecified" | "read_committed" | "snapshot" | "serializable"): Explanation {
  switch (level) {
    case "read_committed":
      return {
        label: "read committed",
        tone: "neutral",
        summary:
          "Reads see only committed data, but a value may change between two reads, and " +
          "read-then-write races are possible unless locks or serialization prevent them.",
      };
    case "snapshot":
      return {
        label: "snapshot isolation",
        tone: "neutral",
        summary:
          "All ordinary reads come from one committed snapshot. Write skew and predicate " +
          "anomalies remain possible.",
      };
    case "serializable":
      return {
        label: "serializable",
        tone: "neutral",
        summary:
          "Committed transactions are equivalent to some serial order. This does not imply " +
          "real-time precedence, nor that a retry is safe.",
      };
    case "unspecified":
      return { label: "isolation unspecified", tone: "warning", summary: "No isolation fact may be assumed." };
  }
}

/** A request input's identity declaration. */
export function requestIdentity(identity: Extract<Input, { kind: "request" }>["identity"]): Explanation {
  if (identity.kind === "keyed") {
    const fields = identity.fields.map(pathText).join(", ");
    return {
      label: `keyed identity: ${fields}`,
      tone: "success",
      summary:
        `Two requests with equal ${fields} present equal payloads — the boundary rejects a retry ` +
        "whose payload disagrees with the original. This fixes what one logical request is; it " +
        "deduplicates nothing by itself.",
    };
  }
  return {
    label: "identity unspecified",
    tone: "warning",
    summary:
      "Distinct attempts may present different payloads under equal field values, so nothing " +
      "pins down which requests are retries of the same logical request.",
  };
}

export function delivery(semantics: DeliverySemantics): Explanation {
  switch (semantics) {
    case "at_least_once":
      return {
        label: "at-least-once delivery",
        tone: "info",
        summary:
          "A published message may be delivered more than once, so duplicate invocations must be " +
          "expected. Redelivery is also what re-drives an interrupted invocation.",
      };
    case "at_most_once":
      return {
        label: "at-most-once delivery",
        tone: "info",
        summary: "A message is never delivered twice, but it may be lost.",
      };
    case "unspecified":
      return {
        label: "delivery unspecified",
        tone: "warning",
        summary: "Neither duplicate delivery nor loss can be excluded.",
      };
  }
}

/** How subscription deliveries are grouped into semantic routing
 *  domains. Absence of the routing block is not a mode — it is the
 *  absence of any member-affinity fact — so callers pass `null`. */
export function subscriptionRouting(key: SubscriptionRoutingKey | null | undefined): Explanation {
  if (!key) {
    return {
      label: "no member affinity",
      tone: "warning",
      summary:
        "Deliveries execute within the target pool, and nothing relates same-key deliveries to a " +
        "common member. Not a claim that assignment is arbitrary — that is what a round-robin " +
        "member assignment states — simply no fact.",
    };
  }

  return {
    label: "routed by grouping key",
    tone: "info",
    summary:
      "Deliveries sharing the effective grouping key — declared on the topic runtime or on this " +
      "subscription, whichever holds the scope — belong to one routing domain. The member " +
      "assignment maps that domain onto a pool member; the pool's member concurrency decides " +
      "whether invocations there can overlap.",
  };
}

/** How outbox consumption attempts are grouped into semantic routing
 *  domains — the same two independent facts subscription routing
 *  declares, read against the partition domain. Absence of the routing
 *  block is not a mode; callers pass `null`. */
export function outboxRouting(key: OutboxRoutingKey | null | undefined): Explanation {
  if (!key) {
    return {
      label: "no member affinity",
      tone: "warning",
      summary:
        "Consumption attempts execute within the target pool, and nothing relates " +
        "same-partition attempts to a common member. Not a claim that assignment is arbitrary " +
        "— that is what a round-robin member assignment states — simply no fact.",
    };
  }

  return {
    label: "routed by partition key",
    tone: "info",
    summary:
      "Attempts sharing the declared partition key belong to one routing domain. The member " +
      "assignment maps that domain onto a pool member; the pool's member concurrency decides " +
      "whether invocations there can overlap.",
  };
}

/** The outbox's intrinsic consumption contract — an L0 fact of the
 *  abstraction, present whether or not a runtime is declared. */
export function intrinsicRedrive(): Explanation {
  return {
    label: "intrinsic re-drive",
    tone: "info",
    summary:
      "A committed message stays durably pending, and a pending message keeps admitting " +
      "consumption attempts, until one reaches successful logical completion. Attempts may " +
      "overlap after timeout or uncertainty, so duplicate invocations must be expected — an L0 " +
      "fact of the outbox itself, not a declared delivery semantic.",
  };
}

/** A boundary with no declared realization at all — no pool, no router,
 *  no dispatch. Distinct from a realization that declares a pool and no
 *  routing: that states an execution population and withholds affinity;
 *  this states nothing. */
export function noRuntimeDeclared(): Explanation {
  return {
    label: "no L1 facts",
    tone: "neutral",
    summary:
      "This boundary declares no runtime realization: no execution pool, and so no member " +
      "affinity and no member concurrency. Absence is the absence of a fact, not a realization " +
      "that lacks these properties — and no proof may read it either way.",
  };
}

/** How a routing domain is mapped onto a pool member. */
export function memberAssignment(value: MemberAssignment): Explanation {
  switch (value.kind) {
    case "consistent_hash":
      return {
        label: "consistent-hash assignment",
        tone: "success",
        summary:
          "Equal routing domains are owned by the same pool member during a stable ownership " +
          "epoch, and ownership transfers safely when membership changes. Different domains may " +
          "share a member.",
      };
    case "round_robin":
      return {
        label: "round-robin assignment",
        tone: "warning",
        summary:
          "Each invocation goes to the next member in rotation, irrespective of routing domain. " +
          "Affinity is known not to exist here — a stronger statement than declaring no routing " +
          "at all — so same-key invocations land on different members and no serialization or " +
          "ordering proof can rest on it.",
      };
  }
}

/** Request routing: a semantic key evaluated against the request
 *  payload, or nothing at all. */
export function requestRouting(key: string[][] | null | undefined): Explanation {
  if (!key || key.length === 0) {
    return {
      label: "no member affinity",
      tone: "warning",
      summary:
        "Requests through this boundary execute within the target pool, and nothing relates " +
        "same-key requests to a common member.",
    };
  }

  return {
    label: `routed by ${key.map((path) => path.join(".")).join(", ")}`,
    tone: "info",
    summary:
      "Requests whose values at these fields are equal belong to one semantic routing domain. " +
      "The key names a domain, never a worker, shard, host, or storage partition.",
  };
}

export function memberConcurrency(value: MemberConcurrency): Explanation {
  switch (value.kind) {
    case "bounded":
      return value.value === 1
        ? {
            label: "one invocation at a time per member",
            tone: "success",
            summary:
              "A pool member runs at most one invocation at once, across every workload assigned " +
              "to it. With a routing key that matches, same-key invocations cannot overlap.",
          }
        : {
            label: `up to ${value.value} at a time per member`,
            tone: "warning",
            summary:
              "A pool member may run several invocations at once, so same-key invocations may " +
              "overlap however they are routed.",
          };
    case "unbounded":
      return {
        label: "unbounded member concurrency",
        tone: "warning",
        summary: "No finite member-level execution bound may be assumed.",
      };
    case "unspecified":
      return {
        label: "member concurrency unspecified",
        tone: "warning",
        summary: "No usable fact about simultaneous execution on one pool member.",
      };
  }
}

export function messageIdentity(identity: Topic["message_identity"]): Explanation {
  if (identity.kind === "keyed") {
    return {
      label: "keyed message identity",
      tone: "success",
      summary:
        "One logical message is identified by the mapped fields of its schema: publications with " +
        "equal identity are the same message, however many times it is published.",
    };
  }
  return {
    label: "message identity unspecified",
    tone: "warning",
    summary: "Nothing identifies a logical message, so two publications are two messages even with equal payloads.",
  };
}

/** The transport precedence in force — a realization fact, not a
 *  property of the logical channel, and independent of grouping. */
export function transportOrdering(ordering: OrderingSemantics | undefined): Explanation {
  switch (ordering) {
    case "within_group":
      return {
        label: "ordered within each group",
        tone: "success",
        summary:
          "The transport delivers messages of one runtime group in publication order; " +
          "different groups are unordered relative to each other.",
      };
    case "global":
      return {
        label: "globally ordered",
        tone: "success",
        summary:
          "Every message is part of one ordered sequence — stronger than per-group order, " +
          "and it needs no grouping key of its own. It does not imply ordered execution: " +
          "the execution topology must still preserve the precedence.",
      };
    default:
      return {
        label: "no transport order",
        tone: "warning",
        summary: "No usable precedence guarantee; observed order may not be relied on.",
      };
  }
}

/** The runtime equivalence domains the transport groups into. Enough
 *  on its own for serialization to reason about; ordering is a
 *  separate fact. */
export function transportGrouping(grouping: GroupingKey | undefined): Explanation {
  if (grouping) {
    return {
      label: "grouped by key",
      tone: "success",
      summary:
        "Messages whose key tuples are equal belong to one runtime group. That is all it " +
        "says — not ordering, not serialization, not member assignment, each of which " +
        "needs its own declared fact.",
    };
  }

  return {
    label: "no grouping",
    tone: "warning",
    summary: "The transport establishes no equivalence domain over these messages.",
  };
}

export function externalIdentity(identity: ExternalIdentity): Explanation {
  switch (identity.kind) {
    case "keyed":
      return { label: "keyed interaction identity", tone: "info", summary: "Equal evaluated key tuples are applications of one logical external interaction. Identity alone claims nothing about behaviour — the idempotency and result-replay declarations say what holds across one interaction's applications." };
    case "unspecified":
      return { label: "no interaction identity", tone: "neutral", summary: "No fact says which applications of this boundary are the same logical interaction; per-identity guarantees cannot be declared." };
  }
}

export function externalIdempotency(idempotency: ExternalIdempotency): Explanation {
  switch (idempotency) {
    case "identical_per_identity":
      return { label: "duplicates identical per identity", tone: "success", summary: "Across applications of one keyed interaction, any number of applications produces modeled external work indistinguishable from exactly one, under every admitted interleaving. The property is declared, not the mechanism." };
    case "side_effect_free":
      return { label: "side-effect-free", tone: "success", summary: "Any application causes no modeled externally observable state change beyond producing its synchronous result; duplicates are harmless with no key condition." };
    case "distinguishable":
      return { label: "duplicates distinguishable", tone: "warning", summary: "An explicit negative: repeated applications may produce distinguishable modeled external work — duplicate execution is duplicate work." };
    case "unspecified":
      return { label: "idempotency unspecified", tone: "warning", summary: "No fact says what duplicate applications do to external state." };
  }
}

/** A request input's declared `Result<Ok, Err>` contract. */
export function requestResult(): Explanation {
  return {
    label: "returns Result<ok, err>",
    tone: "info",
    summary:
      "A request through this input completes with exactly one of two typed outcomes: an ok " +
      "payload or an err payload. Err is a logical outcome the boundary returned — a declined " +
      "card, a rejected request — not a crash, a timeout, or a lost connection. The err's " +
      "disposition says whether observing it terminally resolves the logical request or " +
      "semantically admits another attempt; it causes no retry by itself.",
  };
}

/** What an external boundary's result says under its declared
 *  `result_replay` behaviour: `replay_stable` over a keyed identity
 *  fixes one interaction's terminal result, and the error's
 *  disposition decides whether an observed err is that terminal
 *  result. Independent of the idempotency axis. */
export function externalResult(
  result: ResultType | null,
  resultReplay: ExternalResultReplay,
): Explanation {
  if (!result) {
    return {
      label: "no synchronous result",
      tone: "neutral",
      summary: "The boundary returns nothing the program can observe; executing it binds no result.",
    };
  }
  if (resultReplay === "unstable") {
    return {
      label: "returns a per-attempt result",
      tone: "warning",
      summary:
        "The boundary explicitly declares its result unstable: per-attempt results may differ " +
        "(a fresh URL, a fresh nonce), so no terminal result is fixed and a decision on this " +
        "result is not established to replay.",
    };
  }
  if (resultReplay !== "replay_stable") {
    return {
      label: "returns a result",
      tone: "warning",
      summary:
        "The boundary returns Result<ok, err>, and the program may branch on it — but without " +
        "result_replay: replay_stable, nothing fixes the interaction's terminal result, so a " +
        "decision on this result is not established to replay.",
    };
  }
  switch (result.err.disposition) {
    case "terminal":
      return {
        label: "returns a fixed terminal result",
        tone: "success",
        summary:
          "Equal identity keys are one logical interaction whose terminal result the guarantee " +
          "fixes: ok is terminal by definition and the err is declared terminal, so a same-key " +
          "repeat observes the same outcome again and a decision on this result replays " +
          "whenever the identity key is class-fixed.",
      };
    case "retryable":
      return {
        label: "returns a result with a retryable err",
        tone: "info",
        summary:
          "Equal identity keys are one logical interaction, and its terminal ok is fixed — but " +
          "the retryable err conclusively ends only its own attempt: a later same-key " +
          "application may observe a different outcome, so only the ok arm of a decision on " +
          "this result is established to replay.",
      };
    case "unspecified":
      return {
        label: "returns a result",
        tone: "info",
        summary:
          "Equal identity keys are one logical interaction, and its terminal ok is fixed — but " +
          "the err's disposition is unspecified: no fact says whether an observed err " +
          "terminally resolved the interaction, so the err arm of a decision on this result " +
          "is not established to replay.",
      };
  }
}

/** A request effect's result, inherited from the input it targets. */
export function inheritedResult(): Explanation {
  return {
    label: "inherits the target's result",
    tone: "info",
    summary:
      "The request yields the Result<ok, err> its target input declares. Repeated payload-equal " +
      "requests observe the same outcome exactly when the target proves its result " +
      "replay-consistent for that input.",
  };
}

/** A transaction output: data a transaction exports into the program. */
export function transactionOutput(): Explanation {
  return {
    label: "exported by a transaction",
    tone: "info",
    summary:
      "A typed value the transaction establishes atomically with its commit and exposes to the " +
      "steps that follow. It is data, not work: an effect intent is the artifact for that. A " +
      "transaction read never leaves its transaction; this is the only way an observation does.",
  };
}

/** A result binding: an operation-local observation of an effect's outcome. */
export function resultBinding(): Explanation {
  return {
    label: "operation-local observation",
    tone: "neutral",
    summary:
      "The bound result is available to the steps after the binding; its ok payload only inside " +
      "the ok arm of a match on it, its err payload only inside the err arm. It is not a " +
      "transaction artifact and is not durable: a retry re-executes the effect and observes afresh.",
  };
}
