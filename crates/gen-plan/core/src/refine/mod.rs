pub mod decision;
pub mod gate_execution;
pub mod gate_registration;
pub mod gate_targets;
pub mod impact;
pub mod rewrite;
pub mod runtime_state;
pub mod scope;
pub mod summary;

pub use decision::{
    ExpectedGateRevalidation, RefineAffectedScope, RefineAffectedTrackedNode, RefineCommentSource,
    RefineDecision, RefineDecisionChangeType, RefineDecisionConfirmation, RefineDecisionStatus,
    RefineRewriteAction, RefineRewriteActionKind, RefineRewriteLinkChangeKind,
};
pub use gate_execution::{
    RefineGateAttempt, RefineGateAttemptOutcome, RefineGateExecutionError,
    RefineGateExecutionReport, RefineGateExecutionStatus, RefineGateInvocationFailure,
    RefineGateKind, RefineGateProcessedCommentBlock, RefineGateRetryPolicy,
    RefineGateRevalidationContext, RunRefineGateRevalidationRequest, run_refine_gate_revalidation,
};
pub use gate_registration::{
    RefineFrontierRegistrationCandidate, RefineGatePreparationError, RefineGateTargetReason,
    RefineLeafRegistrationCandidate, RefineParentRegistrationCandidate,
    RegisterRefineGateTargetsRequest, RegisteredRefineFrontierTarget, RegisteredRefineGateTargets,
    RegisteredRefineLeafTarget, RegisteredRefineParentTarget, register_refine_gate_targets,
};
pub use gate_targets::{
    ExpectedFrontierGateTargetSummary, ExpectedGateTargetSummary, ExpectedLeafGateTargetSummary,
    RefineFrontierGateTarget, RefineGateTargetSelection, RefineLeafGateTarget,
    SelectRefineGateTargetsRequest, StaleGateApproval, select_refine_gate_targets,
};
pub use impact::{
    RefineImpactAnalysis, RefineImpactAnalysisRequest, RefineImpactCategory,
    RefineImpactMappingIssue, analyze_refine_comment_impact,
};
pub use rewrite::{
    RefineChangedFile, RefineChangedFileKind, RefineContextInvalidation, RefineLinkUpdateReport,
    RefineRewriteError, RefineRewriteRequest, RefineRewriteResult, RefineStaleMarkReport,
    RefineStaleNode, RefineStructuralChange, RefineStructuralChangeKind, RefineUnchangedNode,
    apply_refine_rewrite,
};
pub use runtime_state::{
    BuildRefineGateSelectionInputsRequest, RefineGateSelectionInputs,
    RefinePriorFrontierGateSummary, RefinePriorGateSummaries, RefinePriorLeafGateSummary,
    RefineRuntimeNodeSnapshot, RefineRuntimeNodeSummary, RefineRuntimeStateLoadError,
    RefineStaleGateClassification, RefineStaleResultHandoff, StaleGateTargetKind,
    build_refine_gate_selection_inputs,
};
pub use scope::{
    RefineLinkChange, RefineNodeCreation, RefineNodeCreationRemovalPlan, RefineNodeRemoval,
    RefineRewriteScope, RefineRewriteScopeConflict, RefineRewriteScopeError,
    RefineRewriteScopeRequest, RefineRewriteTarget, RefineStaleDescendant,
    plan_refine_rewrite_scope,
};
pub use summary::{RefineRewriteSummary, RefineStaleNodeSummaryEntry, RefineStaleNodeSummaryKind};
