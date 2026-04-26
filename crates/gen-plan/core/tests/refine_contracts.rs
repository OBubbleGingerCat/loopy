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
use rusqlite::{Connection, params};

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
fn refine_gate_registration_preserves_new_nested_parent_ancestor_link() -> Result<()> {
    let workspace = support::workspace()?;
    let runtime = Runtime::new(workspace.path())?;
    let plan = runtime.ensure_plan(EnsurePlanRequest {
        plan_name: "demo".to_owned(),
        task_type: "coding-task".to_owned(),
        project_directory: workspace.path().to_path_buf(),
    })?;
    let plan_root = workspace.path().join(".loopy/plans/demo");
    fs::create_dir_all(plan_root.join("api/auth"))?;
    fs::write(
        plan_root.join("api/api.md"),
        "# API\n\n## Child Nodes\n\n- [Auth](./auth/auth.md)\n",
    )?;
    fs::write(plan_root.join("api/auth/auth.md"), "# Auth\n")?;
    runtime.ensure_node_id(EnsureNodeIdRequest {
        plan_id: plan.plan_id.clone(),
        relative_path: "api/api.md".to_owned(),
        parent_relative_path: None,
    })?;

    let selection = select_refine_gate_targets(SelectRefineGateTargetsRequest {
        plan_id: plan.plan_id.clone(),
        rewrite_result: RefineRewriteResult {
            changed_files: vec![RefineChangedFile {
                relative_path: "api/auth/auth.md".to_owned(),
                node_id: None,
                change_kind: RefineChangedFileKind::Created,
            }],
            structural_changes: vec![],
            stale_nodes: vec![],
            context_invalidations: vec![],
            unchanged_nodes: vec![],
            expected_gate_targets: vec![],
            unresolved_follow_ups: vec![],
            summary: Default::default(),
        },
        runtime_snapshot: RefineRuntimeNodeSnapshot {
            nodes: vec![RefineRuntimeNodeSummary {
                node_id: "api-parent".to_owned(),
                relative_path: "api/api.md".to_owned(),
                node_kind: NodeKind::Parent,
                parent_node_id: None,
                parent_relative_path: None,
                child_relative_paths: vec![],
            }],
        },
        prior_gate_summaries: RefinePriorGateSummaries::default(),
        stale_result_handoff: vec![],
    });
    let registration_request = selection.to_registration_request(plan.plan_id.clone());
    assert_eq!(registration_request.parent_candidates.len(), 1);
    assert_eq!(
        registration_request.parent_candidates[0]
            .parent_relative_path
            .as_deref(),
        Some("api/api.md")
    );

    let registered = register_refine_gate_targets(&runtime, registration_request)
        .expect("new nested parent should register under its tracked ancestor");
    assert_eq!(registered.parent_targets.len(), 1);
    assert_eq!(
        registered.parent_targets[0].parent_relative_path.as_deref(),
        Some("api/api.md")
    );
    let nested = runtime.inspect_node(InspectNodeRequest {
        plan_id: plan.plan_id,
        node_id: None,
        relative_path: Some("api/auth/auth.md".to_owned()),
    })?;
    assert_eq!(nested.parent_relative_path.as_deref(), Some("api/api.md"));
    Ok(())
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
        structural_changes: vec![RefineStructuralChange {
            parent_relative_path: "api/api.md".to_owned(),
            parent_node_id: Some("parent-1".to_owned()),
            change_kind: RefineStructuralChangeKind::ChangedChildSet,
            added_child_relative_paths: vec!["api/new-child.md".to_owned()],
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
fn mixed_parent_contract_and_link_edits_revalidate_existing_descendants() {
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

    let existing = selection
        .leaf_targets
        .iter()
        .find(|target| target.relative_path == "api/existing.md")
        .expect("mixed parent contract and link edits should revalidate existing descendants");
    assert!(
        existing
            .reasons
            .contains(&RefineGateTargetReason::ParentContractChanged)
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
fn moved_existing_leaf_uses_destination_parent_for_revalidation() {
    let rewrite_result = RefineRewriteResult {
        changed_files: vec![],
        structural_changes: vec![RefineStructuralChange {
            parent_relative_path: "api/new/new.md".to_owned(),
            parent_node_id: Some("new-parent".to_owned()),
            change_kind: RefineStructuralChangeKind::ChangedChildSet,
            added_child_relative_paths: vec!["api/moved.md".to_owned()],
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
                    node_id: "old-parent".to_owned(),
                    relative_path: "api/old/old.md".to_owned(),
                    node_kind: NodeKind::Parent,
                    parent_node_id: None,
                    parent_relative_path: None,
                    child_relative_paths: vec![],
                },
                RefineRuntimeNodeSummary {
                    node_id: "new-parent".to_owned(),
                    relative_path: "api/new/new.md".to_owned(),
                    node_kind: NodeKind::Parent,
                    parent_node_id: None,
                    parent_relative_path: None,
                    child_relative_paths: vec!["api/moved.md".to_owned()],
                },
                RefineRuntimeNodeSummary {
                    node_id: "leaf-1".to_owned(),
                    relative_path: "api/moved.md".to_owned(),
                    node_kind: NodeKind::Leaf,
                    parent_node_id: Some("old-parent".to_owned()),
                    parent_relative_path: Some("api/old/old.md".to_owned()),
                    child_relative_paths: vec![],
                },
            ],
        },
        prior_gate_summaries: RefinePriorGateSummaries::default(),
        stale_result_handoff: vec![],
    });

    let moved = selection
        .leaf_targets
        .iter()
        .find(|target| target.relative_path == "api/moved.md")
        .expect("moved leaf should be selected for destination-parent revalidation");
    assert_eq!(
        moved.parent_relative_path.as_deref(),
        Some("api/new/new.md")
    );
}

#[test]
fn attached_existing_parent_revalidates_descendant_leaves_before_frontier() {
    let rewrite_result = RefineRewriteResult {
        changed_files: vec![],
        structural_changes: vec![RefineStructuralChange {
            parent_relative_path: "api/api.md".to_owned(),
            parent_node_id: Some("parent-1".to_owned()),
            change_kind: RefineStructuralChangeKind::ChangedChildSet,
            added_child_relative_paths: vec!["api/auth/auth.md".to_owned()],
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
                    child_relative_paths: vec!["api/auth/auth.md".to_owned()],
                },
                RefineRuntimeNodeSummary {
                    node_id: "nested-parent".to_owned(),
                    relative_path: "api/auth/auth.md".to_owned(),
                    node_kind: NodeKind::Parent,
                    parent_node_id: Some("parent-1".to_owned()),
                    parent_relative_path: Some("api/api.md".to_owned()),
                    child_relative_paths: vec!["api/auth/check.md".to_owned()],
                },
                RefineRuntimeNodeSummary {
                    node_id: "leaf-1".to_owned(),
                    relative_path: "api/auth/check.md".to_owned(),
                    node_kind: NodeKind::Leaf,
                    parent_node_id: Some("nested-parent".to_owned()),
                    parent_relative_path: Some("api/auth/auth.md".to_owned()),
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
        .find(|target| target.relative_path == "api/auth/check.md")
        .expect("attached existing subtree descendant leaf should be revalidated");
    assert_eq!(leaf.node_id.as_deref(), Some("leaf-1"));
    assert!(
        leaf.reasons
            .contains(&RefineGateTargetReason::ChangedChildSet)
    );
    assert_eq!(selection.frontier_targets.len(), 1);
}

#[test]
fn root_plan_parent_contract_change_selects_frontier_and_descendant_leaves() {
    let rewrite_result = RefineRewriteResult {
        changed_files: vec![RefineChangedFile {
            relative_path: "demo.md".to_owned(),
            node_id: Some("root-1".to_owned()),
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
                    node_id: "root-1".to_owned(),
                    relative_path: "demo.md".to_owned(),
                    node_kind: NodeKind::Parent,
                    parent_node_id: None,
                    parent_relative_path: None,
                    child_relative_paths: vec!["intro.md".to_owned()],
                },
                RefineRuntimeNodeSummary {
                    node_id: "leaf-1".to_owned(),
                    relative_path: "intro.md".to_owned(),
                    node_kind: NodeKind::Leaf,
                    parent_node_id: Some("root-1".to_owned()),
                    parent_relative_path: Some("demo.md".to_owned()),
                    child_relative_paths: vec![],
                },
            ],
        },
        prior_gate_summaries: RefinePriorGateSummaries::default(),
        stale_result_handoff: vec![],
    });

    assert!(
        selection
            .leaf_targets
            .iter()
            .all(|target| target.relative_path != "demo.md")
    );
    let leaf = selection
        .leaf_targets
        .iter()
        .find(|target| target.relative_path == "intro.md")
        .expect("root contract change should revalidate descendant leaf");
    assert!(
        leaf.reasons
            .contains(&RefineGateTargetReason::ParentContractChanged)
    );
    let frontier = selection
        .frontier_targets
        .iter()
        .find(|target| target.parent_relative_path == "demo.md")
        .expect("root contract change should schedule frontier gate");
    assert_eq!(frontier.parent_node_id.as_deref(), Some("root-1"));
    assert!(
        frontier
            .reasons
            .contains(&RefineGateTargetReason::ParentContractChanged)
    );
}

#[test]
fn root_scope_new_leaf_targets_use_root_plan_parent() {
    let selection = select_refine_gate_targets(SelectRefineGateTargetsRequest {
        plan_id: "plan-1".to_owned(),
        rewrite_result: RefineRewriteResult {
            changed_files: vec![RefineChangedFile {
                relative_path: "demo/leaf.md".to_owned(),
                node_id: None,
                change_kind: RefineChangedFileKind::Created,
            }],
            structural_changes: vec![],
            stale_nodes: vec![],
            context_invalidations: vec![],
            unchanged_nodes: vec![],
            expected_gate_targets: vec![],
            unresolved_follow_ups: vec![],
            summary: Default::default(),
        },
        runtime_snapshot: RefineRuntimeNodeSnapshot {
            nodes: vec![RefineRuntimeNodeSummary {
                node_id: "root-1".to_owned(),
                relative_path: "demo.md".to_owned(),
                node_kind: NodeKind::Parent,
                parent_node_id: None,
                parent_relative_path: None,
                child_relative_paths: vec![],
            }],
        },
        prior_gate_summaries: RefinePriorGateSummaries::default(),
        stale_result_handoff: vec![],
    });

    let leaf = selection
        .leaf_targets
        .iter()
        .find(|target| target.relative_path == "demo/leaf.md")
        .expect("new root-scope leaf should be selected");
    assert_eq!(leaf.parent_relative_path.as_deref(), Some("demo.md"));
    let registration = selection.to_registration_request("plan-1".to_owned());
    assert_eq!(
        registration.leaf_candidates[0]
            .parent_relative_path
            .as_deref(),
        Some("demo.md")
    );
}

#[test]
fn root_scope_top_level_new_leaf_targets_use_root_plan_parent() {
    let selection = select_refine_gate_targets(SelectRefineGateTargetsRequest {
        plan_id: "plan-1".to_owned(),
        rewrite_result: RefineRewriteResult {
            changed_files: vec![RefineChangedFile {
                relative_path: "intro.md".to_owned(),
                node_id: None,
                change_kind: RefineChangedFileKind::Created,
            }],
            structural_changes: vec![],
            stale_nodes: vec![],
            context_invalidations: vec![],
            unchanged_nodes: vec![],
            expected_gate_targets: vec![],
            unresolved_follow_ups: vec![],
            summary: Default::default(),
        },
        runtime_snapshot: RefineRuntimeNodeSnapshot {
            nodes: vec![RefineRuntimeNodeSummary {
                node_id: "root-1".to_owned(),
                relative_path: "demo.md".to_owned(),
                node_kind: NodeKind::Parent,
                parent_node_id: None,
                parent_relative_path: None,
                child_relative_paths: vec![],
            }],
        },
        prior_gate_summaries: RefinePriorGateSummaries::default(),
        stale_result_handoff: vec![],
    });

    let leaf = selection
        .leaf_targets
        .iter()
        .find(|target| target.relative_path == "intro.md")
        .expect("new top-level root-scope leaf should be selected");
    assert_eq!(leaf.parent_relative_path.as_deref(), Some("demo.md"));
    let registration = selection.to_registration_request("plan-1".to_owned());
    assert_eq!(
        registration.leaf_candidates[0]
            .parent_relative_path
            .as_deref(),
        Some("demo.md")
    );
}

#[test]
fn root_scope_new_leaf_targets_use_untracked_root_plan_parent_from_structural_change() {
    let selection = select_refine_gate_targets(SelectRefineGateTargetsRequest {
        plan_id: "plan-1".to_owned(),
        rewrite_result: RefineRewriteResult {
            changed_files: vec![RefineChangedFile {
                relative_path: "demo/leaf.md".to_owned(),
                node_id: None,
                change_kind: RefineChangedFileKind::Created,
            }],
            structural_changes: vec![RefineStructuralChange {
                parent_relative_path: "demo.md".to_owned(),
                parent_node_id: None,
                change_kind: RefineStructuralChangeKind::ChangedChildSet,
                added_child_relative_paths: vec!["demo/leaf.md".to_owned()],
                removed_child_relative_paths: vec![],
            }],
            stale_nodes: vec![],
            context_invalidations: vec![],
            unchanged_nodes: vec![],
            expected_gate_targets: vec![],
            unresolved_follow_ups: vec![],
            summary: Default::default(),
        },
        runtime_snapshot: RefineRuntimeNodeSnapshot { nodes: vec![] },
        prior_gate_summaries: RefinePriorGateSummaries::default(),
        stale_result_handoff: vec![],
    });

    let leaf = selection
        .leaf_targets
        .iter()
        .find(|target| target.relative_path == "demo/leaf.md")
        .expect("new root-scope leaf should be selected");
    assert_eq!(leaf.parent_relative_path.as_deref(), Some("demo.md"));
}

#[test]
fn root_scope_new_parent_registration_uses_root_plan_parent() {
    let selection = select_refine_gate_targets(SelectRefineGateTargetsRequest {
        plan_id: "plan-1".to_owned(),
        rewrite_result: RefineRewriteResult {
            changed_files: vec![RefineChangedFile {
                relative_path: "demo/api/api.md".to_owned(),
                node_id: None,
                change_kind: RefineChangedFileKind::Created,
            }],
            structural_changes: vec![RefineStructuralChange {
                parent_relative_path: "demo.md".to_owned(),
                parent_node_id: Some("root-1".to_owned()),
                change_kind: RefineStructuralChangeKind::ChangedChildSet,
                added_child_relative_paths: vec!["demo/api/api.md".to_owned()],
                removed_child_relative_paths: vec![],
            }],
            stale_nodes: vec![],
            context_invalidations: vec![],
            unchanged_nodes: vec![],
            expected_gate_targets: vec![],
            unresolved_follow_ups: vec![],
            summary: Default::default(),
        },
        runtime_snapshot: RefineRuntimeNodeSnapshot {
            nodes: vec![RefineRuntimeNodeSummary {
                node_id: "root-1".to_owned(),
                relative_path: "demo.md".to_owned(),
                node_kind: NodeKind::Parent,
                parent_node_id: None,
                parent_relative_path: None,
                child_relative_paths: vec!["demo/api/api.md".to_owned()],
            }],
        },
        prior_gate_summaries: RefinePriorGateSummaries::default(),
        stale_result_handoff: vec![],
    });

    let registration = selection.to_registration_request("plan-1".to_owned());
    let parent = registration
        .parent_candidates
        .iter()
        .find(|candidate| candidate.relative_path == "demo/api/api.md")
        .expect("new root-scope parent should be registered");
    assert_eq!(parent.parent_relative_path.as_deref(), Some("demo.md"));
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
                removed_child_relative_paths: Vec::new(),
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
                removed_child_relative_paths: Vec::new(),
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
fn refine_gate_registration_reconciles_removed_only_frontier_children() -> Result<()> {
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
                changed_child_relative_paths: Vec::new(),
                removed_child_relative_paths: vec!["api/removed.md".to_owned()],
                reasons: vec![RefineGateTargetReason::ParentContractChanged],
            }],
        },
    )
    .expect("removed-only frontier paths should trigger reconciliation");

    assert_eq!(registered.frontier_targets.len(), 1);
    assert_eq!(
        registered.frontier_targets[0].changed_child_relative_paths,
        vec!["api/removed.md"]
    );
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
fn refine_gate_registration_validates_frontier_children_before_reconciling() -> Result<()> {
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
    let child = runtime.ensure_node_id(EnsureNodeIdRequest {
        plan_id: plan.plan_id.clone(),
        relative_path: "api/old.md".to_owned(),
        parent_relative_path: Some("api/api.md".to_owned()),
    })?;
    fs::write(plan_root.join("api/api.md"), "# API\n\n## Child Nodes\n\n")?;

    let error = register_refine_gate_targets(
        &runtime,
        loopy_gen_plan::refine::RegisterRefineGateTargetsRequest {
            plan_id: plan.plan_id.clone(),
            parent_candidates: vec![],
            leaf_candidates: vec![],
            frontier_candidates: vec![RefineFrontierRegistrationCandidate {
                parent_relative_path: "api/api.md".to_owned(),
                changed_child_relative_paths: vec!["api/typo.md".to_owned()],
                removed_child_relative_paths: Vec::new(),
                reasons: vec![RefineGateTargetReason::ChangedChildSet],
            }],
        },
    )
    .expect_err("unknown frontier child path should fail before reconciliation");
    assert!(matches!(
        error,
        loopy_gen_plan::refine::RefineGatePreparationError::IncoherentFrontierChildren { .. }
    ));

    let children = runtime.list_children(ListChildrenRequest {
        plan_id: plan.plan_id.clone(),
        parent_node_id: Some(parent.node_id),
        parent_relative_path: None,
    })?;
    assert_eq!(children.children.len(), 1);
    assert_eq!(children.children[0].node_id, child.node_id);
    Ok(())
}

#[test]
fn refine_gate_registration_reconciles_existing_leaf_attach_before_leaf_registration() -> Result<()>
{
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
        "# API\n\n## Child Nodes\n\n- [Attached](./attached.md)\n",
    )?;
    fs::write(plan_root.join("api/attached.md"), "# Attached\n")?;

    let new_parent = runtime.ensure_node_id(EnsureNodeIdRequest {
        plan_id: plan.plan_id.clone(),
        relative_path: "api/api.md".to_owned(),
        parent_relative_path: None,
    })?;
    let attached = runtime.ensure_node_id(EnsureNodeIdRequest {
        plan_id: plan.plan_id.clone(),
        relative_path: "api/attached.md".to_owned(),
        parent_relative_path: Some("api/api.md".to_owned()),
    })?;
    fs::write(plan_root.join("api/api.md"), "# API\n\n## Child Nodes\n\n")?;
    runtime.reconcile_parent_child_links(ReconcileParentChildLinksRequest {
        plan_id: plan.plan_id.clone(),
        parent_relative_path: "api/api.md".to_owned(),
    })?;
    fs::write(
        plan_root.join("api/api.md"),
        "# API\n\n## Child Nodes\n\n- [Attached](./attached.md)\n",
    )?;

    let registered = register_refine_gate_targets(
        &runtime,
        loopy_gen_plan::refine::RegisterRefineGateTargetsRequest {
            plan_id: plan.plan_id.clone(),
            parent_candidates: vec![],
            leaf_candidates: vec![RefineLeafRegistrationCandidate {
                relative_path: "api/attached.md".to_owned(),
                parent_relative_path: Some("api/api.md".to_owned()),
                reasons: vec![RefineGateTargetReason::ChangedChildSet],
            }],
            frontier_candidates: vec![RefineFrontierRegistrationCandidate {
                parent_relative_path: "api/api.md".to_owned(),
                changed_child_relative_paths: vec!["api/attached.md".to_owned()],
                removed_child_relative_paths: Vec::new(),
                reasons: vec![RefineGateTargetReason::ChangedChildSet],
            }],
        },
    )
    .expect("existing attached leaf should be reconciled before leaf re-registration");

    assert_eq!(registered.leaf_targets.len(), 1);
    assert_eq!(registered.leaf_targets[0].node_id, attached.node_id);
    let children = runtime.list_children(ListChildrenRequest {
        plan_id: plan.plan_id.clone(),
        parent_node_id: Some(new_parent.node_id),
        parent_relative_path: None,
    })?;
    assert_eq!(children.children.len(), 1);
    assert_eq!(children.children[0].relative_path, "api/attached.md");
    Ok(())
}

#[test]
fn parent_contract_frontier_registration_does_not_detach_existing_children() -> Result<()> {
    let workspace = support::workspace()?;
    let runtime = Runtime::new(workspace.path())?;
    let plan = runtime.ensure_plan(EnsurePlanRequest {
        plan_name: "demo".to_owned(),
        task_type: "coding-task".to_owned(),
        project_directory: workspace.path().to_path_buf(),
    })?;
    let plan_root = workspace.path().join(".loopy/plans/demo");
    fs::create_dir_all(plan_root.join("api"))?;
    fs::write(plan_root.join("api/child.md"), "# Child\n")?;
    fs::write(
        plan_root.join("api/api.md"),
        "# API\n\n## Child Nodes\n\n- [Child](./child.md)\n",
    )?;
    let parent = runtime.ensure_node_id(EnsureNodeIdRequest {
        plan_id: plan.plan_id.clone(),
        relative_path: "api/api.md".to_owned(),
        parent_relative_path: None,
    })?;
    let child = runtime.ensure_node_id(EnsureNodeIdRequest {
        plan_id: plan.plan_id.clone(),
        relative_path: "api/child.md".to_owned(),
        parent_relative_path: Some("api/api.md".to_owned()),
    })?;

    fs::write(plan_root.join("api/api.md"), "# API\n\nUpdated contract\n")?;

    let registered = register_refine_gate_targets(
        &runtime,
        loopy_gen_plan::refine::RegisterRefineGateTargetsRequest {
            plan_id: plan.plan_id.clone(),
            parent_candidates: vec![],
            leaf_candidates: vec![],
            frontier_candidates: vec![RefineFrontierRegistrationCandidate {
                parent_relative_path: "api/api.md".to_owned(),
                changed_child_relative_paths: vec![],
                removed_child_relative_paths: Vec::new(),
                reasons: vec![RefineGateTargetReason::ParentContractChanged],
            }],
        },
    )
    .expect("parent-only frontier target should not reconcile child links");

    assert_eq!(registered.frontier_targets.len(), 1);
    let children = runtime.list_children(ListChildrenRequest {
        plan_id: plan.plan_id.clone(),
        parent_node_id: Some(parent.node_id),
        parent_relative_path: None,
    })?;
    assert_eq!(children.children.len(), 1);
    assert_eq!(children.children[0].node_id, child.node_id);
    Ok(())
}

#[test]
fn refine_gate_registration_accepts_existing_root_parent_for_leaf_targets() -> Result<()> {
    let workspace = support::workspace()?;
    let runtime = Runtime::new(workspace.path())?;
    let plan = runtime.ensure_plan(EnsurePlanRequest {
        plan_name: "demo".to_owned(),
        task_type: "coding-task".to_owned(),
        project_directory: workspace.path().to_path_buf(),
    })?;
    let plan_root = workspace.path().join(".loopy/plans/demo");
    fs::create_dir_all(plan_root.join("demo"))?;
    fs::write(plan_root.join("demo.md"), "# Demo\n")?;
    fs::write(plan_root.join("demo/leaf.md"), "# Leaf\n")?;
    let connection = Connection::open(workspace.path().join(".loopy/loopy.db"))?;
    connection.execute(
        "INSERT INTO GEN_PLAN__nodes (
            plan_id, node_id, relative_path, node_name, node_kind, parent_node_id, created_at, updated_at
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, '', '')",
        params![plan.plan_id, "root-1", "demo.md", "demo", "parent", Option::<String>::None],
    )?;
    connection.execute(
        "INSERT INTO GEN_PLAN__nodes (
            plan_id, node_id, relative_path, node_name, node_kind, parent_node_id, created_at, updated_at
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, '', '')",
        params![plan.plan_id, "leaf-1", "demo/leaf.md", "leaf", "leaf", "root-1"],
    )?;

    let registered = register_refine_gate_targets(
        &runtime,
        loopy_gen_plan::refine::RegisterRefineGateTargetsRequest {
            plan_id: plan.plan_id,
            parent_candidates: vec![],
            leaf_candidates: vec![RefineLeafRegistrationCandidate {
                relative_path: "demo/leaf.md".to_owned(),
                parent_relative_path: Some("demo.md".to_owned()),
                reasons: vec![RefineGateTargetReason::TextChanged],
            }],
            frontier_candidates: vec![],
        },
    )
    .expect("existing root-parent leaf should register for refine gates");

    assert_eq!(registered.leaf_targets.len(), 1);
    assert_eq!(registered.leaf_targets[0].node_id, "leaf-1");
    assert_eq!(
        registered.leaf_targets[0].parent_relative_path.as_deref(),
        Some("demo.md")
    );
    Ok(())
}

#[test]
fn refine_gate_registration_accepts_existing_root_parent_for_frontier_targets() -> Result<()> {
    let workspace = support::workspace()?;
    let runtime = Runtime::new(workspace.path())?;
    let plan = runtime.ensure_plan(EnsurePlanRequest {
        plan_name: "demo".to_owned(),
        task_type: "coding-task".to_owned(),
        project_directory: workspace.path().to_path_buf(),
    })?;
    let plan_root = workspace.path().join(".loopy/plans/demo");
    fs::write(plan_root.join("demo.md"), "# Demo\n\nUpdated contract\n")?;
    let connection = Connection::open(workspace.path().join(".loopy/loopy.db"))?;
    connection.execute(
        "INSERT INTO GEN_PLAN__nodes (
            plan_id, node_id, relative_path, node_name, node_kind, parent_node_id, created_at, updated_at
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, '', '')",
        params![plan.plan_id, "root-1", "demo.md", "demo", "parent", Option::<String>::None],
    )?;

    let registered = register_refine_gate_targets(
        &runtime,
        loopy_gen_plan::refine::RegisterRefineGateTargetsRequest {
            plan_id: plan.plan_id,
            parent_candidates: vec![],
            leaf_candidates: vec![],
            frontier_candidates: vec![RefineFrontierRegistrationCandidate {
                parent_relative_path: "demo.md".to_owned(),
                changed_child_relative_paths: Vec::new(),
                removed_child_relative_paths: Vec::new(),
                reasons: vec![RefineGateTargetReason::ParentContractChanged],
            }],
        },
    )
    .expect("existing root parent should register for frontier revalidation");

    assert_eq!(registered.frontier_targets.len(), 1);
    assert_eq!(registered.frontier_targets[0].parent_node_id, "root-1");
    assert_eq!(
        registered.frontier_targets[0].parent_relative_path,
        "demo.md"
    );
    Ok(())
}

#[test]
fn refine_gate_registration_creates_root_parent_through_public_runtime() -> Result<()> {
    let workspace = support::workspace()?;
    let runtime = Runtime::new(workspace.path())?;
    let plan = runtime.ensure_plan(EnsurePlanRequest {
        plan_name: "demo".to_owned(),
        task_type: "coding-task".to_owned(),
        project_directory: workspace.path().to_path_buf(),
    })?;
    let plan_root = workspace.path().join(".loopy/plans/demo");
    fs::write(plan_root.join("demo.md"), "# Demo\n\nRoot contract.\n")?;

    let registered = register_refine_gate_targets(
        &runtime,
        loopy_gen_plan::refine::RegisterRefineGateTargetsRequest {
            plan_id: plan.plan_id.clone(),
            parent_candidates: vec![RefineParentRegistrationCandidate {
                relative_path: "demo.md".to_owned(),
                parent_relative_path: None,
                reasons: vec![RefineGateTargetReason::ParentContractChanged],
            }],
            leaf_candidates: vec![],
            frontier_candidates: vec![RefineFrontierRegistrationCandidate {
                parent_relative_path: "demo.md".to_owned(),
                changed_child_relative_paths: Vec::new(),
                removed_child_relative_paths: Vec::new(),
                reasons: vec![RefineGateTargetReason::ParentContractChanged],
            }],
        },
    )
    .expect("root parent should be created through public runtime registration");

    assert_eq!(registered.parent_targets.len(), 1);
    assert_eq!(registered.parent_targets[0].relative_path, "demo.md");
    assert_eq!(registered.parent_targets[0].parent_relative_path, None);
    assert_eq!(registered.frontier_targets.len(), 1);
    let root = runtime.inspect_node(InspectNodeRequest {
        plan_id: plan.plan_id,
        node_id: Some(registered.parent_targets[0].node_id.clone()),
        relative_path: None,
    })?;
    assert_eq!(root.node_kind, NodeKind::Parent);
    assert_eq!(root.parent_relative_path, None);
    Ok(())
}

#[test]
fn refine_gate_registration_validates_all_targets_before_parent_mutation() -> Result<()> {
    let workspace = support::workspace()?;
    let runtime = Runtime::new(workspace.path())?;
    let plan = runtime.ensure_plan(EnsurePlanRequest {
        plan_name: "demo".to_owned(),
        task_type: "coding-task".to_owned(),
        project_directory: workspace.path().to_path_buf(),
    })?;
    let plan_root = workspace.path().join(".loopy/plans/demo");
    fs::create_dir_all(plan_root.join("api/new"))?;
    fs::write(plan_root.join("api/api.md"), "# API\n")?;
    fs::write(plan_root.join("api/new/new.md"), "# New\n")?;
    runtime.ensure_node_id(EnsureNodeIdRequest {
        plan_id: plan.plan_id.clone(),
        relative_path: "api/api.md".to_owned(),
        parent_relative_path: None,
    })?;

    let error = register_refine_gate_targets(
        &runtime,
        loopy_gen_plan::refine::RegisterRefineGateTargetsRequest {
            plan_id: plan.plan_id.clone(),
            parent_candidates: vec![RefineParentRegistrationCandidate {
                relative_path: "api/new/new.md".to_owned(),
                parent_relative_path: Some("api/api.md".to_owned()),
                reasons: vec![RefineGateTargetReason::NewParent],
            }],
            leaf_candidates: vec![],
            frontier_candidates: vec![RefineFrontierRegistrationCandidate {
                parent_relative_path: "api/api.md".to_owned(),
                changed_child_relative_paths: vec!["api/typo.md".to_owned()],
                removed_child_relative_paths: Vec::new(),
                reasons: vec![RefineGateTargetReason::ChangedChildSet],
            }],
        },
    )
    .expect_err("invalid frontier child should fail before registering new parents");
    assert!(matches!(
        error,
        loopy_gen_plan::refine::RefineGatePreparationError::IncoherentFrontierChildren { .. }
    ));
    let missing_parent = runtime.inspect_node(InspectNodeRequest {
        plan_id: plan.plan_id,
        node_id: None,
        relative_path: Some("api/new/new.md".to_owned()),
    });
    assert!(missing_parent.is_err(), "new parent must not be persisted");
    Ok(())
}

#[test]
fn refine_gate_registration_rejects_non_root_top_level_parent_candidate() -> Result<()> {
    let workspace = support::workspace()?;
    let runtime = Runtime::new(workspace.path())?;
    let plan = runtime.ensure_plan(EnsurePlanRequest {
        plan_name: "demo".to_owned(),
        task_type: "coding-task".to_owned(),
        project_directory: workspace.path().to_path_buf(),
    })?;
    let plan_root = workspace.path().join(".loopy/plans/demo");
    fs::write(plan_root.join("overview.md"), "# Overview\n")?;

    let error = register_refine_gate_targets(
        &runtime,
        loopy_gen_plan::refine::RegisterRefineGateTargetsRequest {
            plan_id: plan.plan_id.clone(),
            parent_candidates: vec![RefineParentRegistrationCandidate {
                relative_path: "overview.md".to_owned(),
                parent_relative_path: None,
                reasons: vec![RefineGateTargetReason::NewParent],
            }],
            leaf_candidates: vec![],
            frontier_candidates: vec![],
        },
    )
    .expect_err("only the actual root plan markdown may be a top-level parent");
    assert!(matches!(
        error,
        loopy_gen_plan::refine::RefineGatePreparationError::InvalidCanonicalPath { .. }
    ));
    let missing_parent = runtime.inspect_node(InspectNodeRequest {
        plan_id: plan.plan_id,
        node_id: None,
        relative_path: Some("overview.md".to_owned()),
    });
    assert!(
        missing_parent.is_err(),
        "invalid parent must not be persisted"
    );
    Ok(())
}

#[test]
fn refine_gate_registration_validates_all_frontiers_before_reconciling_links() -> Result<()> {
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
    fs::write(plan_root.join("api/api.md"), "# API\n\n## Child Nodes\n\n")?;

    let error = register_refine_gate_targets(
        &runtime,
        loopy_gen_plan::refine::RegisterRefineGateTargetsRequest {
            plan_id: plan.plan_id.clone(),
            parent_candidates: vec![],
            leaf_candidates: vec![],
            frontier_candidates: vec![
                RefineFrontierRegistrationCandidate {
                    parent_relative_path: "api/api.md".to_owned(),
                    changed_child_relative_paths: vec!["api/old.md".to_owned()],
                    removed_child_relative_paths: Vec::new(),
                    reasons: vec![RefineGateTargetReason::ChangedChildSet],
                },
                RefineFrontierRegistrationCandidate {
                    parent_relative_path: "missing/missing.md".to_owned(),
                    changed_child_relative_paths: Vec::new(),
                    removed_child_relative_paths: Vec::new(),
                    reasons: vec![RefineGateTargetReason::ParentContractChanged],
                },
            ],
        },
    )
    .expect_err("invalid later frontier should fail before any reconciliation mutates state");
    assert!(matches!(
        error,
        loopy_gen_plan::refine::RefineGatePreparationError::MissingParentRegistration { .. }
    ));

    let children = runtime.list_children(ListChildrenRequest {
        plan_id: plan.plan_id,
        parent_node_id: Some(parent.node_id),
        parent_relative_path: None,
    })?;
    assert_eq!(children.children.len(), 1);
    assert_eq!(children.children[0].node_id, old_child.node_id);
    Ok(())
}

#[test]
fn refine_runtime_state_allows_untracked_root_parent_to_register_later() -> Result<()> {
    let workspace = support::workspace()?;
    let runtime = Runtime::new(workspace.path())?;
    let plan = runtime.ensure_plan(EnsurePlanRequest {
        plan_name: "demo".to_owned(),
        task_type: "coding-task".to_owned(),
        project_directory: workspace.path().to_path_buf(),
    })?;
    let plan_root = workspace.path().join(".loopy/plans/demo");
    fs::write(
        plan_root.join("demo.md"),
        "# Demo\n\nUpdated root contract.\n",
    )?;
    let rewrite_result = RefineRewriteResult {
        changed_files: vec![RefineChangedFile {
            relative_path: "demo.md".to_owned(),
            node_id: None,
            change_kind: RefineChangedFileKind::TextUpdated,
        }],
        structural_changes: vec![RefineStructuralChange {
            parent_relative_path: "demo.md".to_owned(),
            parent_node_id: None,
            change_kind: RefineStructuralChangeKind::ParentContractChanged,
            added_child_relative_paths: vec![],
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
            plan_id: plan.plan_id.clone(),
            rewrite_result,
            stale_result_handoff: vec![],
        },
    )?;
    assert!(
        inputs.runtime_snapshot.nodes.is_empty(),
        "untracked root parent should be left for registration"
    );

    let selection = select_refine_gate_targets(SelectRefineGateTargetsRequest {
        plan_id: plan.plan_id.clone(),
        rewrite_result: inputs.rewrite_result,
        runtime_snapshot: inputs.runtime_snapshot,
        prior_gate_summaries: inputs.prior_gate_summaries,
        stale_result_handoff: inputs.stale_result_handoff,
    });
    assert_eq!(selection.frontier_targets.len(), 1);
    assert_eq!(
        selection.frontier_targets[0].parent_relative_path,
        "demo.md"
    );
    assert_eq!(selection.frontier_targets[0].parent_node_id, None);

    let registered = register_refine_gate_targets(
        &runtime,
        selection.to_registration_request(plan.plan_id.clone()),
    )?;
    assert_eq!(registered.parent_targets.len(), 1);
    assert_eq!(registered.parent_targets[0].relative_path, "demo.md");
    let root = runtime.inspect_node(InspectNodeRequest {
        plan_id: plan.plan_id,
        node_id: Some(registered.parent_targets[0].node_id.clone()),
        relative_path: None,
    })?;
    assert_eq!(root.node_kind, NodeKind::Parent);

    Ok(())
}

#[test]
fn refine_gate_registration_allows_removed_untracked_child_link() -> Result<()> {
    let workspace = support::workspace()?;
    let runtime = Runtime::new(workspace.path())?;
    let plan = runtime.ensure_plan(EnsurePlanRequest {
        plan_name: "demo".to_owned(),
        task_type: "coding-task".to_owned(),
        project_directory: workspace.path().to_path_buf(),
    })?;
    let plan_root = workspace.path().join(".loopy/plans/demo");
    fs::create_dir_all(plan_root.join("api"))?;
    fs::write(plan_root.join("api/api.md"), "# API\n\n## Child Nodes\n\n")?;
    runtime.ensure_node_id(EnsureNodeIdRequest {
        plan_id: plan.plan_id.clone(),
        relative_path: "api/api.md".to_owned(),
        parent_relative_path: None,
    })?;

    let registered = register_refine_gate_targets(
        &runtime,
        loopy_gen_plan::refine::RegisterRefineGateTargetsRequest {
            plan_id: plan.plan_id,
            parent_candidates: vec![],
            leaf_candidates: vec![],
            frontier_candidates: vec![RefineFrontierRegistrationCandidate {
                parent_relative_path: "api/api.md".to_owned(),
                changed_child_relative_paths: vec!["api/missing.md".to_owned()],
                removed_child_relative_paths: vec!["api/missing.md".to_owned()],
                reasons: vec![RefineGateTargetReason::ChangedChildSet],
            }],
        },
    )
    .expect("removed untracked child paths should not block reconciliation");

    assert_eq!(registered.frontier_targets.len(), 1);
    assert!(
        registered.frontier_targets[0]
            .changed_child_relative_paths
            .is_empty(),
        "untracked removed children should not be dispatched as changed child targets"
    );

    Ok(())
}

#[test]
fn refine_gate_registration_rejects_changed_children_outside_frontier() -> Result<()> {
    let workspace = support::workspace()?;
    let runtime = Runtime::new(workspace.path())?;
    let plan = runtime.ensure_plan(EnsurePlanRequest {
        plan_name: "demo".to_owned(),
        task_type: "coding-task".to_owned(),
        project_directory: workspace.path().to_path_buf(),
    })?;
    let plan_root = workspace.path().join(".loopy/plans/demo");
    fs::create_dir_all(plan_root.join("backend"))?;
    fs::create_dir_all(plan_root.join("docs"))?;
    fs::write(
        plan_root.join("backend/backend.md"),
        "# Backend\n\n## Child Nodes\n\n- [Current](./current.md)\n",
    )?;
    fs::write(plan_root.join("backend/current.md"), "# Current\n")?;
    fs::write(
        plan_root.join("docs/docs.md"),
        "# Docs\n\n## Child Nodes\n\n- [Guide](./guide.md)\n",
    )?;
    fs::write(plan_root.join("docs/guide.md"), "# Guide\n")?;
    runtime.ensure_node_id(EnsureNodeIdRequest {
        plan_id: plan.plan_id.clone(),
        relative_path: "backend/backend.md".to_owned(),
        parent_relative_path: None,
    })?;
    runtime.ensure_node_id(EnsureNodeIdRequest {
        plan_id: plan.plan_id.clone(),
        relative_path: "backend/current.md".to_owned(),
        parent_relative_path: Some("backend/backend.md".to_owned()),
    })?;
    runtime.ensure_node_id(EnsureNodeIdRequest {
        plan_id: plan.plan_id.clone(),
        relative_path: "docs/docs.md".to_owned(),
        parent_relative_path: None,
    })?;
    runtime.ensure_node_id(EnsureNodeIdRequest {
        plan_id: plan.plan_id.clone(),
        relative_path: "docs/guide.md".to_owned(),
        parent_relative_path: Some("docs/docs.md".to_owned()),
    })?;

    let error = register_refine_gate_targets(
        &runtime,
        loopy_gen_plan::refine::RegisterRefineGateTargetsRequest {
            plan_id: plan.plan_id,
            parent_candidates: vec![],
            leaf_candidates: vec![],
            frontier_candidates: vec![RefineFrontierRegistrationCandidate {
                parent_relative_path: "backend/backend.md".to_owned(),
                changed_child_relative_paths: vec!["docs/guide.md".to_owned()],
                removed_child_relative_paths: Vec::new(),
                reasons: vec![RefineGateTargetReason::ChangedChildSet],
            }],
        },
    )
    .expect_err("changed child paths outside the reviewed frontier must fail closed");
    assert!(matches!(
        error,
        loopy_gen_plan::refine::RefineGatePreparationError::IncoherentFrontierChildren { .. }
    ));
    Ok(())
}

#[test]
fn refine_gate_registration_preflights_existing_leaf_parent_conflicts_before_mutation() -> Result<()>
{
    let workspace = support::workspace()?;
    let runtime = Runtime::new(workspace.path())?;
    let plan = runtime.ensure_plan(EnsurePlanRequest {
        plan_name: "demo".to_owned(),
        task_type: "coding-task".to_owned(),
        project_directory: workspace.path().to_path_buf(),
    })?;
    let plan_root = workspace.path().join(".loopy/plans/demo");
    fs::create_dir_all(plan_root.join("api/new"))?;
    fs::create_dir_all(plan_root.join("docs"))?;
    fs::write(plan_root.join("api/api.md"), "# API\n")?;
    fs::write(plan_root.join("api/new/new.md"), "# New\n")?;
    fs::write(plan_root.join("api/existing.md"), "# Existing\n")?;
    fs::write(plan_root.join("docs/docs.md"), "# Docs\n")?;
    runtime.ensure_node_id(EnsureNodeIdRequest {
        plan_id: plan.plan_id.clone(),
        relative_path: "api/api.md".to_owned(),
        parent_relative_path: None,
    })?;
    runtime.ensure_node_id(EnsureNodeIdRequest {
        plan_id: plan.plan_id.clone(),
        relative_path: "docs/docs.md".to_owned(),
        parent_relative_path: None,
    })?;
    runtime.ensure_node_id(EnsureNodeIdRequest {
        plan_id: plan.plan_id.clone(),
        relative_path: "api/existing.md".to_owned(),
        parent_relative_path: Some("api/api.md".to_owned()),
    })?;

    let error = register_refine_gate_targets(
        &runtime,
        loopy_gen_plan::refine::RegisterRefineGateTargetsRequest {
            plan_id: plan.plan_id.clone(),
            parent_candidates: vec![RefineParentRegistrationCandidate {
                relative_path: "api/new/new.md".to_owned(),
                parent_relative_path: Some("api/api.md".to_owned()),
                reasons: vec![RefineGateTargetReason::NewParent],
            }],
            leaf_candidates: vec![RefineLeafRegistrationCandidate {
                relative_path: "api/existing.md".to_owned(),
                parent_relative_path: Some("docs/docs.md".to_owned()),
                reasons: vec![RefineGateTargetReason::ContextInvalidated],
            }],
            frontier_candidates: vec![],
        },
    )
    .expect_err("existing leaf parent conflicts must fail before parent registration");
    assert!(matches!(
        error,
        loopy_gen_plan::refine::RefineGatePreparationError::RegistrationFailed { .. }
    ));
    let missing_parent = runtime.inspect_node(InspectNodeRequest {
        plan_id: plan.plan_id,
        node_id: None,
        relative_path: Some("api/new/new.md".to_owned()),
    });
    assert!(missing_parent.is_err(), "new parent must not be persisted");
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
fn refine_runtime_state_loads_structurally_added_tracked_leaf_for_revalidation() -> Result<()> {
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
        "# API\n\n## Child Nodes\n\n- [Attached](./attached.md)\n",
    )?;
    fs::write(plan_root.join("api/attached.md"), "# Attached\n")?;
    let parent = runtime.ensure_node_id(EnsureNodeIdRequest {
        plan_id: plan.plan_id.clone(),
        relative_path: "api/api.md".to_owned(),
        parent_relative_path: None,
    })?;
    let leaf = runtime.ensure_node_id(EnsureNodeIdRequest {
        plan_id: plan.plan_id.clone(),
        relative_path: "api/attached.md".to_owned(),
        parent_relative_path: Some("api/api.md".to_owned()),
    })?;
    let connection = Connection::open(workspace.path().join(".loopy/loopy.db"))?;
    connection.execute(
        "UPDATE GEN_PLAN__nodes
         SET parent_node_id = NULL
         WHERE plan_id = ?1 AND node_id = ?2",
        params![plan.plan_id, leaf.node_id],
    )?;

    let rewrite_result = RefineRewriteResult {
        changed_files: vec![],
        structural_changes: vec![RefineStructuralChange {
            parent_relative_path: "api/api.md".to_owned(),
            parent_node_id: Some(parent.node_id.clone()),
            change_kind: RefineStructuralChangeKind::ChangedChildSet,
            added_child_relative_paths: vec!["api/attached.md".to_owned()],
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
        inputs.runtime_snapshot.nodes.iter().any(|node| {
            node.node_id == leaf.node_id && node.relative_path == "api/attached.md"
        }),
        "structurally added tracked leaf should be loaded for target selection"
    );

    let selection = select_refine_gate_targets(SelectRefineGateTargetsRequest {
        plan_id: "plan-1".to_owned(),
        rewrite_result: inputs.rewrite_result,
        runtime_snapshot: inputs.runtime_snapshot,
        prior_gate_summaries: inputs.prior_gate_summaries,
        stale_result_handoff: inputs.stale_result_handoff,
    });
    let target = selection
        .leaf_targets
        .iter()
        .find(|target| target.relative_path == "api/attached.md")
        .expect("added tracked leaf should be revalidated");
    assert_eq!(target.node_id.as_deref(), Some(leaf.node_id.as_str()));
    assert!(
        target
            .reasons
            .contains(&RefineGateTargetReason::ChangedChildSet)
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
fn reconcile_parent_child_links_rejects_stealing_still_linked_child() -> Result<()> {
    let workspace = support::workspace()?;
    let runtime = Runtime::new(workspace.path())?;
    let plan = runtime.ensure_plan(EnsurePlanRequest {
        plan_name: "demo".to_owned(),
        task_type: "coding-task".to_owned(),
        project_directory: workspace.path().to_path_buf(),
    })?;
    let plan_root = workspace.path().join(".loopy/plans/demo");
    fs::create_dir_all(plan_root.join("api/auth"))?;
    fs::write(
        plan_root.join("api/api.md"),
        "# API\n\n## Child Nodes\n\n- [Check](./auth/check.md)\n",
    )?;
    fs::write(
        plan_root.join("api/auth/auth.md"),
        "# Auth\n\n## Child Nodes\n\n- [Check](./check.md)\n",
    )?;
    fs::write(plan_root.join("api/auth/check.md"), "# Check\n")?;
    let root_parent = runtime.ensure_node_id(EnsureNodeIdRequest {
        plan_id: plan.plan_id.clone(),
        relative_path: "api/api.md".to_owned(),
        parent_relative_path: None,
    })?;
    let nested_parent = runtime.ensure_node_id(EnsureNodeIdRequest {
        plan_id: plan.plan_id.clone(),
        relative_path: "api/auth/auth.md".to_owned(),
        parent_relative_path: Some("api/api.md".to_owned()),
    })?;
    let child = runtime.ensure_node_id(EnsureNodeIdRequest {
        plan_id: plan.plan_id.clone(),
        relative_path: "api/auth/check.md".to_owned(),
        parent_relative_path: Some("api/auth/auth.md".to_owned()),
    })?;
    let connection = Connection::open(workspace.path().join(".loopy/loopy.db"))?;
    connection.execute(
        "UPDATE GEN_PLAN__nodes
         SET parent_node_id = ?1
         WHERE plan_id = ?2 AND node_id = ?3",
        params![root_parent.node_id, plan.plan_id, child.node_id],
    )?;

    let error = runtime
        .reconcile_parent_child_links(ReconcileParentChildLinksRequest {
            plan_id: plan.plan_id.clone(),
            parent_relative_path: "api/auth/auth.md".to_owned(),
        })
        .expect_err("reconcile should reject stealing a still-linked child");
    assert!(
        format!("{error:#}").contains("still linked"),
        "unexpected reconcile error: {error:#}"
    );

    let child_after = runtime.inspect_node(InspectNodeRequest {
        plan_id: plan.plan_id.clone(),
        node_id: None,
        relative_path: Some("api/auth/check.md".to_owned()),
    })?;
    assert_eq!(
        child_after.parent_node_id.as_deref(),
        Some(root_parent.node_id.as_str())
    );
    assert_ne!(
        child_after.parent_node_id.as_deref(),
        Some(nested_parent.node_id.as_str())
    );
    Ok(())
}

#[test]
fn reconcile_parent_child_links_accepts_root_plan_parent() -> Result<()> {
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
        plan_root.join("demo.md"),
        "# Demo\n\n## Child Nodes\n\n- [Intro](./intro.md)\n- [API](./api/api.md)\n",
    )?;
    fs::write(plan_root.join("intro.md"), "# Intro\n")?;
    fs::write(plan_root.join("api/api.md"), "# API\n")?;
    let connection = Connection::open(workspace.path().join(".loopy/loopy.db"))?;
    connection.execute(
        "INSERT INTO GEN_PLAN__nodes (
            plan_id, node_id, relative_path, node_name, node_kind, parent_node_id, created_at, updated_at
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, '', '')",
        params![plan.plan_id, "root-1", "demo.md", "demo", "parent", Option::<String>::None],
    )?;
    connection.execute(
        "INSERT INTO GEN_PLAN__nodes (
            plan_id, node_id, relative_path, node_name, node_kind, parent_node_id, created_at, updated_at
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, '', '')",
        params![plan.plan_id, "leaf-1", "intro.md", "intro", "leaf", Option::<String>::None],
    )?;
    connection.execute(
        "INSERT INTO GEN_PLAN__nodes (
            plan_id, node_id, relative_path, node_name, node_kind, parent_node_id, created_at, updated_at
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, '', '')",
        params![plan.plan_id, "api-1", "api/api.md", "api", "parent", Option::<String>::None],
    )?;

    let reconciled = runtime.reconcile_parent_child_links(ReconcileParentChildLinksRequest {
        plan_id: plan.plan_id.clone(),
        parent_relative_path: "demo.md".to_owned(),
    })?;
    assert_eq!(reconciled.parent_node_id, "root-1");
    assert_eq!(
        reconciled.linked_child_relative_paths,
        vec!["intro.md", "api/api.md"]
    );
    assert_eq!(
        reconciled.attached_child_relative_paths,
        vec!["intro.md", "api/api.md"]
    );

    let children = runtime.list_children(ListChildrenRequest {
        plan_id: plan.plan_id,
        parent_node_id: Some("root-1".to_owned()),
        parent_relative_path: None,
    })?;
    let child_paths = children
        .children
        .iter()
        .map(|child| child.relative_path.as_str())
        .collect::<Vec<_>>();
    assert_eq!(child_paths, vec!["api/api.md", "intro.md"]);
    Ok(())
}

#[test]
fn refine_gate_execution_empty_selection_passes_without_running_gates() -> Result<()> {
    let workspace = support::workspace()?;
    let runtime = Runtime::new(workspace.path())?;
    let plan = runtime.ensure_plan(EnsurePlanRequest {
        plan_name: "demo".to_owned(),
        task_type: "coding-task".to_owned(),
        project_directory: workspace.path().to_path_buf(),
    })?;
    let plan_root = workspace.path().join(".loopy/plans/demo");
    let report = run_refine_gate_revalidation(
        &runtime,
        RunRefineGateRevalidationRequest {
            plan_id: plan.plan_id,
            plan_root,
            planner_mode: loopy_gen_plan::PlannerMode::Auto,
            registered_targets: Default::default(),
            retry_policy: RefineGateRetryPolicy {
                max_invocation_retries: 1,
            },
            refine_context: Default::default(),
        },
    )?;
    assert_eq!(
        report.status,
        loopy_gen_plan::refine::RefineGateExecutionStatus::Passed
    );
    Ok(())
}
