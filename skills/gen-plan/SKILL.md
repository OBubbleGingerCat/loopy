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

### 0.3 Interaction Contract

Unless the user explicitly opts into auto-generation, this skill operates in interactive mode.

Interactive mode is blocking:
- after proposing a layer outline, the Agent MUST stop and wait for explicit user confirmation,
- after completing a layer refinement, the Agent MUST stop and wait for explicit user confirmation before entering the next layer,
- after a layer is confirmed, the Agent MUST write that confirmed layer to disk before starting breadth-first expansion of the next layer,
- after each confirmed layer or confirmed parent-scoped frontier slice is written, the Agent MUST explicitly ask whether to continue manually, switch to auto-generation, or pause before any further expansion.

The Agent MUST NOT infer confirmation from:
- the initial invocation,
- the user’s silence,
- the user’s general desire for a plan,
- the existence of an output path,
- the Agent’s own judgment that a layer is “stable enough.”

Without explicit confirmation, the Agent must not generate deeper layers or the remaining tree.

### 0.4 Frontier-Scoped Manual Expansion

In this skill, breadth-first planning is performed over a frontier of confirmed parent nodes at the current working depth.

In manual mode, the Agent MUST NOT expand the direct children of multiple frontier parents in the same unconfirmed planning step.

Instead, the Agent must:
- select one confirmed parent node from the current frontier,
- expand only that parent node’s direct children,
- ask the user to confirm that parent-scoped expansion,
- write only that confirmed parent-scoped expansion to disk,
- provide a short subtree summary for that parent,
- only then return to the remaining frontier or ask whether to switch to auto-generation.

This means breadth-first is preserved at the depth/frontier level, while manual interaction remains parent-scoped to control context size, readability, and attention.

### 0.4.1 Manual Clarification Gate

In manual mode, the Agent is not limited to mechanically splitting one parent node into child nodes. If the correct child structure depends on an unresolved user choice, missing constraint, or ambiguous assumption, the Agent MUST stop and ask the user before finalizing that parent-scoped expansion.

This applies especially to first-order choices that materially change decomposition, execution boundaries, or downstream artifacts, such as:
- programming language,
- framework,
- database or storage technology,
- deployment target,
- interface style,
- integration direction,
- any other decision that would cause different child nodes, different leaf contracts, or different acceptance criteria.

The Agent may recommend one or more options and explain the tradeoffs, but it MUST NOT silently make that choice on the user’s behalf in manual mode.

If multiple such choices remain unresolved, the Agent should ask them one at a time whenever possible, using question-and-answer rounds until the parent-scoped expansion becomes stable enough to confirm.

### 0.4.2 Auto Clarification Gate

Auto-Generation does not mean the Agent should stop asking questions entirely.

Before automatic expansion of the remaining frontier or remaining layers begins, the Agent MUST first try to resolve the missing user-owned details that would materially affect decomposition, execution boundaries, downstream artifacts, leaf contracts, or acceptance criteria.

This applies especially to first-order choices such as:
- programming language,
- framework,
- database or storage technology,
- deployment target,
- interface style,
- integration direction,
- any other decision that would cause meaningfully different child nodes, leaf outputs, or acceptance checks.

The Agent should front-load these high-leverage clarification questions before entering Auto-Generation whenever practical.

The Agent may recommend defaults or tradeoffs, but it MUST NOT silently convert unresolved user-owned choices into agent-owned decisions merely because Auto-Generation was approved.

## 1. Purpose and Scope

`loopy:gen-plan` is an AI Agent skill for transforming a user draft into an actionable tree-structured plan.

The draft may be incomplete, unstructured, ambiguous, or simply a collection of natural language notes, goals, loose ideas, or early requirements. The purpose of this skill is not to directly produce the final deliverable. Instead, it should identify user intent, extract key tasks, constraints, and possible dependencies, and organize them into a clear, extensible, progressively executable plan tree.

This skill is designed for “plan first, execute later” scenarios and helps users turn ideas into execution structure.

### 1.1 Existing Repository Context Requirement

If the target project is non-empty or the draft depends on an existing codebase, the Agent MUST inspect the relevant repository context before proposing the first layer.

The Agent must not invent plan structure without first reading the project areas that constrain the plan.

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
- create that plan directory under `docs/plan`,
- start the plan generation workflow in interactive mode by default rather than batch generation.

This invocation does not, by itself, authorize full-tree generation in one shot.

If batch-style generation is desired, the user must explicitly request auto-generation for the remaining layers, for example by:
- providing a dedicated auto flag in the surrounding workflow,
- or explicitly stating in natural language that the Agent should generate the remaining layers automatically.

If the surrounding workflow records an early request for auto-generation, that request records user intent only. The Agent may actually enter Auto-Generation only at the next mode-choice checkpoint after a confirmed write.

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
- entries corresponding to all first-layer nodes,
- first-layer non-leaf nodes represented as directories,
- first-layer leaf nodes represented as markdown files directly under the root directory.

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

Every leaf node must also satisfy these hard requirements:
- the node must bind execution to a bounded artifact, change, or verifiable outcome rather than an open-ended planning conversation,
- if inputs are listed, they should prefer explicit upstream files, named artifacts, datasets, configs, tables, or other concrete materials over generic labels such as "project goals" or "requirements",
- if expected outputs are listed, they must name concrete deliverables or checks that the execution Agent can produce directly,
- the node must not hand unresolved first-order product, architecture, or scope decisions back to the execution Agent unless an explicit decision rule is already provided,
- the node must have acceptance criteria that are binary-checkable or strongly falsifiable rather than merely descriptive.

Each leaf markdown file must include at least:
- task name,
- Goal,
- Task Description,
- Inputs,
- Expected Outputs,
- Acceptance Criteria,
- Suggested Steps.

When necessary, it may also include:
- Constraints,
- Notes.

A leaf node should describe what to do, to what degree, and how completion is judged, but it should not replace the implementation process itself.

For example, for a programming task:
- it may specify functional requirements, input-output expectations, acceptance conditions, and suggested steps,
- but it should not include concrete code implementation.

### 9.2.1 Leaf Title and Deliverable Rules

A leaf title should describe an execution unit or deliverable, not an unresolved planning act.

The Agent should treat titles such as `define-*`, `choose-*`, `plan-*`, `identify-*`, and `decide-*` as strong red flags for leaves, because these titles usually indicate that the node is still carrying planner work. If a node truly is executable, prefer titles that reflect the bounded deliverable or action, such as:
- `write-isa-software-contract`
- `produce-pipeline-width-comparison-table`
- `assemble-directed-cache-miss-test-suite`
- `implement-trace-schema-checks`

The same caution applies to expected outputs. Outputs phrased only as "a document", "a plan", "a strategy", "a definition", or "a list" are not sufficient for a leaf unless the artifact structure, completion rule, and expected downstream use are already concretely specified.

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

### 10.1 Leaf Readiness Gate

Before the Agent stops expanding a node and marks it as a leaf, it MUST check all of the following:
- Could a separate execution Agent start immediately without asking the planner to make a major product, architecture, or scope choice?
- Are the required inputs concrete enough that the execution Agent can locate them immediately?
- Are the expected outputs concrete enough that completion produces a bounded artifact, change, or verifiable check?
- Are the acceptance criteria falsifiable without returning to the planner for interpretation?
- Does the node represent one cohesive execution unit rather than multiple bundled workstreams or phases?

If any answer is no, the node is not yet a leaf and must continue expanding.

## 11. Must-Expand Rule

If a node’s scope is still too large, too vague, too mixed, or not yet sufficient to support direct execution, it should not be treated as a leaf node and must continue to be expanded.

A node should generally not become a leaf if:
- its scope is still too broad and contains multiple directions,
- its boundaries are still blurry,
- it carries multiple goals, phases, or dimensions that should be separated,
- it contains natural and stable sub-scopes,
- it cannot yet support strong acceptance criteria,
- it can describe direction, but still cannot support an execution Agent starting work,
- it still mainly asks the execution Agent to "define", "choose", "plan", "identify", or "decide" something at the same abstraction level as the planner,
- its inputs are generic labels rather than concrete upstream artifacts or clearly discoverable materials,
- its expected outputs are abstract planning documents rather than bounded deliverables,
- it bundles several independent deliverables or multiple phases into one node,
- finishing it would still leave the execution Agent naturally asking "which option should I pick?" or "what exactly should I produce?"

### 11.1 Leaf Red Flags

The following are strong red flags that a node is not yet a true leaf:
- the node title is planner-shaped, such as `define-*`, `choose-*`, `plan-*`, `identify-*`, or `decide-*`,
- the expected outputs are only "a document", "a plan", "a strategy", or "a list",
- the acceptance criteria rely on words like "clear", "reasonable", "appropriate", "well-designed", or "can be used later" without stronger checks,
- the inputs refer only to vague context such as "requirements", "project goals", or "design results" without concrete references,
- the execution Agent would still need to ask the planner to arbitrate among materially different options,
- the node clearly covers a phase, stream, or package of work rather than one bounded execution unit

These red flags do not merely suggest lower quality. In most cases they mean the node must continue expanding.

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

Leaf template notes:
- `Inputs` should prefer explicit upstream files, artifact names, datasets, configs, tables, or other concrete references whenever available.
- `Expected Outputs` should name concrete deliverables, checks, tables, configs, scripts, files, or other bounded artifacts rather than generic "document/plan/definition" phrasing.
- If a leaf truly has no external inputs, the `Inputs` section should say `None` rather than being omitted.
- If meaningful choices still remain inside the task, the leaf should either include the decision rule explicitly or continue expanding.

## 13. Naming and Linking Rules

This skill uses the following conventions:
- non-leaf nodes are represented by directories,
- the self-description file of a non-leaf node lives inside that directory,
- the self-description file must have the same name as the directory,
- leaf nodes are represented directly as markdown files,
- all parent references to children must point to the actual markdown files of those children.

### 13.1 Non-Leaf Naming

Example:

`runtime-validation/`
- `runtime-validation.md`

### 13.2 Leaf Naming

Example:

`produce-constraint-matrix.md`

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
- frontier-scoped expansion in manual mode,
- outline first, detail later,
- refine nodes one by one,
- summarize completed parent subtrees before moving on,
- switch to auto mode only when the user explicitly opts in.

### 15.1 Why Breadth-First Is Required

Painting does not work by infinitely refining one corner before returning to the whole. It begins with an overall idea, then an outline, then local refinement, and only later fine detail.

Therefore, this skill must use breadth-first generation and must not use depth-first recursive expansion as its default behavior.

Breadth-first here means controlling the active depth frontier, not dumping every sibling parent’s children in a single response. In manual mode, the Agent should progress across the frontier one parent at a time.

### 15.2 Layer Generation Flow

Each layer should follow the same flow:
1. derive the current layer outline from the previous layer,
2. ask the user whether to add or revise anything,
3. ask the user to confirm the current layer outline,
4. write the confirmed layer outline to the filesystem,
5. ask the user whether to continue manually, switch to auto-generation, or pause,
6. if the user chooses manual mode, select one parent node from the current frontier,
7. expand only that parent node’s direct children,
8. if any unresolved user choice or missing constraint would materially change that expansion, ask the necessary clarification question or questions and wait for the user,
9. ask the user to confirm that parent-scoped expansion,
10. write the confirmed parent-scoped expansion to the filesystem,
11. provide a subtree summary for that parent,
12. ask the explicit mode-choice question and indicate which same-frontier parent nodes remain,
13. if the user chooses to continue manually, repeat steps 6-12 for the remaining frontier,
14. only after the current breadth frontier has been processed may the Agent derive the next breadth-first layer.

### 15.2.1 Mode Choice Gate

After every successful write checkpoint, the Agent MUST ask a single explicit mode-choice question before continuing.

The preferred options are:
1. continue manually with the next parent node,
2. switch to auto-generation for the remaining frontier or remaining layers,
3. pause or stop.

This is a mutually exclusive mode-choice checkpoint. Exactly one mode may be active after the user’s response.

If the user’s response is ambiguous, mixed, or conflicting, the Agent must ask a clarification question rather than infer precedence.

The Agent must not skip this question, even if interactive mode is still the default.

### 15.2.2 Parent Subtree Summary Rule

When a manual-mode parent node has had its direct children confirmed and written, the Agent MUST provide a compact subtree summary before moving to another parent or a deeper layer.

That summary should include at least:
- the completed parent node,
- the confirmed direct child nodes,
- which children are leaves and which remain expandable,
- which same-frontier parent nodes remain unresolved.

### 15.3 Question Presentation

In most cases, the Agent should prefer the format:

**one question + N numbered options**

The user should be allowed to:
- select one option,
- select multiple compatible options when the question is not mutually exclusive,
- select an option with additional comments,
- ignore the options and provide a freeform answer.

If a question is naturally better handled in open discussion, the Agent may skip options and ask openly.

### 15.4 Auto-Generation Entry Rule

Auto-Generation is opt-in only.

The Agent may enter Auto-Generation only after the user explicitly says one of the following or an equivalent instruction:
- continue to the next layer automatically,
- generate the remaining layers automatically,
- switch to auto mode.

This approval should be obtained at a mode-choice checkpoint after a confirmed write. The Agent must not treat silence, momentum, or the absence of objections as persistence of a previous mode choice.

If the surrounding workflow recorded an earlier auto-generation request, that earlier request counts only as user intent and does not authorize an immediate transition. The actual transition may occur only at the next mode-choice checkpoint after a confirmed write.

Explicit approval for Auto-Generation authorizes automatic continuation only after the Auto Clarification Gate has been satisfied. It does not authorize immediate blind generation in the presence of material unresolved user-owned choices.

The Agent must not enter Auto-Generation based on:
- the initial command alone,
- the Agent’s own judgment that the current layer is complete,
- the need to reduce context pressure,
- the existence of an output directory.

### 15.4.1 Auto-Generation Clarification Gate

Before entering Auto-Generation, the Agent MUST inspect the confirmed context and remaining planning surface for missing details that are still best resolved by the user.

If any unresolved user choice, missing constraint, or ambiguous assumption would materially change decomposition, execution boundaries, leaf contracts, artifact choices, or acceptance criteria, the Agent MUST ask clarification questions first and wait for the user’s answers before beginning automatic expansion.

The Agent should try to ask the minimum high-leverage clarification questions needed to reduce future guesswork. The goal is not to interrogate the user for every minor preference, but to resolve the details that would otherwise cause structural drift or hidden planner decisions during Auto-Generation.

If the user explicitly delegates a choice, approves a recommended default, or instructs the Agent to proceed despite a named uncertainty, the Agent may record that instruction and continue.

### 15.4.2 Auto-Generation Leaf Lint Gate

Entering Auto-Generation does not relax leaf quality requirements.

Before the Agent emits any node as a leaf while in Auto-Generation, it MUST run the following leaf lint:
- Is the node free of unresolved first-order product, architecture, and scope choices?
- Are the inputs concrete enough for another Agent to locate immediately?
- Are the outputs concrete enough to produce a bounded artifact, change, or verifiable check?
- Are the acceptance criteria falsifiable without returning to the planner?
- Is the node one cohesive execution unit rather than a bundled phase or workstream?

If any answer is no, the Agent must continue expanding rather than stop at that node.

The Agent must not use Auto-Generation as justification for stopping expansion early, collapsing planner work into leaves, or trading leaf executability for context compression.

## 16. Dialogue Template Rules

To maintain stable, consistent, controllable interaction quality, the Agent should prefer standardized dialogue templates.

Templates should at least cover:
- layer outline proposals,
- layer confirmations,
- mode-choice checkpoints,
- non-leaf refinement,
- leaf refinement,
- parent subtree summaries,
- auto-generation switching.

When using templates, the Agent should follow these constraints:
- ask only one core question per round whenever possible,
- prefer structured options when they reduce response cost,
- switch to open questions immediately if options would distort the issue,
- avoid fake options created only to satisfy the template pattern,
- avoid asking implementation-detail questions during structural clarification unless the answer would materially change decomposition, node boundaries, execution contracts, or acceptance criteria,
- avoid using non-leaf decomposition prompts on nodes that should already be leaves,
- in manual mode, avoid asking the user to review expansions for multiple parent nodes in the same round,
- do not move from one completed parent node to another without first giving the required subtree summary.

The Agent should aim for the minimum number of questions needed to make a stable judgment and only continue asking when key information is still missing.

## 17. State Machine and Transition Rules

The generation process of `loopy:gen-plan` should be treated as a state machine with explicit phases, clear inputs and outputs, and controlled transition conditions.

Core states include:
- `Draft Intake`
- `Plan Naming`
- `Layer Outline Proposal`
- `Layer Outline Confirmation`
- `Mode Choice`
- `Frontier Parent Selection`
- `Non-Leaf Refinement`
- `Leaf Refinement`
- `Layer Completion Review`
- `Layer Write`
- `Parent Subtree Summary`
- `Auto Clarification`
- `Auto-Generation`
- `Pause / Stop`

Core transition logic:
- `Draft Intake` -> `Plan Naming`
- `Plan Naming` -> `Layer Outline Proposal`
- `Layer Outline Proposal` -> `Layer Outline Confirmation`
- `Layer Outline Confirmation` -> `Layer Outline Proposal` or `Layer Write` or `Pause / Stop`
- `Layer Write` -> `Mode Choice` or `Parent Subtree Summary` or `Auto-Generation` or `Pause / Stop`
- `Mode Choice` -> `Frontier Parent Selection` or `Auto Clarification` or `Pause / Stop`
- `Frontier Parent Selection` -> `Non-Leaf Refinement` or `Leaf Refinement`
- `Non-Leaf Refinement` -> `Layer Completion Review`
- `Leaf Refinement` -> `Layer Completion Review`
- `Layer Completion Review` -> `Layer Write` or `Pause / Stop`
- `Auto Clarification` -> `Auto Clarification` or `Auto-Generation` or `Pause / Stop`
- `Parent Subtree Summary` -> `Mode Choice` or `Frontier Parent Selection` or `Layer Outline Proposal` or `Pause / Stop`

Core constraints include:
- do not skip layer confirmation and jump directly into deeper nodes,
- do not enter the next layer before the current one is complete,
- do not replace necessary user confirmation with guesswork,
- do not enter `Layer Write` before the user has explicitly confirmed the current layer,
- do not enter the next layer before the confirmed current layer has been written to disk,
- do not skip the mode-choice checkpoint after a confirmed write,
- do not expand multiple frontier parents in one manual round,
- do not enter `Auto-Generation` before the Auto Clarification Gate has been satisfied,
- do not move to a sibling parent or a deeper layer before emitting the required parent subtree summary.

### 17.1 Confirmation Gate Checklist

Before generating any deeper layer or entering Auto-Generation, the Agent must check:
- Has the current layer or current parent-scoped frontier slice been explicitly confirmed by the user?
- Has the confirmed layer or slice already been written to disk?
- Has the required mode-choice checkpoint already been asked if the Agent intends to continue interactively?
- Has the just-completed parent node already been summarized before moving to another parent or a deeper layer?
- Has the user explicitly approved Auto-Generation if the Agent intends to use it?
- Have the material user-facing clarification questions required for Auto-Generation already been asked and resolved, delegated, or explicitly waived by the user?

If any answer is no, the Agent must stop and ask rather than continue.

## 18. Final Assembly and File Writing Rules

The final output must be an actual markdown file tree written to the filesystem.

Under the `--output` directory, create:

`<output>/<plan-name>/`

### 18.0 Writing Gate

The Agent MUST NOT write the full output tree to the filesystem until one of the following is true:
- the user has confirmed all required layers,
- or the user has explicitly approved Auto-Generation for the remaining layers.

Before that point, the Agent may:
- write the draft file,
- write only the nodes, layers, and parent-scoped frontier slices that have already been explicitly confirmed by the user.

The Agent must not write speculative deeper layers, speculative sibling-parent expansions, or any structure that has not yet been confirmed or explicitly authorized through Auto-Generation.

### 18.1 Incremental Layer Writing Rule

This skill uses incremental writing to reduce context pressure and preserve confirmed structure.

After each layer is confirmed:
- the Agent MUST write that layer’s markdown files and directories to disk,
- the Agent MUST update parent-to-child links for the confirmed layer,
- the Agent MUST verify that the written layer matches the confirmed structure before starting the next breadth-first layer.

After each confirmed parent-scoped frontier slice in manual mode:
- the Agent MUST write only that confirmed slice’s markdown files and directories to disk,
- the Agent MUST update parent-to-child links for that slice,
- the Agent MUST verify that the written slice matches the confirmed structure,
- the Agent MUST provide the required parent subtree summary before moving to another parent or a deeper layer,
- the Agent MUST ask the explicit mode-choice question before continuing,
- the Agent MUST stop and wait for the user’s response before moving to another parent or a deeper layer.

Incremental writing is part of the generation workflow, not a final cleanup step.

### 18.2 Root Write Rule

The root must contain:
- `<plan-name>_draft.md`
- entries corresponding to all first-layer nodes,
- first-layer non-leaf nodes as directories,
- first-layer leaf nodes as markdown files directly under the root directory.

The draft file may be written as soon as the root directory name is determined.

### 18.3 Non-Leaf and Leaf Write Rule

For every non-leaf node, create:
- `<node-name>/`
- `<node-name>/<node-name>.md`

For every leaf node, create:

`<leaf-node-name>.md`

All `Child Nodes` links must point to the actual markdown files of child nodes and should use relative paths whenever possible.

If the user revises a previously generated node, the Agent must update the corresponding file rather than append a parallel version.

### 18.3.1 Revision Invalidation Rule

If a previously confirmed non-leaf node is revised in a way that changes its scope, boundaries, decomposition, or child contract after descendants in that subtree have already been confirmed, those descendants become stale.

Stale descendants must not be treated as confirmed until they are regenerated and explicitly re-confirmed.

The Agent must not silently preserve stale descendant content as if it still satisfied the revised parent node.

### 18.4 Invalid Behavior Examples

Invalid:
- proposing the first layer and then generating deeper layers without user confirmation,
- writing the full tree immediately after the initial draft intake,
- treating “the user asked for a plan” as permission to auto-generate all layers,
- entering Auto-Generation without explicit approval,
- entering Auto-Generation before asking the material clarification questions needed to resolve missing user-owned details that would change downstream structure or leaf contracts,
- continuing to the next breadth-first layer before the confirmed current layer has been written to disk,
- writing multiple sibling-parent expansions in a single manual round,
- silently choosing a programming language, framework, database, deployment target, or similar first-order decision in manual mode when that choice materially changes the child structure or leaf contracts,
- moving on after a write checkpoint without asking the required manual/auto/pause mode-choice question,
- finishing one parent’s expansion and then jumping to another parent without providing the subtree summary,
- marking a node as a leaf even though execution would still require asking the planner to choose among major options,
- using planner-shaped leaf titles such as `define-*`, `choose-*`, or `plan-*` when the actual deliverable and decision rule remain unspecified,
- using abstract outputs such as "a plan", "a document", or "a strategy" as leaf deliverables without concrete artifact structure or completion rules,
- collapsing multiple phases or independent deliverables into a single leaf merely to reduce context pressure,
- using Auto-Generation to stop expansion before the leaf readiness gate has actually been satisfied

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
- Context Control

After each layer, the Agent should at least check:
- whether the nodes in the layer serve the same parent scope,
- whether sibling node granularity is roughly consistent,
- whether there is obvious overlap or omission,
- whether some nodes should already become leaves,
- whether some leaves should actually continue expanding,
- whether any candidate leaf still carries unresolved planner-level decisions,
- whether any candidate leaf is still using planner-shaped naming or abstract deliverables,
- whether the current layer is stable enough to enter the next layer,
- whether the current reply expanded more than one frontier parent in manual mode,
- whether the required subtree summary has been provided for the just-completed parent,
- whether the required mode-choice checkpoint has been asked after the latest write,
- whether Auto-Generation was preceded by the necessary clarification questions about material missing details,
- whether the current response size is starting to overload context and should be split into smaller parent-scoped rounds.

Before final writing or each incremental write, the Agent should at least check:
- whether the root directory name is appropriate,
- whether `<plan-name>_draft.md` exists and is correct,
- whether every non-leaf is a directory with a same-named `.md`,
- whether every leaf is a standalone `.md` file,
- whether all parent-child links exist and are correct,
- whether there are naming conflicts, dangling nodes, or unreferenced nodes,
- whether node content matches the template for its node type,
- whether every leaf passes the leaf readiness gate,
- whether each leaf binds to concrete inputs and concrete outputs,
- whether each leaf has falsifiable acceptance criteria,
- whether any leaf still bundles multiple phases or independent workstreams,
- whether any auto-generated leaf was accepted without first passing the leaf lint gate
