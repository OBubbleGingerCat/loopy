## Gen-Plan Domain Contract

A gen-plan plan is a tree of markdown nodes.
It represents decomposition from higher-level planning scopes down to execution-ready leaf nodes.

This is not a free-form planning note.
Each node has a structural role in the plan tree.
Your review must judge the target against gen-plan tree semantics, not generic document quality alone.

### Non-Leaf Node
A non-leaf node represents a bounded planning scope that still requires decomposition.
It explains what area of work it owns and why further decomposition is still needed.
A non-leaf node is not expected to be directly executable.

### Leaf Node
A leaf node is the final execution unit in the plan tree.
It is expected to be directly executable by a downstream executor.
A valid leaf node must not still contain planner work, unresolved decomposition work, or unresolved design choices that materially block execution.

### Frontier Parent
A frontier parent is a non-leaf node currently being expanded into its direct children.

### Parent-Scoped Expansion
A parent-scoped expansion is the current set of direct child nodes under one frontier parent.
Frontier review judges this child set as a decomposition of the parent.
It does not review the entire plan tree globally.

### What Makes A Node Stop At Leaf
A node may stop at leaf only if further decomposition would no longer materially clarify execution authority, execution boundary, execution order, or acceptance criteria.
If further decomposition is still needed to remove planner work or execution ambiguity, the node should not be accepted as a leaf.

### Review Non-Goals
You are not rewriting the plan.
You are not choosing product direction unless needed to identify a true user-owned decision.
You are not reviewing prose style for its own sake.
You are judging whether the target satisfies gen-plan structural semantics and gate-specific quality requirements.
