# Review Runbook

How we review ADRs and design docs in this repo. The goal is a **falsifiable**
review — reviewers try to break each load-bearing claim, not nod at it. Peer
"LGTM" carries weight only after the claim has survived an attempt to refute it.

## The 8 axes

Every **load-bearing claim** in a doc is scored against all 8. A claim survives
only if it passes **every** axis.

1. **Simplicity / concise** — minimal surface area; no state/column/sentence that
   could be cut. *Fails on:* bloat.
2. **In scope** — decides only what this doc is for; no sprawl (e.g. don't fold a
   RuntimeDriver contract or implementation detail into a state-model ADR).
3. **Factcheck** — runtime behaviour and prior-art claims are true, **with a
   source**. A claim with no source does not pass.
4. **Refute** — assume the claim is *wrong* and try to prove it (adversarial
   default); it survives only if the refutation fails.
5. **Coverage / MECE** — exhaustive and mutually exclusive. Ask "what state /
   edge / runtime situation is missing?" and "can one situation fall into two?"
   (Distinct from Refute: Refute attacks "what you said is wrong"; Coverage
   attacks "you didn't say X".)
6. **Consistency** — sections don't contradict each other (definition ↔ diagram
   ↔ projection ↔ principles) and align with the doc's own first-principles.
7. **Decidable / actionable** — the decision is actually made, and an
   implementer/driver can act on it without ambiguity.
8. **Reversibility / lock-in** — what this locks in and how expensive it is to
   change later.

## Verdict rule

- Score each load-bearing claim across all 8 axes.
- **Refute** defaults to *refuted* — a claim is only "survived" once refutation
  attempts fail.
- **Factcheck** with no source does not pass.
- Report only the axes a claim **fails**, with the counter-example or source.
  Passing axes need no restatement.

## How to run a refute pass

1. Enumerate the doc's load-bearing claims (the ones the decision rests on).
2. Assign refuters; each is told to assume the claim is wrong and produce a
   counter-example, a missing case, or a contradicting source.
3. A claim survives only if no refuter lands. Surviving-with-fixes → fold the
   fix; failed → back to the author.
4. Consolidate into one review comment on the PR; the author decides how to land.

## References (ADR writing)

- **Michael Nygard**, *Documenting Architecture Decisions* — the origin;
  Status / Context / Decision / Consequences.
- **MADR** — Markdown ADR: context → drivers → considered options → decision
  outcome → consequences. <https://adr.github.io/madr/>
- **adr.github.io** — templates and `adr-tools`. <https://adr.github.io/>
- **Joel Parker Henderson**, ADR templates & examples collection.
  <https://github.com/joelparkerhenderson/architecture-decision-record>
- **Y-statement** — one-line decision summary: "In context X, facing Y, we
  decided Z, to achieve W, accepting V."
