mod support;

use std::fs;

use anyhow::Result;
use loopy_gen_plan::refine::{
    RefineChangedFile, RefineChangedFileKind, RefineContextInvalidation,
    RefineFrontierRegistrationCandidate, RefineGateRetryPolicy, RefineGateTargetReason,
    RefineLeafRegistrationCandidate, RefineParentRegistrationCandidate, RefinePriorGateSummaries,
    RefinePriorLeafGateSummary, RefineRewriteResult, RefineRuntimeNodeSnapshot,
    RefineRuntimeNodeSummary, RefineStaleGateClassification, RefineStaleResultHandoff,
    RefineStructuralChange, RefineStructuralChangeKind, RunRefineGateRevalidationRequest,
    SelectRefineGateTargetsRequest, StaleGateTargetKind, register_refine_gate_targets,
    run_refine_gate_revalidation, select_refine_gate_targets,
};
use loopy_gen_plan::runtime::comments::{
    CommentDiscoveryError, collect_plan_markdown_files, discover_plan_comments,
};
use loopy_gen_plan::{
    EnsureNodeIdRequest, EnsurePlanRequest, GateSummary, InspectNodeRequest, ListChildrenRequest,
    NodeKind, ReconcileParentChildLinksRequest, Runtime,
};

#[test]
fn refine_comment_discovery_is_deterministic_and_fail_closed() -> Result<()> {
    let workspace = support::workspace()?;
    let plan_root = workspace.path().join(".loopy/plans/demo");
    fs::create_dir_all(plan_root.join("b"))?;
    fs::write(plan_root.join("demo_draft.md"), "# draft\n")?;
    fs::write(
        plan_root.join("b/b.md"),
        "# B\n\nBEGIN_COMMENT\nsecond\nEND_COMMENT\n",
    )?;
    fs::write(
        plan_root.join("a.md"),
        "# A\n\nBEGIN_COMMENT\nfirst\nEND_COMMENT\n",
    )?;
    fs::write(plan_root.join("notes.txt"), "ignore\n")?;

    assert_eq!(
        collect_plan_markdown_files(&plan_root, "demo")?,
        vec!["a.md".to_owned(), "b/b.md".to_owned()]
    );
    let comments = discover_plan_comments(&plan_root, "demo")?;
    assert_eq!(comments[0].relative_path, "a.md");
    assert_eq!(comments[0].start_line, 3);
    assert_eq!(comments[0].end_line, 5);
    assert_eq!(comments[0].text, "first");

    fs::write(plan_root.join("a.md"), "END_COMMENT\n")?;
    let error = discover_plan_comments(&plan_root, "demo")
        .expect_err("orphan end must fail closed before returning comments");
    assert!(matches!(
        error,
        CommentDiscoveryError::MalformedStructure {
            relative_path,
            line: 1,
            ..
        } if relative_path == "a.md"
    ));
    Ok(())
}

#[test]
fn refine_gate_targets_select_expected_targets_without_running_gates() {
    let rewrite_result = RefineRewriteResult {
        changed_files: vec![
            RefineChangedFile {
                relative_path: "api/add-auth-tests.md".to_owned(),
                node_id: Some("leaf-1".to_owned()),
                change_kind: RefineChangedFileKind::TextUpdated,
            },
            RefineChangedFile {
                relative_path: "new-scope/new-scope.md".to_owned(),
                node_id: None,
                change_kind: RefineChangedFileKind::Created,
            },
        ],
        structural_changes: vec![RefineStructuralChange {
            parent_relative_path: "api/api.md".to_owned(),
            parent_node_id: Some("parent-1".to_owned()),
            change_kind: RefineStructuralChangeKind::ChangedChildSet,
            added_child_relative_paths: vec!["api/add-auth-tests.md".to_owned()],
            removed_child_relative_paths: vec![],
        }],
        stale_nodes: vec![],
        context_invalidations: vec![RefineContextInvalidation {
            relative_path: "api/add-auth-tests.md".to_owned(),
            node_id: Some("leaf-1".to_owned()),
            reason: "parent contract changed".to_owned(),
        }],
        unchanged_nodes: vec![],
        expected_gate_targets: vec![],
        unresolved_follow_ups: vec![],
        summary: Default::default(),
    };
    let selection = select_refine_gate_targets(SelectRefineGateTargetsRequest {
        plan_id: "plan-1".to_owned(),
        rewrite_result,
        runtime_snapshot: RefineRuntimeNodeSnapshot {
            nodes: vec![
                RefineRuntimeNodeSummary {
                    node_id: "leaf-1".to_owned(),
                    relative_path: "api/add-auth-tests.md".to_owned(),
                    node_kind: NodeKind::Leaf,
                    parent_node_id: Some("parent-1".to_owned()),
                    parent_relative_path: Some("api/api.md".to_owned()),
                    child_relative_paths: vec![],
                },
                RefineRuntimeNodeSummary {
                    node_id: "parent-1".to_owned(),
                    relative_path: "api/api.md".to_owned(),
                    node_kind: NodeKind::Parent,
                    parent_node_id: None,
                    parent_relative_path: None,
                    child_relative_paths: vec!["api/add-auth-tests.md".to_owned()],
                },
            ],
        },
        prior_gate_summaries: RefinePriorGateSummaries {
            leaf: vec![RefinePriorLeafGateSummary {
                node_id: "leaf-1".to_owned(),
                relative_path: "api/add-auth-tests.md".to_owned(),
                summary: GateSummary {
                    gate_run_id: "leaf-run-1".to_owned(),
                    reviewer_role_id: "codex_default".to_owned(),
                    summary: "old pass".to_owned(),
                },
            }],
            frontier: vec![],
        },
        stale_result_handoff: vec![RefineStaleResultHandoff {
            target_kind: StaleGateTargetKind::Leaf,
            node_id: Some("leaf-1".to_owned()),
            relative_path: "api/add-auth-tests.md".to_owned(),
            parent_node_id: None,
            parent_relative_path: Some("api/api.md".to_owned()),
            regenerated_child_relative_path: None,
            classification: RefineStaleGateClassification::Stale,
            invalidation_reason: "changed parent context".to_owned(),
        }],
    });

    assert_eq!(selection.leaf_targets.len(), 1);
    assert!(
        selection.leaf_targets[0]
            .reasons
            .contains(&RefineGateTargetReason::TextChanged)
    );
    assert!(
        selection.leaf_targets[0]
            .reasons
            .contains(&RefineGateTargetReason::ContextInvalidated)
    );
    assert!(
        selection.leaf_targets[0]
            .reasons
            .contains(&RefineGateTargetReason::StaleDescendant)
    );
    assert_eq!(selection.frontier_targets.len(), 2);
    assert!(selection.frontier_targets.iter().any(|target| {
        target.parent_relative_path == "new-scope/new-scope.md"
            && target.reasons.contains(&RefineGateTargetReason::NewParent)
    }));
    assert_eq!(selection.stale_leaf_approvals[0].gate_run_id, "leaf-run-1");
    let registration = selection.to_registration_request("plan-1".to_owned());
    assert_eq!(
        registration.leaf_candidates[0].relative_path,
        "api/add-auth-tests.md"
    );
}

#[test]
fn parent_contract_changes_select_descendant_leaf_revalidation_targets() {
    let rewrite_result = RefineRewriteResult {
        changed_files: vec![RefineChangedFile {
            relative_path: "api/api.md".to_owned(),
            node_id: Some("parent-1".to_owned()),
            change_kind: RefineChangedFileKind::TextUpdated,
        }],
        structural_changes: vec![],
        stale_nodes: vec![],
        context_invalidations: vec![],
        unchanged_nodes: vec![],
        expected_gate_targets: vec![],
        unresolved_follow_ups: vec![],
        summary: Default::default(),
    };

    let selection = select_refine_gate_targets(SelectRefineGateTargetsRequest {
        plan_id: "plan-1".to_owned(),
        rewrite_result,
        runtime_snapshot: RefineRuntimeNodeSnapshot {
            nodes: vec![
                RefineRuntimeNodeSummary {
                    node_id: "parent-1".to_owned(),
                    relative_path: "api/api.md".to_owned(),
                    node_kind: NodeKind::Parent,
                    parent_node_id: None,
                    parent_relative_path: None,
                    child_relative_paths: vec![
                        "api/add-auth-tests.md".to_owned(),
                        "api/auth/auth.md".to_owned(),
                    ],
                },
                RefineRuntimeNodeSummary {
                    node_id: "leaf-1".to_owned(),
                    relative_path: "api/add-auth-tests.md".to_owned(),
                    node_kind: NodeKind::Leaf,
                    parent_node_id: Some("parent-1".to_owned()),
                    parent_relative_path: Some("api/api.md".to_owned()),
                    child_relative_paths: vec![],
                },
                RefineRuntimeNodeSummary {
                    node_id: "nested-parent-1".to_owned(),
                    relative_path: "api/auth/auth.md".to_owned(),
                    node_kind: NodeKind::Parent,
                    parent_node_id: Some("parent-1".to_owned()),
                    parent_relative_path: Some("api/api.md".to_owned()),
                    child_relative_paths: vec!["api/auth/check.md".to_owned()],
                },
                RefineRuntimeNodeSummary {
                    node_id: "nested-leaf-1".to_owned(),
                    relative_path: "api/auth/check.md".to_owned(),
                    node_kind: NodeKind::Leaf,
                    parent_node_id: Some("nested-parent-1".to_owned()),
                    parent_relative_path: Some("api/auth/auth.md".to_owned()),
                    child_relative_paths: vec![],
                },
            ],
        },
        prior_gate_summaries: RefinePriorGateSummaries::default(),
        stale_result_handoff: vec![],
    });

    assert_eq!(selection.frontier_targets.len(), 1);
    assert_eq!(selection.leaf_targets.len(), 2);
    for relative_path in ["api/add-auth-tests.md", "api/auth/check.md"] {
        let target = selection
            .leaf_targets
            .iter()
            .find(|target| target.relative_path == relative_path)
            .expect("descendant leaf should be selected");
        assert!(
            target
                .reasons
                .contains(&RefineGateTargetReason::ParentContractChanged),
            "missing parent-contract reason for {relative_path}"
        );
    }
}

#[test]
fn link_only_parent_edits_do_not_revalidate_unrelated_descendant_leaf_targets() {
    let rewrite_result = RefineRewriteResult {
        changed_files: vec![RefineChangedFile {
            relative_path: "api/api.md".to_owned(),
            node_id: Some("parent-1".to_owned()),
            change_kind: RefineChangedFileKind::TextUpdated,
        }],
        structural_changes: vec![
            RefineStructuralChange {
                parent_relative_path: "api/api.md".to_owned(),
                parent_node_id: Some("parent-1".to_owned()),
                change_kind: RefineStructuralChangeKind::ChangedChildSet,
                added_child_relative_paths: vec!["api/new-child.md".to_owned()],
                removed_child_relative_paths: Vec::new(),
            },
            RefineStructuralChange {
                parent_relative_path: "api/api.md".to_owned(),
                parent_node_id: Some("parent-1".to_owned()),
                change_kind: RefineStructuralChangeKind::ParentContractChanged,
                added_child_relative_paths: vec!["api/new-child.md".to_owned()],
                removed_child_relative_paths: Vec::new(),
            },
        ],
        stale_nodes: vec![],
        context_invalidations: vec![],
        unchanged_nodes: vec![],
        expected_gate_targets: vec![],
        unresolved_follow_ups: vec![],
        summary: Default::default(),
    };

    let selection = select_refine_gate_targets(SelectRefineGateTargetsRequest {
        plan_id: "plan-1".to_owned(),
        rewrite_result,
        runtime_snapshot: RefineRuntimeNodeSnapshot {
            nodes: vec![
                RefineRuntimeNodeSummary {
                    node_id: "parent-1".to_owned(),
                    relative_path: "api/api.md".to_owned(),
                    node_kind: NodeKind::Parent,
                    parent_node_id: None,
                    parent_relative_path: None,
                    child_relative_paths: vec!["api/existing.md".to_owned()],
                },
                RefineRuntimeNodeSummary {
                    node_id: "leaf-1".to_owned(),
                    relative_path: "api/existing.md".to_owned(),
                    node_kind: NodeKind::Leaf,
                    parent_node_id: Some("parent-1".to_owned()),
                    parent_relative_path: Some("api/api.md".to_owned()),
                    child_relative_paths: vec![],
                },
            ],
        },
        prior_gate_summaries: RefinePriorGateSummaries::default(),
        stale_result_handoff: vec![],
    });

    assert!(selection.leaf_targets.is_empty());
    assert_eq!(selection.frontier_targets.len(), 1);
    assert_eq!(
        selection.frontier_targets[0].changed_child_relative_paths,
        vec!["api/new-child.md"]
    );
}

#[test]
fn added_existing_leaf_child_is_revalidated_before_frontier() {
    let rewrite_result = RefineRewriteResult {
        changed_files: vec![],
        structural_changes: vec![RefineStructuralChange {
            parent_relative_path: "api/api.md".to_owned(),
            parent_node_id: Some("parent-1".to_owned()),
            change_kind: RefineStructuralChangeKind::ChangedChildSet,
            added_child_relative_paths: vec!["api/attached.md".to_owned()],
            removed_child_relative_paths: Vec::new(),
        }],
        stale_nodes: vec![],
        context_invalidations: vec![],
        unchanged_nodes: vec![],
        expected_gate_targets: vec![],
        unresolved_follow_ups: vec![],
        summary: Default::default(),
    };

    let selection = select_refine_gate_targets(SelectRefineGateTargetsRequest {
        plan_id: "plan-1".to_owned(),
        rewrite_result,
        runtime_snapshot: RefineRuntimeNodeSnapshot {
            nodes: vec![
                RefineRuntimeNodeSummary {
                    node_id: "parent-1".to_owned(),
                    relative_path: "api/api.md".to_owned(),
                    node_kind: NodeKind::Parent,
                    parent_node_id: None,
                    parent_relative_path: None,
                    child_relative_paths: vec!["api/attached.md".to_owned()],
                },
                RefineRuntimeNodeSummary {
                    node_id: "leaf-1".to_owned(),
                    relative_path: "api/attached.md".to_owned(),
                    node_kind: NodeKind::Leaf,
                    parent_node_id: Some("parent-1".to_owned()),
                    parent_relative_path: Some("api/api.md".to_owned()),
                    child_relative_paths: vec![],
                },
            ],
        },
        prior_gate_summaries: RefinePriorGateSummaries::default(),
        stale_result_handoff: vec![],
    });

    let leaf = selection
        .leaf_targets
        .iter()
        .find(|target| target.relative_path == "api/attached.md")
        .expect("added existing leaf should be revalidated");
    assert_eq!(leaf.node_id.as_deref(), Some("leaf-1"));
    assert!(
        leaf.reasons
            .contains(&RefineGateTargetReason::ChangedChildSet)
    );
    assert_eq!(selection.frontier_targets.len(), 1);
}

#[test]
fn refine_gate_registration_prepares_targets_fail_closed() -> Result<()> {
    let workspace = support::workspace()?;
    let runtime = Runtime::new(workspace.path())?;
    let plan = runtime.ensure_plan(EnsurePlanRequest {
        plan_name: "demo".to_owned(),
        task_type: "coding-task".to_owned(),
        project_directory: workspace.path().to_path_buf(),
    })?;
    let plan_root = workspace.path().join(".loopy/plans/demo");
    fs::create_dir_all(plan_root.join("api/auth"))?;
    fs::write(plan_root.join("api/api.md"), "# API\n")?;
    fs::write(
        plan_root.join("api/auth/auth.md"),
        "# Auth\n\n## Child Nodes\n\n- [Check](./check.md)\n",
    )?;
    fs::write(plan_root.join("api/auth/check.md"), "# Check\n")?;

    let registered = register_refine_gate_targets(
        &runtime,
        loopy_gen_plan::refine::RegisterRefineGateTargetsRequest {
            plan_id: plan.plan_id.clone(),
            parent_candidates: vec![
                RefineParentRegistrationCandidate {
                    relative_path: "api/auth/auth.md".to_owned(),
                    parent_relative_path: Some("api/api.md".to_owned()),
                    reasons: vec![RefineGateTargetReason::NewParent],
                },
                RefineParentRegistrationCandidate {
                    relative_path: "api/api.md".to_owned(),
                    parent_relative_path: None,
                    reasons: vec![RefineGateTargetReason::NewParent],
                },
            ],
            leaf_candidates: vec![RefineLeafRegistrationCandidate {
                relative_path: "api/auth/check.md".to_owned(),
                parent_relative_path: Some("api/auth/auth.md".to_owned()),
                reasons: vec![RefineGateTargetReason::NewLeaf],
            }],
            frontier_candidates: vec![RefineFrontierRegistrationCandidate {
                parent_relative_path: "api/auth/auth.md".to_owned(),
                changed_child_relative_paths: vec!["api/auth/check.md".to_owned()],
                reasons: vec![RefineGateTargetReason::ChangedChildSet],
            }],
        },
    )
    .expect("out-of-order nested parents should register before leaves/frontiers");

    assert_eq!(registered.parent_targets.len(), 2);
    assert_eq!(registered.leaf_targets.len(), 1);
    assert_eq!(registered.frontier_targets.len(), 1);

    let error = register_refine_gate_targets(
        &runtime,
        loopy_gen_plan::refine::RegisterRefineGateTargetsRequest {
            plan_id: plan.plan_id,
            parent_candidates: vec![],
            leaf_candidates: vec![RefineLeafRegistrationCandidate {
                relative_path: "other/check.md".to_owned(),
                parent_relative_path: Some("other/other.md".to_owned()),
                reasons: vec![],
            }],
            frontier_candidates: vec![],
        },
    )
    .expect_err("empty reasons fail closed");
    assert!(matches!(
        error,
        loopy_gen_plan::refine::RefineGatePreparationError::EmptySelectionReasons { .. }
    ));
    Ok(())
}

#[test]
fn refine_gate_registration_reconciles_removed_child_links_before_frontier() -> Result<()> {
    let workspace = support::workspace()?;
    let runtime = Runtime::new(workspace.path())?;
    let plan = runtime.ensure_plan(EnsurePlanRequest {
        plan_name: "demo".to_owned(),
        task_type: "coding-task".to_owned(),
        project_directory: workspace.path().to_path_buf(),
    })?;
    let plan_root = workspace.path().join(".loopy/plans/demo");
    fs::create_dir_all(plan_root.join("api"))?;
    fs::write(
        plan_root.join("api/api.md"),
        "# API\n\n## Child Nodes\n\n- [Removed](./removed.md)\n",
    )?;
    fs::write(plan_root.join("api/removed.md"), "# Removed\n")?;

    let parent = runtime.ensure_node_id(EnsureNodeIdRequest {
        plan_id: plan.plan_id.clone(),
        relative_path: "api/api.md".to_owned(),
        parent_relative_path: None,
    })?;
    let child = runtime.ensure_node_id(EnsureNodeIdRequest {
        plan_id: plan.plan_id.clone(),
        relative_path: "api/removed.md".to_owned(),
        parent_relative_path: Some("api/api.md".to_owned()),
    })?;

    fs::write(plan_root.join("api/api.md"), "# API\n\n## Child Nodes\n\n")?;

    let registered = register_refine_gate_targets(
        &runtime,
        loopy_gen_plan::refine::RegisterRefineGateTargetsRequest {
            plan_id: plan.plan_id.clone(),
            parent_candidates: vec![],
            leaf_candidates: vec![],
            frontier_candidates: vec![RefineFrontierRegistrationCandidate {
                parent_relative_path: "api/api.md".to_owned(),
                changed_child_relative_paths: vec!["api/removed.md".to_owned()],
                reasons: vec![RefineGateTargetReason::ChangedChildSet],
            }],
        },
    )
    .expect("removed child should not remain runtime-visible under the old parent");

    assert_eq!(registered.frontier_targets.len(), 1);
    let children = runtime.list_children(ListChildrenRequest {
        plan_id: plan.plan_id.clone(),
        parent_node_id: Some(parent.node_id),
        parent_relative_path: None,
    })?;
    assert!(children.children.is_empty());
    let child = runtime.inspect_node(InspectNodeRequest {
        plan_id: plan.plan_id,
        node_id: Some(child.node_id),
        relative_path: None,
    })?;
    assert_eq!(child.parent_node_id, None);
    assert_eq!(child.parent_relative_path, None);
    Ok(())
}

#[test]
fn refine_runtime_state_builds_selection_inputs_from_public_runtime() -> Result<()> {
    let workspace = support::workspace()?;
    let runtime = Runtime::new(workspace.path())?;
    let plan = runtime.ensure_plan(EnsurePlanRequest {
        plan_name: "demo".to_owned(),
        task_type: "coding-task".to_owned(),
        project_directory: workspace.path().to_path_buf(),
    })?;
    let parent = runtime.ensure_node_id(EnsureNodeIdRequest {
        plan_id: plan.plan_id.clone(),
        relative_path: "api/api.md".to_owned(),
        parent_relative_path: None,
    })?;
    let leaf = runtime.ensure_node_id(EnsureNodeIdRequest {
        plan_id: plan.plan_id.clone(),
        relative_path: "api/add-auth-tests.md".to_owned(),
        parent_relative_path: Some("api/api.md".to_owned()),
    })?;

    let rewrite_result = RefineRewriteResult {
        changed_files: vec![
            RefineChangedFile {
                relative_path: "api/add-auth-tests.md".to_owned(),
                node_id: Some(leaf.node_id.clone()),
                change_kind: RefineChangedFileKind::TextUpdated,
            },
            RefineChangedFile {
                relative_path: "new-leaf.md".to_owned(),
                node_id: None,
                change_kind: RefineChangedFileKind::Created,
            },
        ],
        structural_changes: vec![RefineStructuralChange {
            parent_relative_path: "api/api.md".to_owned(),
            parent_node_id: Some(parent.node_id.clone()),
            change_kind: RefineStructuralChangeKind::ChangedChildSet,
            added_child_relative_paths: vec!["api/add-auth-tests.md".to_owned()],
            removed_child_relative_paths: vec![],
        }],
        stale_nodes: vec![],
        context_invalidations: vec![],
        unchanged_nodes: vec![],
        expected_gate_targets: vec![],
        unresolved_follow_ups: vec![],
        summary: Default::default(),
    };

    let inputs = loopy_gen_plan::refine::build_refine_gate_selection_inputs(
        &runtime,
        loopy_gen_plan::refine::BuildRefineGateSelectionInputsRequest {
            plan_id: plan.plan_id,
            rewrite_result,
            stale_result_handoff: vec![],
        },
    )?;

    assert!(
        inputs
            .runtime_snapshot
            .nodes
            .iter()
            .any(|node| node.node_id == leaf.node_id
                && node.relative_path == "api/add-auth-tests.md")
    );
    assert!(
        inputs
            .runtime_snapshot
            .nodes
            .iter()
            .any(|node| node.node_id == parent.node_id
                && node.child_relative_paths == vec!["api/add-auth-tests.md"])
    );
    assert!(
        !inputs
            .runtime_snapshot
            .nodes
            .iter()
            .any(|node| node.relative_path == "new-leaf.md")
    );
    Ok(())
}

#[test]
fn refine_runtime_state_loads_descendants_for_parent_only_revalidation() -> Result<()> {
    let workspace = support::workspace()?;
    let runtime = Runtime::new(workspace.path())?;
    let plan = runtime.ensure_plan(EnsurePlanRequest {
        plan_name: "demo".to_owned(),
        task_type: "coding-task".to_owned(),
        project_directory: workspace.path().to_path_buf(),
    })?;
    let parent = runtime.ensure_node_id(EnsureNodeIdRequest {
        plan_id: plan.plan_id.clone(),
        relative_path: "api/api.md".to_owned(),
        parent_relative_path: None,
    })?;
    runtime.ensure_node_id(EnsureNodeIdRequest {
        plan_id: plan.plan_id.clone(),
        relative_path: "api/direct.md".to_owned(),
        parent_relative_path: Some("api/api.md".to_owned()),
    })?;
    let nested_parent = runtime.ensure_node_id(EnsureNodeIdRequest {
        plan_id: plan.plan_id.clone(),
        relative_path: "api/auth/auth.md".to_owned(),
        parent_relative_path: Some("api/api.md".to_owned()),
    })?;
    runtime.ensure_node_id(EnsureNodeIdRequest {
        plan_id: plan.plan_id.clone(),
        relative_path: "api/auth/check.md".to_owned(),
        parent_relative_path: Some("api/auth/auth.md".to_owned()),
    })?;

    let rewrite_result = RefineRewriteResult {
        changed_files: vec![RefineChangedFile {
            relative_path: "api/api.md".to_owned(),
            node_id: Some(parent.node_id),
            change_kind: RefineChangedFileKind::TextUpdated,
        }],
        structural_changes: vec![],
        stale_nodes: vec![],
        context_invalidations: vec![],
        unchanged_nodes: vec![],
        expected_gate_targets: vec![],
        unresolved_follow_ups: vec![],
        summary: Default::default(),
    };

    let inputs = loopy_gen_plan::refine::build_refine_gate_selection_inputs(
        &runtime,
        loopy_gen_plan::refine::BuildRefineGateSelectionInputsRequest {
            plan_id: plan.plan_id.clone(),
            rewrite_result,
            stale_result_handoff: vec![],
        },
    )?;

    assert!(inputs.runtime_snapshot.nodes.iter().any(|node| {
        node.relative_path == "api/auth/auth.md" && node.node_id == nested_parent.node_id
    }));

    let selection = select_refine_gate_targets(SelectRefineGateTargetsRequest {
        plan_id: plan.plan_id,
        rewrite_result: inputs.rewrite_result,
        runtime_snapshot: inputs.runtime_snapshot,
        prior_gate_summaries: inputs.prior_gate_summaries,
        stale_result_handoff: inputs.stale_result_handoff,
    });

    for relative_path in ["api/direct.md", "api/auth/check.md"] {
        assert!(
            selection
                .leaf_targets
                .iter()
                .any(|target| target.relative_path == relative_path),
            "missing descendant leaf target {relative_path}"
        );
    }
    Ok(())
}

#[test]
fn reconcile_parent_child_links_validates_before_detaching_children() -> Result<()> {
    let workspace = support::workspace()?;
    let runtime = Runtime::new(workspace.path())?;
    let plan = runtime.ensure_plan(EnsurePlanRequest {
        plan_name: "demo".to_owned(),
        task_type: "coding-task".to_owned(),
        project_directory: workspace.path().to_path_buf(),
    })?;
    let plan_root = workspace.path().join(".loopy/plans/demo");
    fs::create_dir_all(plan_root.join("api"))?;
    fs::write(
        plan_root.join("api/api.md"),
        "# API\n\n## Child Nodes\n\n- [Old](./old.md)\n",
    )?;
    fs::write(plan_root.join("api/old.md"), "# Old\n")?;
    let parent = runtime.ensure_node_id(EnsureNodeIdRequest {
        plan_id: plan.plan_id.clone(),
        relative_path: "api/api.md".to_owned(),
        parent_relative_path: None,
    })?;
    let old_child = runtime.ensure_node_id(EnsureNodeIdRequest {
        plan_id: plan.plan_id.clone(),
        relative_path: "api/old.md".to_owned(),
        parent_relative_path: Some("api/api.md".to_owned()),
    })?;

    fs::write(
        plan_root.join("api/api.md"),
        "# API\n\n## Child Nodes\n\n- [Missing](./missing.md)\n",
    )?;

    let error = runtime
        .reconcile_parent_child_links(ReconcileParentChildLinksRequest {
            plan_id: plan.plan_id.clone(),
            parent_relative_path: "api/api.md".to_owned(),
        })
        .expect_err("missing linked child should reject reconciliation");
    assert!(
        format!("{error:#}").contains("missing.md"),
        "unexpected reconcile error: {error:#}"
    );

    let children = runtime.list_children(ListChildrenRequest {
        plan_id: plan.plan_id.clone(),
        parent_node_id: Some(parent.node_id),
        parent_relative_path: None,
    })?;
    assert_eq!(children.children.len(), 1);
    assert_eq!(children.children[0].node_id, old_child.node_id);
    assert_eq!(children.children[0].relative_path, "api/old.md");
    Ok(())
}

#[test]
fn reconcile_parent_child_links_rejects_untracked_linked_children_before_detaching() -> Result<()> {
    let workspace = support::workspace()?;
    let runtime = Runtime::new(workspace.path())?;
    let plan = runtime.ensure_plan(EnsurePlanRequest {
        plan_name: "demo".to_owned(),
        task_type: "coding-task".to_owned(),
        project_directory: workspace.path().to_path_buf(),
    })?;
    let plan_root = workspace.path().join(".loopy/plans/demo");
    fs::create_dir_all(plan_root.join("api"))?;
    fs::write(plan_root.join("api/old.md"), "# Old\n")?;
    fs::write(plan_root.join("api/untracked.md"), "# Untracked\n")?;
    fs::write(
        plan_root.join("api/api.md"),
        "# API\n\n## Child Nodes\n\n- [Untracked](./untracked.md)\n",
    )?;
    let parent = runtime.ensure_node_id(EnsureNodeIdRequest {
        plan_id: plan.plan_id.clone(),
        relative_path: "api/api.md".to_owned(),
        parent_relative_path: None,
    })?;
    let old_child = runtime.ensure_node_id(EnsureNodeIdRequest {
        plan_id: plan.plan_id.clone(),
        relative_path: "api/old.md".to_owned(),
        parent_relative_path: Some("api/api.md".to_owned()),
    })?;

    let error = runtime
        .reconcile_parent_child_links(ReconcileParentChildLinksRequest {
            plan_id: plan.plan_id.clone(),
            parent_relative_path: "api/api.md".to_owned(),
        })
        .expect_err("untracked linked child should reject reconciliation");
    assert!(
        format!("{error:#}").contains("untracked.md"),
        "unexpected reconcile error: {error:#}"
    );

    let children = runtime.list_children(ListChildrenRequest {
        plan_id: plan.plan_id.clone(),
        parent_node_id: Some(parent.node_id),
        parent_relative_path: None,
    })?;
    assert_eq!(children.children.len(), 1);
    assert_eq!(children.children[0].node_id, old_child.node_id);
    Ok(())
}

#[test]
fn refine_gate_execution_empty_selection_passes_without_running_gates() -> Result<()> {
    let workspace = support::workspace()?;
    let runtime = Runtime::new(workspace.path())?;
    let plan_root = workspace.path().join(".loopy/plans/demo");
    fs::create_dir_all(&plan_root)?;
    let report = run_refine_gate_revalidation(
        &runtime,
        RunRefineGateRevalidationRequest {
            plan_id: "plan-1".to_owned(),
            plan_root,
            planner_mode: loopy_gen_plan::PlannerMode::Auto,
            registered_targets: Default::default(),
            retry_policy: RefineGateRetryPolicy {
                max_invocation_retries: 1,
            },
        },
    )?;
    assert_eq!(
        report.status,
        loopy_gen_plan::refine::RefineGateExecutionStatus::Passed
    );
    Ok(())
}
