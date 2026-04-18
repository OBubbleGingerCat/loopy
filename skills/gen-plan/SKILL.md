---
name: "loopy:gen-plan"
description: Use when you need to generate a plan from a prompt or draft doc
---

# loopy:gen-plan

## 0. Core Philosophy

`loopy:gen-plan` treats the transformation from draft to plan as a process similar to painting, rather than simple text cleanup or task listing.

This process can be understood in four stages:
- Having an idea: the user provides a draft that expresses goals, intentions, problems, concepts, or scattered information.
- Sketching the outline: the Agent generates the first few layers of the plan tree and establishes the overall structure, major directions, and core branches.
- Adding detail: the Agent progressively expands the structure, refining branches and adding intermediate nodes.
- Coloring in: once a branch has been sufficiently refined, the Agent lands it at the executable level by producing leaf nodes and forming the final plan.

Plan generation is therefore not a one-shot act. The Agent should first grasp the overall composition, then progressively add detail, and finally land on concrete execution.

### 0.1 Node Roles

In this skill, non-leaf nodes and leaf nodes serve different roles.

Non-leaf nodes do not directly carry execution. They are used to organize, constrain, and refine the structure of the plan. They represent directions, layers, or structural units that still need further expansion.

Leaf nodes are the final execution units in the plan tree. They must yield clear, concrete, actionable execution steps.

In other words:
- non-leaf nodes answer “what still needs to be broken down,”
- leaf nodes answer “what exactly should be done now.”

### 0.2 Generation Principles

`loopy:gen-plan` should follow these principles:

- Structure first, detail later.
- Abstraction first, execution later.
- Non-leaf nodes refine structure rather than carry final execution.
- Leaf nodes must be executable.
- The tree should progressively converge from vague, abstract, directional descriptions into concrete, explicit, executable actions.

## 1. Purpose and Scope

`loopy:gen-plan` is an AI Agent skill for transforming a user draft into an actionable tree-structured plan.

The draft may be incomplete, unstructured, ambiguous, or simply a collection of natural language notes, goals, loose ideas, or early requirements. The purpose of this skill is not to directly produce the final deliverable. Instead, it should identify user intent, extract key tasks, constraints, and possible dependencies, and organize them into a clear, extensible, progressively executable plan tree.

This skill is designed for “plan first, execute later” scenarios and helps users turn ideas into execution structure.

## 2. Skill Name

`loopy:gen-plan`

## 3. Use Cases

This skill should be used when the user provides a draft and expects the Agent to transform it into a structured execution plan.

Typical use cases include:
- breaking rough project ideas into task structures,
- turning brainstorming notes into phased plans,
- converting vague goals into milestones, tasks, and subtasks,
- restructuring messy or scattered input into a clear plan tree,
- preparing implementation paths for writing, product, research, operations, or personal workflow tasks.

## 4. Input Definition

The input to this skill is a draft.

The draft may take one of the following forms:
- a natural language paragraph,
- a markdown file,
- a plain text file,
- another readable text-based file.

The draft may include, but is not limited to:
- a goal statement,
- scattered notes,
- a preliminary outline,
- a set of pending tasks,
- an unstructured project or problem description.

The input does not need to be complete and does not need to already contain hierarchy. This skill should be able to extract a plan skeleton from rough input.

## 5. Output Definition

The output of this skill is a markdown file tree.

This file tree is the result of progressively expanding the draft layer by layer. It expresses the hierarchy of the plan using a tree-shaped file structure rather than a single markdown document.

The output must satisfy the following:
- directories and markdown files jointly represent plan hierarchy,
- the root directory preserves the input source and the first expansion layer,
- lower-level directories and files represent further expansion of parent nodes,
- the full file tree expresses decomposition from goal to subtasks,
- the user can understand the whole structure and local detail by navigating the directory tree.

## 6. Invocation

Using Codex as an example, the invocation looks like this:

`$ loopy:gen-plan --input draft.md --output docs/plan`

This means:
- read the draft from `draft.md`,
- write the generated result under `docs/plan`,
- automatically generate an appropriate root directory name based on the draft’s topic and goal,
- create that plan directory under `docs/plan` and generate the full markdown file tree inside it.

## 7. Root Directory Naming

The skill must not use a fixed root directory name.

Instead, it should generate a clear, stable, readable root directory name based on the core goal, topic, or project object of the draft.

The root directory name should:
- reflect the theme of the plan,
- be concise and unambiguous,
- be suitable as a directory name,
- avoid vague, generic, or repetitive wording,
- remain stylistically consistent across similar tasks.

Examples:
- `launch-personal-portfolio-site`
- `ai-product-research-plan`
- `quarterly-retro-preparation`

## 8. Output Tree Structure

The output must be represented as a tree-shaped directory structure.

### 8.1 Root Directory

The root directory must contain:
- a draft file,
- directories corresponding to all first-layer nodes.

The root itself is not a normal task node. It is the container of the whole plan tree.

### 8.2 Non-Leaf Node Structure

Every non-leaf node must be represented as a directory.

That directory must contain:
- a markdown file with the same name as the directory,
- directories or files corresponding to all of its child nodes.

If a child node can be expanded further, it should be represented as a directory. If a child node is already a leaf, it should be represented as a markdown file.

### 8.3 Leaf Node Structure

A leaf node must not be represented as a directory. It should be represented directly as a markdown file.

The leaf file should express the executable content of that node itself and should not create further lower-level structure.

### 8.4 Recursive Rule

The full output tree follows one recursive rule:
- the root contains the draft and first-layer nodes,
- non-leaf nodes are directories,
- non-leaf directories contain “self description file + child nodes,”
- leaf nodes are markdown files,
- each layer is a further expansion of the layer above it.

## 9. Node Content Specification

### 9.1 Non-Leaf Nodes

Non-leaf nodes do not directly carry execution. Their role is to define a scope and break that scope into more specific sub-scopes or task units.

Each non-leaf node must do two things:
- clearly define the scope represented by the node,
- decompose that scope into child nodes and establish references to the corresponding child files.

The markdown file for each non-leaf node should include at least:
- Scope name
- Scope description
- Purpose
- Responsibilities
- Boundaries
- Decomposition
- Child Nodes

Boundary definition is especially important. If a node has no clear boundary, its child nodes are likely to overlap, cross, or omit necessary content.

The focus of a non-leaf node should be:
- what the current scope is,
- why this scope exists,
- where its boundaries lie,
- what child nodes it is decomposed into,
- which part of the scope each child node carries.

A non-leaf node is fundamentally a structural definition and decomposition page, not an execution page.

### 9.2 Leaf Nodes

Leaf nodes are the final execution units in the plan tree.

Each leaf node must:
- define a clear executable task,
- state the goal of the task,
- provide acceptance criteria that can be used to judge completion,
- provide suggested execution steps so that an execution Agent can begin work directly.

A valid leaf node should satisfy the following:
- an execution Agent should not need to ask what exactly should be done,
- the execution Agent should understand the goal and expected result,
- the execution Agent should be able to start directly from the document,
- the execution Agent should be able to self-check completion against the acceptance criteria.

Each leaf markdown file should include at least:
- task name,
- Goal,
- Task Description,
- Acceptance Criteria,
- Suggested Steps.

When necessary, it may also include:
- Inputs,
- Expected Outputs,
- Constraints,
- Notes.

A leaf node should describe what to do, to what degree, and how completion is judged, but it should not replace the implementation process itself.

For example, for a programming task:
- it may specify functional requirements, input-output expectations, acceptance conditions, and suggested steps,
- but it should not include concrete code implementation.

## 10. Leaf Determination Rule

Whether a node should continue to expand must not be judged merely by whether it can still be split. It should be judged by whether its scope has reached a complete state suitable for direct execution.

When a node’s scope is already concrete, no longer vague, and instead boundary-clear, functionally complete, and self-contained, it should be treated as a leaf node rather than expanded further.

A node should usually stop expanding and become a leaf when:
- its scope is already sufficiently concrete and execution goals are clear,
- its boundaries are already clear,
- it can already be understood and executed as a complete task unit,
- its major functions and responsibilities already form a stable whole,
- further decomposition would fragment the integrity of the task,
- further decomposition would reduce the ability to understand and control the task as a whole.

A leaf node is not necessarily the smallest atomic action. It is the most appropriate complete execution unit in the current planning context.

## 11. Must-Expand Rule

If a node’s scope is still too large, too vague, too mixed, or not yet sufficient to support direct execution, it should not be treated as a leaf node and must continue to be expanded.

A node should generally not become a leaf if:
- its scope is still too broad and contains multiple directions,
- its boundaries are still blurry,
- it carries multiple goals, phases, or dimensions that should be separated,
- it contains natural and stable sub-scopes,
- it cannot yet support strong acceptance criteria,
- it can describe direction, but still cannot support an execution Agent starting work.

## 12. Markdown Templates

### 12.1 Non-Leaf Template

```md
# <Node Title>

## Scope
One-sentence definition of the current node’s scope.

## Description
A short explanation of what this node is responsible for in the overall plan.

## Purpose
Why this node is needed and what role it plays in the parent node.

## Responsibilities
- ...
- ...
- ...

## Boundaries
Clearly state what this node includes, what it excludes, and how it differs from sibling nodes.

## Decomposition
Explain how this scope is broken into child nodes and on what basis.

## Child Nodes
- [<Child Node 1>](./<child-node-1>/<child-node-1>.md)
- [<Child Node 2>](./<child-node-2>.md)

## Notes
Additional considerations.
```

### 12.2 Leaf Template

```md
# <Task Title>

## Goal
State the result this task should achieve.

## Task Description
Describe what should be done and where this task fits in the overall plan.

## Inputs
- ...
- ...
- ...

## Expected Outputs
- ...
- ...
- ...

## Acceptance Criteria
- ...
- ...
- ...

## Suggested Steps
1. ...
2. ...
3. ...

## Constraints
- ...
- ...
- ...

## Notes
Risks, reminders, and execution notes.
```

## 13. Naming and Linking Rules

This skill uses the following conventions:
- non-leaf nodes are represented by directories,
- the self-description file of a non-leaf node lives inside that directory,
- the self-description file must have the same name as the directory,
- leaf nodes are represented directly as markdown files,
- all parent references to children must point to the actual markdown files of those children.

### 13.1 Non-Leaf Naming

Example:

`define-scope/`
- `define-scope.md`

### 13.2 Leaf Naming

Example:

`identify-constraints.md`

### 13.3 Linking Rules

- if a child is a non-leaf node, link to `./<child-node-name>/<child-node-name>.md`
- if a child is a leaf node, link to `./<leaf-node-name>.md`

### 13.4 Naming Style

Recommended naming style:
- lowercase letters only,
- hyphen-separated words,
- no spaces, underscores, or mixed casing,
- names should reflect node responsibilities,
- sibling nodes should use a consistent naming style.

## 14. Draft File Rule in the Root Directory

The draft file in the root directory must be named:

`<plan-name>_draft.md`

If the input is already a markdown file, its content must be copied as-is into `<plan-name>_draft.md`.

If the input is not markdown, the content should be normalized into markdown and written into `<plan-name>_draft.md`. This transformation should preserve original intent and avoid unnecessary interpretive rewriting.

The draft file is not a normal plan node. Its role is to:
- preserve the input source,
- provide a way to trace back to original context,
- help the user understand how the plan grew from the original draft.

## 15. Dialogue-Driven Generation Strategy

`loopy:gen-plan` must not generate the full tree in a single shot. It must use a dialogue-driven, layer-by-layer generation strategy.

The generation process should follow these principles:
- dialogue-driven,
- layer-by-layer expansion,
- breadth-first,
- outline first, detail later,
- refine nodes one by one,
- optionally switch to auto mode after a layer is completed.

### 15.1 Why Breadth-First Is Required

Painting does not work by infinitely refining one corner before returning to the whole. It begins with an overall idea, then an outline, then local refinement, and only later fine detail.

Therefore, this skill must use breadth-first generation and must not use depth-first recursive expansion as its default behavior.

### 15.2 Layer Generation Flow

Each layer should follow the same flow:
1. derive the current layer outline from the previous layer,
2. ask the user whether to add or revise anything,
3. refine the nodes in this layer one by one,
4. only after the current layer is sufficiently complete, decide whether to enter the next layer.

### 15.3 Question Presentation

In most cases, the Agent should prefer the format:

**one question + N numbered options**

The user should be allowed to:
- select one option,
- select multiple options,
- select an option with additional comments,
- ignore the options and provide a freeform answer.

If a question is naturally better handled in open discussion, the Agent may skip options and ask openly.

## 16. Dialogue Template Rules

To maintain stable, consistent, controllable interaction quality, the Agent should prefer standardized dialogue templates.

Templates should at least cover:
- layer outline proposals,
- layer confirmations,
- non-leaf refinement,
- leaf refinement,
- auto-generation switching.

When using templates, the Agent should follow these constraints:
- ask only one core question per round whenever possible,
- prefer structured options when they reduce response cost,
- switch to open questions immediately if options would distort the issue,
- avoid fake options created only to satisfy the template pattern,
- avoid asking implementation-detail questions during structural clarification,
- avoid using non-leaf decomposition prompts on nodes that should already be leaves.

The Agent should aim for the minimum number of questions needed to make a stable judgment and only continue asking when key information is still missing.

## 17. State Machine and Transition Rules

The generation process of `loopy:gen-plan` should be treated as a state machine with explicit phases, clear inputs and outputs, and controlled transition conditions.

Core states include:
- `Draft Intake`
- `Plan Naming`
- `Layer Outline Proposal`
- `Layer Outline Confirmation`
- `Node Selection`
- `Non-Leaf Refinement`
- `Leaf Refinement`
- `Layer Completion Review`
- `Auto-Generation`
- `Pause / Stop`

Core transition logic:
- `Draft Intake` -> `Plan Naming`
- `Plan Naming` -> `Layer Outline Proposal`
- `Layer Outline Proposal` -> `Layer Outline Confirmation`
- `Layer Outline Confirmation` -> `Layer Outline Proposal` or `Node Selection` or `Pause / Stop`
- `Node Selection` -> `Non-Leaf Refinement` or `Leaf Refinement`
- `Non-Leaf Refinement` -> `Node Selection` or `Leaf Refinement`
- `Leaf Refinement` -> `Node Selection`
- `Node Selection` -> `Layer Completion Review`
- `Layer Completion Review` -> `Layer Outline Proposal` or `Auto-Generation` or `Pause / Stop`

Core constraints include:
- do not skip layer confirmation and jump directly into deeper nodes,
- do not enter the next layer before the current one is complete,
- do not replace necessary user confirmation with guesswork.

## 18. Final Assembly and File Writing Rules

The final output must be an actual markdown file tree written to the filesystem.

Under the `--output` directory, create:

`<output>/<plan-name>/`

The root must contain:
- `<plan-name>_draft.md`
- directories corresponding to all first-layer nodes.

For every non-leaf node, create:
- `<node-name>/`
- `<node-name>/<node-name>.md`

For every leaf node, create:

`<leaf-node-name>.md`

All `Child Nodes` links must point to the actual markdown files of child nodes and should use relative paths whenever possible.

If the user revises a previously generated node, the Agent must update the corresponding file rather than append a parallel version.

## 19. Exception Handling and Conflict Resolution

Because `loopy:gen-plan` is multi-turn, layered, and dialogue-driven, exceptions are not edge cases. They are normal parts of the workflow.

At minimum, the skill should recognize:
- input exceptions,
- structural exceptions,
- scope exceptions,
- type exceptions,
- naming exceptions,
- dialogue exceptions,
- auto-generation exceptions,
- file exceptions.

The handling mechanism should follow these principles:
- detect problems early rather than hiding them,
- prefer local fixes over full resets,
- ask for key clarification rather than force-completing high-uncertainty issues,
- protect boundary stability in the tree structure,
- preserve rollbackability, interpretability, and maintainability.

## 20. Quality Standards and Self-Check Checklist

The goal of `loopy:gen-plan` is not merely to generate a tree, but to generate a tree that is high-quality, navigable, executable, and maintainable.

Quality should be evaluated along at least the following dimensions:
- Goal Alignment
- Structural Quality
- Scope Quality
- Leaf Executability
- Documentation Quality
- Navigation Quality
- Interaction Quality

After each layer, the Agent should at least check:
- whether the nodes in the layer serve the same parent scope,
- whether sibling node granularity is roughly consistent,
- whether there is obvious overlap or omission,
- whether some nodes should already become leaves,
- whether some leaves should actually continue expanding,
- whether the current layer is stable enough to enter the next layer.

Before final writing or each incremental write, the Agent should at least check:
- whether the root directory name is appropriate,
- whether `<plan-name>_draft.md` exists and is correct,
- whether every non-leaf is a directory with a same-named `.md`,
- whether every leaf is a standalone `.md` file,
- whether all parent-child links exist and are correct,
- whether there are naming conflicts, dangling nodes, or unreferenced nodes,
- whether node content matches the template for its node type.
