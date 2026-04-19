# Gen-Plan Review Gates Design

Date: 2026-04-20
Status: Approved design draft
Scope: `loopy:gen-plan`

## Summary

This design adds two mandatory review barriers to `gen-plan`:

- `leaf review gate`
- `frontier review gate`

The goal is to make plan generation iterate until reviewer issues are resolved, without turning `gen-plan` into a second `submit-loop`.

The design intentionally keeps the runtime thin:

- `Markdown tree` remains the source of truth for plan content.
- A lightweight SQLite workspace persists `plan_id`, `node_id`, reviewer selection, and gate run history.
- Reviewer execution is synchronous and blocking.
- There is no event sourcing, projection system, git worktree, bypass-sandbox mode, or content rollback.

The system is organized into four layers:

1. `Gen-Plan Domain Contract`
2. gate-specific `runtime prompt`
3. task-type `role prompt`
4. a thin runtime that assembles prompts, loads context, runs reviewers, and stores lightweight history

## Goals

- Prevent candidate leaf nodes from being accepted as leaves until they pass an explicit `leaf review gate`.
- Prevent a frontier parent expansion from being considered complete until it passes an explicit `frontier review gate`.
- Preserve the existing interactive, incremental nature of `gen-plan`.
- Keep reviewer orchestration lightweight and purpose-built for plan quality, not execution auditing.
- Support both manual and auto planning modes with the same review quality bar and different remediation routing.

## Non-Goals

- Rebuilding `gen-plan` on top of the `submit-loop` event-sourcing architecture
- Adding git worktrees or bypass-sandbox review execution
- Introducing content rollback or revision history for markdown plan content
- Making reviewer output user-facing raw JSON
- Redesigning the whole `gen-plan` file tree or BFS/manual expansion model

## Fixed Product Decisions

### Plan Location

`gen-plan` no longer takes an `--output` path.

Plans are always created under:

```text
.loopy/plans/<plan-name>/
```

`plan_name` must be unique inside a project workspace.

### Plan Entry Points

The runtime exposes two separate entry points:

- `ensure-plan`: create a new plan only
- `open-plan`: open an existing plan only

`open-plan` may open plans in:

- `active`
- `ready`

It must not reopen `discarded` plans directly.

### IDs

The runtime allocates:

- `plan_id`
- `node_id`

Both identifiers are persisted in SQLite and written into markdown YAML metadata.

### Plan Status

The persisted plan status is intentionally minimal:

- `active`
- `ready`
- `discarded`

`ready` means the current planning scope has converged and all required review gates are free of unresolved issues.

## Review Model

## Gate Types

Two new gate types are mandatory:

### `leaf review gate`

Purpose:
- decide whether a candidate node may stop as a leaf

Properties:
- single reviewer
- no `nonblocking_issues`
- no `improvement_opportunities`
- any issue means the gate fails

Allowed verdicts:
- `approved_as_leaf`
- `revise_leaf`
- `must_expand`
- `pause_for_user_decision`

### `frontier review gate`

Purpose:
- decide whether a frontier parent's current child set is a sound decomposition

Properties:
- single reviewer
- no `nonblocking_issues`
- no `improvement_opportunities`
- any issue means the gate fails

Allowed verdicts:
- `revise_frontier`
- `reopen_parent_scope`
- `pause_for_user_decision`

The `frontier review gate` may also return:

- `invalidated_leaf_node_ids`

to indicate that previously accepted leaf approvals under the current frontier should no longer be trusted.

## Gate Ordering

For one frontier parent:

1. Run `leaf review gate` on all candidate leaves under that parent.
2. Use parallel collect-all semantics for those leaf reviews.
3. If any required leaf review fails, do not run `frontier review gate` yet.
4. After all necessary leaf reviews pass, run `frontier review gate`.

`must_expand` means the node cannot remain a leaf. It becomes a non-leaf node and is expanded later in the normal frontier process. It does not force immediate deeper expansion in the same review round.

## Manual And Auto Mode Semantics

Review standards do not change by mode.
Mode only affects issue routing.

### Manual Mode

For a frontier parent expansion:

1. The planner proposes a candidate expansion.
2. The user confirms the candidate expansion.
3. Required `leaf review gate` runs occur.
4. Leaf-review issues are summarized back to the user, with explanation of why revisions are needed.
5. The planner and user revise until required leaf reviews pass.
6. Then `frontier review gate` runs.
7. Frontier-review issues are summarized back to the user, with explanation of why revisions are needed.
8. The planner and user revise until `frontier review gate` passes.
9. If review caused changes, the user re-approves the revised expansion before continuing.

Manual mode therefore requires both:

- reviewer agreement
- user agreement

before a frontier expansion is considered complete.

### Auto Mode

For a frontier parent expansion:

1. The planner proposes a candidate expansion.
2. Required `leaf review gate` runs occur.
3. Ordinary revision issues are handled by the planner.
4. `pause_for_user_decision` interrupts auto mode and asks the user for the missing decision.
5. After the user answers, auto mode resumes rather than downgrading permanently to manual mode.
6. When required leaf reviews pass, `frontier review gate` runs.
7. Ordinary frontier issues are handled by the planner.
8. `pause_for_user_decision` again interrupts only when the missing choice is genuinely user-owned.

Auto mode therefore preserves the same quality bar while preferring autonomous remediation where safe.

## Runtime Architecture

The runtime is a thin gate service.

It is responsible for:

- allocating `plan_id` and `node_id`
- resolving task-type reviewer selection
- loading current markdown content and plan context
- assembling the reviewer prompt stack
- executing reviewers synchronously
- storing lightweight gate history in SQLite

It is explicitly not responsible for:

- event sourcing
- projection rebuilds
- git worktrees
- bypass-sandbox reviewer execution
- content rollback
- exposing low-level review rounds or invocation orchestration to the planner

## Runtime API

The planner-facing runtime surface is intentionally small.

| API | Purpose | Minimum Input | Minimum Output |
| --- | --- | --- | --- |
| `ensure-plan` | Create a new plan | `plan_name`, `task_type`, `project_directory` | `plan_id`, `plan_root`, `plan_status` |
| `open-plan` | Open an existing plan | `plan_name` | `plan_id`, `plan_root`, `plan_status`, `task_type` |
| `ensure-node-id` | Allocate or load a stable id for a node path | `plan_id`, `relative_path`, `parent_relative_path?` | `node_id` |
| `run-leaf-review-gate` | Review one candidate leaf | `plan_id`, `node_id`, `planner_mode` | `gate_run_id`, `reviewer_role_id`, `passed`, `verdict`, `summary`, `issues` |
| `run-frontier-review-gate` | Review one frontier expansion | `plan_id`, `parent_node_id`, `planner_mode` | `gate_run_id`, `reviewer_role_id`, `passed`, `verdict`, `summary`, `issues`, `invalidated_leaf_node_ids` |

Runtime behavior constraints:

- Both gate APIs are synchronous and blocking.
- The planner passes only target identity and current mode.
- The runtime loads content itself from the plan tree and SQLite metadata.
- The planner does not manually assemble reviewer inputs.
- The planner does not manually orchestrate review rounds or reviewer invocations.

## SQLite Schema

SQLite is used for lightweight metadata and history only.

Each plan is logically isolated by `plan_id` in the same SQLite database.
This plan-specific workspace is separate from `submit-loop` main-loop data.

The minimum schema is:

### `GEN_PLAN__plans`

Stores plan-level metadata.

Suggested fields:
- `plan_id`
- `workspace_root`
- `plan_name`
- `plan_root`
- `task_type`
- `plan_status`
- `created_at`
- `updated_at`

Suggested constraint:
- unique on `(workspace_root, plan_name)`

### `GEN_PLAN__nodes`

Stores stable node identity and current relative path mapping.

Suggested fields:
- `plan_id`
- `node_id`
- `relative_path`
- `node_name`
- `parent_node_id`
- `created_at`
- `updated_at`

Suggested constraints:
- primary key on `(plan_id, node_id)`
- unique on `(plan_id, relative_path)`

### `GEN_PLAN__gate_role_selection`

Stores reviewer role resolution for a plan.

Suggested fields:
- `plan_id`
- `task_type`
- `leaf_reviewer_role_id`
- `frontier_reviewer_role_id`
- `resolved_at`

### `GEN_PLAN__leaf_gate_runs`

Stores leaf gate lightweight history.

Suggested fields:
- `leaf_gate_run_id`
- `plan_id`
- `node_id`
- `reviewer_role_id`
- `planner_mode`
- `passed`
- `verdict`
- `summary`
- `issues_json`
- `created_at`

### `GEN_PLAN__frontier_gate_runs`

Stores frontier gate lightweight history.

Suggested fields:
- `frontier_gate_run_id`
- `plan_id`
- `parent_node_id`
- `reviewer_role_id`
- `planner_mode`
- `passed`
- `verdict`
- `summary`
- `issues_json`
- `invalidated_leaf_node_ids_json`
- `created_at`

## Prompt Stack

Reviewer prompting is split into three layers.

## 1. Gen-Plan Domain Contract

This shared layer explains the ontology of `gen-plan` itself:

- what a plan is
- what a non-leaf node is
- what a leaf node is
- what a frontier parent is
- what a parent-scoped expansion is
- what counts as stopping at leaf
- what reviewer non-goals are

This layer exists because a reviewer cannot reliably judge plan materials without first understanding the tree semantics of `gen-plan`.

## 2. Gate Runtime Prompt

This layer is gate-specific.

It defines:

- gate identity
- planner mode
- allowed verdicts
- issue schema
- input semantics
- evidence priority
- required review procedure
- routing rules for user-owned decisions

It also defines the universal leaf-gate hard test:

> If you were the downstream executor of this node, after reading it, would you still want to ask the planner anything material before starting?

This test belongs to the leaf runtime prompt, not to any task-type role prompt.

## 3. Task-Type Role Prompt

This layer defines professional judgment standards for a specific task type.

For `coding-task`, role prompts define things such as:

- whether repository code must be inspected
- what counts as a material execution gap
- what repository structure must constrain judgment
- what testing expectations are mandatory
- what overlap or missing-slice conditions are materially problematic

The role prompt must not duplicate gate protocol.

## Reviewer Inputs And Evidence Rules

The runtime must not simply dump variables into the reviewer prompt.
It must structure inputs and explain their authority.

For `leaf review`, the runtime provides:

- target leaf node
- parent-scoped expansion snapshot
- plan root path
- planner mode
- project directory

For `frontier review`, the runtime provides:

- parent node
- current expansion snapshot
- passed leaf review results and resolution summaries
- plan root path
- planner mode
- project directory

Important evidence rules:

- The current markdown tree is authoritative for plan content.
- Runtime ids are identifiers, not evidence.
- Planner mode is routing context, not quality justification.
- Historical summaries are supporting context, not substitutes for current markdown content.
- Missing information in the primary review object is still missing, even if surrounding context hints at a likely answer.

The reviewer must proactively inspect the relevant subtree under the plan root path before finalizing judgment.

The minimum relevant subtree includes:

- the target node's ancestor chain
- the full direct-child set of the current parent
- neighboring sibling subtrees when needed for boundary, naming, or decomposition judgment

The reviewer should not default to reading the entire plan tree unless that becomes necessary for a reliable judgment.

## Human-Facing Presentation

Raw machine results may be structured objects, but user-facing presentation must not show bare JSON.

For user-facing output:

- show `Verdict`
- show `Summary`
- show issues in a markdown table

Recommended columns:

- `Issue Kind`
- `Target`
- `Summary`
- `Expected Revision`

If relevant:

- `Question For User`
- `Decision Impact`

JSON examples in docs may still be shown, but only as formatted, pretty JSON.

## Task-Type Role Organization

Reviewer roles are physically separated:

- `leaf_reviewer/`
- `frontier_reviewer/`

Task-type configuration uses separate defaults:

```toml
default_leaf_reviewer = "..."
default_frontier_reviewer = "..."
```

For `coding-task`:

- `leaf_reviewer` judges repository-grounded executability of a leaf
- `frontier_reviewer` judges repository-grounded soundness of the decomposition

The draft reviewer prompt content for these layers is captured in:

- [2026-04-20-gen-plan-leaf-review-prompt-draft.md](/home/user/projects/new/new/git/loopy/docs/superpowers/specs/2026-04-20-gen-plan-leaf-review-prompt-draft.md)
- [2026-04-20-gen-plan-frontier-review-prompt-draft.md](/home/user/projects/new/new/git/loopy/docs/superpowers/specs/2026-04-20-gen-plan-frontier-review-prompt-draft.md)

## Required Changes To `loopy:gen-plan`

The existing skill should be updated by inserting review barriers rather than rewriting the whole skill.

### New Hard Rules

Add two explicit hard rules:

- Every candidate leaf must pass `leaf review gate` before it can be accepted as a leaf.
- Every frontier parent expansion must pass `frontier review gate` before it can be considered complete.

### Manual Flow Changes

Update manual mode so that:

1. The planner proposes a candidate parent-scoped expansion.
2. The user confirms that candidate expansion.
3. Required leaf reviews run.
4. Any leaf-review issues are returned to the user with rationale and proposed revision direction.
5. Leaf review repeats until required leaf nodes are issue-free.
6. Frontier review runs.
7. Any frontier-review issues are returned to the user with rationale and proposed revision direction.
8. Review repeats until frontier review is issue-free.
9. If review changed the expansion, the revised version must be shown to the user again before continuing.

### Auto Flow Changes

Update auto mode so that:

1. The planner proposes a candidate parent-scoped expansion.
2. Required leaf reviews run.
3. Ordinary review issues are self-remediated by the planner.
4. `pause_for_user_decision` interrupts auto mode and asks the user.
5. After the user answers, auto mode resumes.
6. After required leaf reviews pass, frontier review runs.
7. Ordinary frontier issues are self-remediated by the planner.
8. `pause_for_user_decision` again interrupts only for true user-owned decisions.

### Leaf Completion Rule

Strengthen the leaf rule:

- a node may stop at leaf only when it passes `leaf review gate` without issues

### Frontier Completion Rule

Strengthen the frontier completion rule:

- a frontier expansion is complete only when required leaf reviews pass and frontier review passes
- in manual mode, user approval after review-driven changes is also required

### Invalid Behavior Examples

Add anti-patterns such as:

- accepting a leaf without running `leaf review gate`
- running `frontier review gate` while required leaf reviews still have issues
- continuing past a frontier whose frontier review still has issues
- modifying structure after review in manual mode without returning to the user
- making user-owned decisions in auto mode instead of pausing
- treating reviewer issues as optional suggestions

### Self-Check Additions

Add checklist items such as:

- Have all candidate leaves under the current frontier passed required leaf review?
- Are there any unresolved leaf-review issues?
- Has the current frontier passed frontier review?
- Are there any unresolved frontier-review issues?
- In manual mode, did review-driven changes go back to the user?
- In auto mode, did the planner incorrectly skip a true user-owned decision?

## Verification And Testing Expectations

Implementation of this design should verify at minimum:

- plan creation under `.loopy/plans/<plan-name>/`
- duplicate `plan_name` rejection
- opening existing `active` and `ready` plans
- stable `node_id` allocation by relative path
- successful synchronous execution of both gate APIs
- correct storage of lightweight gate history in SQLite
- correct blocking behavior when issues are returned
- correct manual-mode routing of review issues back to the user
- correct auto-mode resumption after `pause_for_user_decision`

## Implementation Notes

- The runtime should read current markdown plan content directly instead of trusting planner-constructed payloads.
- Reviewer execution should use the project directory as the working directory.
- `gen-plan` reviewer execution does not need bypass-sandbox variants.
- The initial implementation should prefer a small, explicit system over abstraction that anticipates future `execute-plan` needs.

## Open Design Status

This design is considered ready for implementation planning.
The runtime is intentionally thin, the reviewer prompt stack is separated clearly, and the required `SKILL.md` insertions are localized rather than architectural rewrites.
