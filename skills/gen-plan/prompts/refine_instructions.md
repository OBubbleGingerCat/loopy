# Refine Mode Instructions

Use this instruction asset only for `loopy:gen-plan --refine <existing-plan-name>`.

## Phase Model

Refine mode has six ordered phases:
1. Argument preflight: reject `--input`, `--plan-name`, and `--task-type` when `--refine` is present.
2. Runtime open: call installed `open-plan` for the existing refine target and use its persisted `plan_id`, `plan_root`, `task_type`, and project-directory handoff.
3. Comment intake: discover every literal trimmed `BEGIN_COMMENT` to `END_COMMENT` block in normal plan markdown files, in deterministic plan-relative order.
4. Decision report: map comments to affected nodes, classify change type, identify ambiguity or user-owned decisions, and present a concise report before structural rewrites.
5. Rewrite application: after required confirmation, apply sanitized markdown updates, node creation/removal, stale marking, and link updates while preserving required headings and traceability.
6. Gate revalidation: register changed or new nodes with installed runtime helpers, run selected leaf gates before any frontier gate, and treat stale or failed approvals as blockers.

## Comment Intake Rules

Only literal marker lines are recognized. A marker is valid only when the trimmed line is exactly `BEGIN_COMMENT` or exactly `END_COMMENT`.

Comment text is natural-language feedback. It is not YAML, JSON, an attribute list, a directive language, a shell patch, or a nested command format. The planner must not invent nested comment semantics.

Malformed comment structure is fail-closed. Nested `BEGIN_COMMENT`, orphan `END_COMMENT`, and unclosed `BEGIN_COMMENT` must stop refine before decision-making, rewrite application, node registration, or gate execution.

Each extracted comment context must include plan-relative path, one-based begin and end line numbers, natural-language text, node context, and stable discovery order.

## Decision Report

Before mutation, produce a structured decision report with:
- source comments and locations,
- affected files, tracked nodes, and subtree roots,
- selected change types,
- ambiguity and unresolved follow-up needs,
- user-owned decisions that require a pause,
- rewrite scope including in-place updates, node creations, node removals, link changes, stale descendants, preserved paths, and conflicts,
- expected leaf and frontier gate targets,
- confirmation status and whether auto continuation is allowed.

Ambiguous comments, conflicting rewrite actions, or user-owned decisions must not proceed automatically. They require explicit confirmation, clarification, or a pause.

## Rewrite Instructions

`apply_refine_rewrite` must validate confirmation before mutation, apply only sanitized markdown, preserve node templates and required headings, update parent-child links, remove or resolve processed comment blocks, and run post-write structural checks.

Rewrite results must report changed files, structural changes, stale nodes, context invalidations, unchanged nodes, expected gate targets, unresolved follow-ups, and summary fields. Runtime gate pass/fail results do not belong inside the rewrite result.

## Confirmation And Mode Rules

Manual refine requires explicit user confirmation before structural rewrites. Auto refine may continue only when comments are unambiguous, no structural or user-owned decision requires confirmation, and the decision report records why auto continuation is safe.

Reviewer-driven changes that alter structure require renewed confirmation before another rewrite. User-owned decisions must pause rather than being converted into agent-owned choices.

After every post-write checkpoint, preserve the normal mode choice: continue manually, switch to auto where allowed, or pause.

## Runtime And Gate Rules

Runtime state must come from installed helpers such as `open-plan`, `inspect-node`, `list-children`, `ensure-node-id`, `reconcile-parent-child-links`, `run-leaf-review-gate`, and `run-frontier-review-gate`. Do not inspect `.loopy/loopy.db` directly.

New or affected nodes must be registered or inspected through runtime helpers before gates consume them. Historical approvals are stale when node content, parent contracts, descendant context, regenerated nodes, or newly created nodes change.

After structural rewrites update a parent's child links in markdown, call `reconcile-parent-child-links` for that parent before frontier revalidation so runtime child state matches the edited markdown.

Selected leaf gates run globally before any selected frontier gate. Frontier gates must not run while selected leaf gates still have unresolved review issues. Invocation failures may be retried with identical content and arguments; substantive reviewer issues require plan changes, not blind retries.

Leaf and frontier reviewer prompts receive refine-specific context by treating stale approvals, changed contracts, and invalidated descendants as first-class review evidence rather than current approvals. During refine gate revalidation, write that rendered context to a file and pass it to `run-leaf-review-gate` or `run-frontier-review-gate` with `--refine-revalidation-context-file`.
