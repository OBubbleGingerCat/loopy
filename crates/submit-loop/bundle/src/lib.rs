// Role logic owns manifest loading, task-type resolution, and prompt/executor configuration assembly.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use loopy_common_bundle::{BundleDescriptor, read_descriptor};
use loopy_submit_loop_api::OpenLoopRequest;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const SKILL_ID: &str = "loopy:submit-loop";
pub const LOADER_ID: &str = "loopy.submit-loop.v1";

#[derive(Debug, Deserialize)]
pub struct Manifest {
    executors: HashMap<String, ExecutorProfile>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskTypeConfig {
    task_type: String,
    default_planning_worker: String,
    default_artifact_worker: String,
    default_checkpoint_reviewers: Vec<String>,
    default_artifact_reviewers: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolvedRoleSelection {
    pub task_type: String,
    pub planning_worker: String,
    pub artifact_worker: String,
    pub checkpoint_reviewers: Vec<String>,
    pub artifact_reviewers: Vec<String>,
}

#[derive(Debug)]
pub struct NormalizedOpenLoopInput {
    pub summary: String,
    pub task_type: String,
    pub context: String,
    pub constraints: Value,
    pub bypass_sandbox: bool,
    pub resolved_role_selection: ResolvedRoleSelection,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ExecutorProfile {
    pub kind: String,
    pub command: String,
    pub args: Vec<String>,
    #[serde(default)]
    pub bypass_sandbox_args: Option<Vec<String>>,
    #[serde(default)]
    pub bypass_sandbox_inherit_env: bool,
    pub cwd: String,
    pub timeout_sec: i64,
    pub transcript_capture: String,
    pub env_allow: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
pub struct RoleFrontMatter {
    pub role: String,
    pub executor: String,
}

pub fn load_bundle_descriptor(skill_root: &Path) -> Result<BundleDescriptor> {
    let descriptor = read_descriptor(skill_root)?;
    if descriptor.skill_id != SKILL_ID {
        bail!(
            "expected skill_id {} in {}, found {}",
            SKILL_ID,
            skill_root.join("bundle.toml").display(),
            descriptor.skill_id
        );
    }
    if descriptor.loader_id != LOADER_ID {
        bail!(
            "expected loader_id {} in {}, found {}",
            LOADER_ID,
            skill_root.join("bundle.toml").display(),
            descriptor.loader_id
        );
    }
    Ok(descriptor)
}

pub fn load_manifest(skill_root: &Path) -> Result<Manifest> {
    let descriptor = load_bundle_descriptor(skill_root)?;
    let manifest_path = skill_root.join(descriptor.internal_manifest);
    let manifest_text = fs::read_to_string(&manifest_path)
        .with_context(|| format!("failed to read {}", manifest_path.display()))?;
    Ok(toml::from_str(&manifest_text)
        .with_context(|| format!("failed to parse {}", manifest_path.display()))?)
}

fn role_path_for(skill_root: &Path, task_type: &str, role_kind: &str, role_id: &str) -> PathBuf {
    skill_root
        .join("roles")
        .join(task_type)
        .join(role_kind)
        .join(format!("{role_id}.md"))
}

pub fn load_task_type_role_definition(
    skill_root: &Path,
    manifest: &Manifest,
    task_type: &str,
    role_kind: &str,
    role_id: &str,
) -> Result<(PathBuf, String, RoleFrontMatter, ExecutorProfile)> {
    let role_path = role_path_for(skill_root, task_type, role_kind, role_id);
    let role_markdown = fs::read_to_string(&role_path)
        .with_context(|| format!("failed to read {}", role_path.display()))?;
    let role_front_matter = parse_role_front_matter(&role_markdown)?;
    let role_prompt_markdown = extract_role_body(&role_markdown)?;
    if role_front_matter.role != role_kind {
        bail!(
            "role kind mismatch for {}: expected {}, found {}",
            role_path.display(),
            role_kind,
            role_front_matter.role
        );
    }
    let executor_profile = manifest
        .executors
        .get(&role_front_matter.executor)
        .cloned()
        .ok_or_else(|| anyhow!("missing executor profile {}", role_front_matter.executor))?;
    Ok((
        role_path,
        role_prompt_markdown,
        role_front_matter,
        executor_profile,
    ))
}

fn load_task_type_config(skill_root: &Path, task_type: &str) -> Result<TaskTypeConfig> {
    let config_path = skill_root
        .join("roles")
        .join(task_type)
        .join("task-type.toml");
    if !config_path.is_file() {
        bail!(
            "unknown task_type {task_type}: missing {}",
            config_path.display()
        );
    }
    let config_text = fs::read_to_string(&config_path)
        .with_context(|| format!("failed to read {}", config_path.display()))?;
    let config: TaskTypeConfig = toml::from_str(&config_text)
        .with_context(|| format!("failed to parse {}", config_path.display()))?;
    if config.task_type.trim() != task_type {
        bail!(
            "task_type {} does not match {} in {}",
            task_type,
            config.task_type,
            config_path.display()
        );
    }
    Ok(config)
}

pub fn decode_persisted_resolved_role_selection(
    _skill_root: &Path,
    value: Value,
) -> Result<ResolvedRoleSelection> {
    Ok(serde_json::from_value::<ResolvedRoleSelection>(value)?)
}

fn required_str<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("missing string field {key}"))
}

fn normalize_non_blank(field_name: &str, value: String) -> Result<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        bail!("{field_name} must not be blank");
    }
    Ok(trimmed.to_owned())
}

fn normalize_role_id(field_name: &str, value: String) -> Result<String> {
    let normalized = normalize_non_blank(field_name, value)?;
    if normalized.contains('/') || normalized.contains('\\') {
        bail!("{field_name} must not contain path separators");
    }
    Ok(normalized)
}

fn normalize_reviewer_ids(field_name: &str, values: Vec<String>) -> Result<Vec<String>> {
    if values.is_empty() {
        bail!("{field_name} must not be empty");
    }
    let mut normalized = Vec::with_capacity(values.len());
    let mut seen = HashSet::with_capacity(values.len());
    for value in values {
        let role_id = normalize_role_id(field_name, value)?;
        if !seen.insert(role_id.clone()) {
            bail!("{field_name} contains duplicate reviewer id {}", role_id);
        }
        normalized.push(role_id);
    }
    Ok(normalized)
}

fn resolve_requested_worker_role_id(
    field_name: &str,
    requested_role_id: Option<String>,
    default_role_id: String,
) -> Result<String> {
    normalize_role_id(field_name, requested_role_id.unwrap_or(default_role_id))
        .with_context(|| format!("failed to resolve {field_name}"))
}

fn resolve_requested_reviewer_role_ids(
    field_name: &str,
    requested_role_ids: Option<Vec<String>>,
    default_role_ids: Vec<String>,
) -> Result<Vec<String>> {
    normalize_reviewer_ids(field_name, requested_role_ids.unwrap_or(default_role_ids))
}

fn validate_selected_role(
    skill_root: &Path,
    manifest: &Manifest,
    task_type: &str,
    role_kind: &str,
    role_id: &str,
) -> Result<()> {
    let role_path = role_path_for(skill_root, task_type, role_kind, role_id);
    if !role_path.is_file() {
        bail!(
            "missing role file for {} {} at {}",
            role_kind,
            role_id,
            role_path.display()
        );
    }
    let role_markdown = fs::read_to_string(&role_path)
        .with_context(|| format!("failed to read {}", role_path.display()))?;
    let front_matter = parse_role_front_matter(&role_markdown)
        .with_context(|| format!("failed to parse front matter in {}", role_path.display()))?;
    if front_matter.role != role_kind {
        bail!(
            "role kind mismatch for {}: expected {}, found {}",
            role_path.display(),
            role_kind,
            front_matter.role
        );
    }
    if !manifest.executors.contains_key(&front_matter.executor) {
        bail!(
            "unknown executor {} declared by {}",
            front_matter.executor,
            role_path.display()
        );
    }
    Ok(())
}

pub fn normalize_open_loop_input(
    skill_root: &Path,
    manifest: &Manifest,
    request: OpenLoopRequest,
) -> Result<NormalizedOpenLoopInput> {
    let summary = normalize_non_blank("summary", request.summary)?;
    let task_type = normalize_non_blank("task_type", request.task_type)?;
    let context = request.context.unwrap_or_default();
    let constraints = request.constraints.unwrap_or_else(|| json!({}));
    let bypass_sandbox = request.bypass_sandbox.unwrap_or(false);

    // Task-type defaults come from disk first; explicit request fields may override them after validation.
    let task_type_config = load_task_type_config(skill_root, &task_type)?;
    let planning_worker = resolve_requested_worker_role_id(
        "planning_worker",
        request.planning_worker,
        task_type_config.default_planning_worker,
    )?;
    let artifact_worker = resolve_requested_worker_role_id(
        "artifact_worker",
        request.artifact_worker,
        task_type_config.default_artifact_worker,
    )?;
    let checkpoint_reviewers = resolve_requested_reviewer_role_ids(
        "checkpoint_reviewers",
        request.checkpoint_reviewers,
        task_type_config.default_checkpoint_reviewers,
    )?;
    let artifact_reviewers = resolve_requested_reviewer_role_ids(
        "artifact_reviewers",
        request.artifact_reviewers,
        task_type_config.default_artifact_reviewers,
    )?;

    validate_selected_role(
        skill_root,
        manifest,
        &task_type,
        "planning_worker",
        &planning_worker,
    )?;
    validate_selected_role(
        skill_root,
        manifest,
        &task_type,
        "artifact_worker",
        &artifact_worker,
    )?;
    for role_id in &checkpoint_reviewers {
        validate_selected_role(
            skill_root,
            manifest,
            &task_type,
            "checkpoint_reviewer",
            role_id,
        )?;
    }
    for role_id in &artifact_reviewers {
        validate_selected_role(
            skill_root,
            manifest,
            &task_type,
            "artifact_reviewer",
            role_id,
        )?;
    }

    Ok(NormalizedOpenLoopInput {
        summary,
        task_type: task_type.clone(),
        context,
        constraints,
        bypass_sandbox,
        resolved_role_selection: ResolvedRoleSelection {
            task_type,
            planning_worker,
            artifact_worker,
            checkpoint_reviewers,
            artifact_reviewers,
        },
    })
}

pub fn build_loopy_api_contract(allowed_terminal_apis: &[String]) -> Value {
    json!({
        "coordinator_apis": [
            "open_loop",
            "prepare_worktree",
            "start_worker_invocation",
            "open_review_round",
            "start_reviewer_invocation",
            "handoff_to_caller_finalize",
            "begin_caller_finalize",
            "block_caller_finalize",
            "finalize_success",
            "finalize_failure",
            "rebuild_projections",
        ],
        "non_terminal_apis": [
            "request_timeout_extension",
        ],
        "terminal_apis": allowed_terminal_apis,
    })
}

pub fn build_dispatch_envelope(
    role_definition: &Value,
    invocation_context: &Value,
) -> Result<String> {
    let role_prompt_markdown = role_definition
        .get("prompt_markdown")
        .or_else(|| role_definition.get("prompt"))
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("role definition missing prompt markdown"))?;
    let runtime_prompt_markdown = build_runtime_prompt_markdown(invocation_context)?;
    let invocation_prompt_markdown =
        build_invocation_prompt_markdown(&runtime_prompt_markdown, role_prompt_markdown);
    let invocation_context = augment_invocation_context_with_contract_examples(invocation_context)?;
    Ok(serde_json::to_string_pretty(&json!({
        "invocation_prompt_markdown": invocation_prompt_markdown,
        "runtime_prompt_markdown": runtime_prompt_markdown,
        "role_prompt_markdown": role_prompt_markdown,
        "invocation_context": invocation_context,
    }))?)
}

fn build_invocation_prompt_markdown(
    runtime_prompt_markdown: &str,
    role_prompt_markdown: &str,
) -> String {
    let role_prompt_markdown = role_prompt_markdown.trim();
    if role_prompt_markdown.is_empty() {
        return runtime_prompt_markdown.to_owned();
    }
    format!(
        "{runtime_prompt_markdown}\n\n## Role-Specific Domain Guidance\n\n{role_prompt_markdown}"
    )
}

fn augment_invocation_context_with_contract_examples(invocation_context: &Value) -> Result<Value> {
    let actor_role = required_str(invocation_context, "actor_role")?;
    let examples = match actor_role {
        "worker" => worker_runtime_contract_examples(invocation_context)?,
        "reviewer" => reviewer_runtime_contract_examples(invocation_context)?,
        _ => return Ok(invocation_context.clone()),
    };
    let mut invocation_context = invocation_context.clone();
    let object = invocation_context
        .as_object_mut()
        .ok_or_else(|| anyhow!("invocation context must be an object"))?;
    object.insert("runtime_contract_examples".to_owned(), examples);
    Ok(invocation_context)
}

fn worker_runtime_contract_examples(invocation_context: &Value) -> Result<Value> {
    let stage = required_str(invocation_context, "stage")?;
    match stage {
        "planning" => Ok(json!({
            "review_history_latest_result": {
                "review_round_id": "review-1",
                "review_kind": "checkpoint",
                "round_status": "rejected",
                "target_type": "plan_revision",
                "target_ref": "plan-2",
                "target_metadata": {},
                "summary": "revise the plan",
                "blocking_issues": [{
                    "summary": "Define rollback criteria",
                    "rationale": "reviewers need an explicit rollback path",
                    "expected_revision": "add rollback criteria to the affected checkpoint",
                }],
                "nonblocking_issues": [{
                    "summary": "Prefer one validation command",
                    "rationale": "one canonical command reduces ambiguity",
                    "expected_revision": "document one canonical validation command",
                }],
            },
            "checkpoints_json": [{
                "title": "Implement feature X",
                "kind": "artifact",
                "deliverables": [{
                    "path": "src/foo.rs",
                    "type": "file",
                }],
                "acceptance": {
                    "verification_steps": ["cargo test foo::bar"],
                    "expected_outcomes": ["feature X works for case Y"],
                },
            }],
            "improvement_opportunities_json": [{
                "summary": "remove dead branch",
                "rationale": "the fallback is no longer reachable after this change",
                "suggested_follow_up": "delete the obsolete helper in a later cleanup",
            }],
            "worker_blocked": {
                "summary": "missing required secret",
                "rationale": "the checkpoint depends on credentials that are not present in the worktree",
                "why_unrecoverable": "this invocation cannot fetch or synthesize the missing secret",
            },
            "timeout_extension": {
                "requested_timeout_sec": 120,
                "progress_summary": "completed the repository scan and drafted the remaining steps",
                "rationale": "the current timeout is too short for the remaining implementation and verification work",
            },
        })),
        "artifact" => Ok(json!({
            "bound_checkpoint": {
                "checkpoint_id": "checkpoint-1",
                "sequence_index": 0,
                "title": "Implement feature X",
                "kind": "artifact",
                "deliverables": [{
                    "path": "src/foo.rs",
                    "type": "file",
                }],
                "acceptance": {
                    "verification_steps": ["cargo test foo::bar"],
                    "expected_outcomes": ["feature X works for case Y"],
                },
            },
            "review_history_latest_result": {
                "review_round_id": "review-2",
                "review_kind": "artifact",
                "round_status": "rejected",
                "target_type": "checkpoint_id",
                "target_ref": "checkpoint-1",
                "target_metadata": {},
                "summary": "revise the candidate",
                "blocking_issues": [{
                    "summary": "Preserve failure semantics",
                    "rationale": "the candidate changes failure behavior",
                    "expected_revision": "restore the original failure path",
                }],
                "nonblocking_issues": [{
                    "summary": "Tighten regression coverage",
                    "rationale": "focused tests make the fix easier to audit",
                    "expected_revision": "add a regression test for the failure path",
                }],
            },
            "change_summary_json": {
                "headline": "implement feature X",
                "files": ["src/foo.rs"],
            },
            "improvement_opportunities_json": [{
                "summary": "remove dead branch",
                "rationale": "the fallback is no longer reachable after this change",
                "suggested_follow_up": "delete the obsolete helper in a later cleanup",
            }],
            "worker_blocked": {
                "summary": "missing required secret",
                "rationale": "the checkpoint depends on credentials that are not present in the worktree",
                "why_unrecoverable": "this invocation cannot fetch or synthesize the missing secret",
            },
            "timeout_extension": {
                "requested_timeout_sec": 120,
                "progress_summary": "completed the repository scan and drafted the remaining steps",
                "rationale": "the current timeout is too short for the remaining implementation and verification work",
            },
        })),
        other => bail!("unsupported worker stage {other} in invocation context"),
    }
}

fn reviewer_runtime_contract_examples(invocation_context: &Value) -> Result<Value> {
    let review_kind = required_str(invocation_context, "review_kind")?;
    let review_issue = json!({
        "summary": "Define rollback criteria",
        "rationale": "reviewers need an explicit rollback path",
        "expected_revision": "add rollback criteria to the affected submission",
    });
    let examples = json!({
        "blocking_issues_json": [review_issue.clone()],
        "nonblocking_issues_json": [review_issue],
        "improvement_opportunities_json": [{
            "summary": "remove dead branch",
            "rationale": "the fallback is no longer reachable after this change",
            "suggested_follow_up": "delete the obsolete helper in a later cleanup",
        }],
        "review_blocked": {
            "summary": "required review target is unavailable",
            "rationale": "the invocation cannot inspect the referenced candidate commit",
            "why_unrecoverable": "this reviewer invocation cannot recover the missing review input",
        },
        "timeout_extension": {
            "requested_timeout_sec": 120,
            "progress_summary": "completed the repository scan and drafted the remaining review checks",
            "rationale": "the current timeout is too short for the remaining review and submission work",
        },
    });
    match review_kind {
        "checkpoint" | "artifact" => Ok(examples),
        other => bail!("unsupported review_kind {other} in invocation context"),
    }
}

fn build_runtime_prompt_markdown(invocation_context: &Value) -> Result<String> {
    // The runtime prompt constrains each executor to the single bound invocation contract.
    let actor_role = required_str(invocation_context, "actor_role")?;
    let allowed_terminal_apis = invocation_context
        .get("allowed_terminal_apis")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("invocation context missing allowed_terminal_apis"))?
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| anyhow!("allowed_terminal_apis entries must be strings"))
        })
        .collect::<Result<Vec<_>>>()?;
    let allowed_terminal_apis_json = serde_json::to_string(&allowed_terminal_apis)?;

    let header = match actor_role {
        "worker" => "# Loopy Runtime Worker Invocation",
        "reviewer" => "# Loopy Runtime Reviewer Invocation",
        other => bail!("unsupported actor_role {other} in invocation context"),
    };

    let mut lines = vec![
        header.to_owned(),
        String::new(),
        "You are a non-interactive loopy runtime invocation. You are not the user-facing assistant.".to_owned(),
        "Treat `invocation_context` as authoritative for loop identity, stage, target, paths, and allowed terminal APIs.".to_owned(),
        "Do not use any skill.".to_owned(),
        "Do not spawn subagents.".to_owned(),
        "Do not ask the user for clarification, approval, confirmation, or review.".to_owned(),
        "Do not wait for additional input.".to_owned(),
        "Do not call coordinator APIs or invent ad hoc loop actions.".to_owned(),
        "Do not end with prose-only commentary when a terminal API call is required.".to_owned(),
        format!("The only allowed terminal APIs for this invocation are: {allowed_terminal_apis_json}."),
        "Always call the bundled runtime at `invocation_context.bundle_bin`.".to_owned(),
        "Always pass `--invocation-context-path` using `invocation_context.invocation_context_path`.".to_owned(),
    ];
    lines.push(String::new());
    lines.push("## Exact Terminal API CLI Forms".to_owned());
    lines.push(
        "When you call a terminal API, use one of these exact CLI forms. Do not rename flags, reorder required flags, or invent extra required arguments."
            .to_owned(),
    );
    for cli_form in terminal_api_cli_forms(actor_role, invocation_context)? {
        lines.push(format!("- `{cli_form}`"));
    }
    lines.push(String::new());
    lines.push("## Timeout Extension Signaling".to_owned());
    lines.push(
        "If you are making progress but the current timeout looks under-provisioned, write an advisory timeout request without consuming your terminal submission token."
            .to_owned(),
    );
    lines.push(
        "- `invocation_context.bundle_bin request-timeout-extension --invocation-context-path invocation_context.invocation_context_path --requested-timeout-sec <seconds> --progress-summary <summary> --rationale <rationale>`"
            .to_owned(),
    );
    if actor_role == "worker" {
        lines.push(
            "Use `invocation_context.runtime_contract_examples` in this envelope as the canonical machine-readable examples for worker-side payload shapes."
                .to_owned(),
        );
    } else if actor_role == "reviewer" {
        lines.push(
            "Use `invocation_context.runtime_contract_examples` in this envelope as the canonical machine-readable examples for reviewer-side payload shapes."
                .to_owned(),
        );
    }

    match actor_role {
        "worker" => {
            let stage = required_str(invocation_context, "stage")?;
            lines.push(String::new());
            lines.push("## Workflow".to_owned());
            match stage {
                "planning" => {
                    lines.push("This is a planning worker invocation. Plan the requested work; do not implement it in this invocation.".to_owned());
                    lines.push("Do not start broader brainstorming, spec-writing, or approval-gated workflows.".to_owned());
                    lines.push("Produce an ordered checkpoint plan that covers the requested work and its verification, or declare blocked if a valid plan cannot be produced.".to_owned());
                    lines.push("If the request does not describe a concrete repository task or you cannot identify any legitimate repository deliverable from the request and current worktree, call `declare-worker-blocked` instead of inventing work or exiting without a terminal API.".to_owned());
                    lines.push("A checkpoint is a durable deliverable boundary or state transition that may require its own candidate commit and review cycle.".to_owned());
                    lines.push("Each checkpoint must describe one artifact boundary using `title`, `kind`, `deliverables`, and `acceptance`.".to_owned());
                    lines.push("Use `kind = artifact` for normal coding tasks.".to_owned());
                    lines.push("Put produced files in `deliverables`.".to_owned());
                    lines.push("Put verification commands in `acceptance.verification_steps` and behavioral goals in `acceptance.expected_outcomes`.".to_owned());
                    lines.push("When acceptance must prove that only specific tracked files changed, write verification against `HEAD` or another stable baseline instead of relying only on worktree-versus-index diff checks.".to_owned());
                    lines.push("When the checkpoint limits which tracked files may change, make the allowed changed-file set match the full deliverable set for that checkpoint and do not omit newly created deliverables from the allowed diff paths.".to_owned());
                    lines.push("When the task requires append-only, exact-content, or exact line-count semantics for an existing file, make the verification prove the full post-change file equals the stable baseline plus the required edit instead of relying only on spot checks like `tail` or changed-file lists.".to_owned());
                    lines.push("When the task requires exact content for a new or fully replaced file, make the verification compare the full required bytes or text literally, including the intended trailing newline behavior, instead of accepting placeholder content such as only `\\n`.".to_owned());
                    lines.push("When artifact verification must compare the candidate against its pre-change basis, write the command so it still passes with the candidate commit checked out as `HEAD`; use `HEAD^` or another explicit stable pre-change reference instead of assuming `HEAD` is unchanged.".to_owned());
                    lines.push("Every verification step must be copy-paste executable as written. If you need to compare multi-line output such as changed-file lists, use Python or properly quoted expected strings instead of malformed shell placeholders or bare `$...` pseudo-literals.".to_owned());
                    lines.push("Treat routine verification as part of the checkpoint that produces the artifact, not as a separate checkpoint.".to_owned());
                    lines.push("Do not create a standalone checkpoint whose only purpose is syntax checking, test execution, local validation, or confirming expected behavior.".to_owned());
                    lines.push("Create a standalone verification checkpoint only when verification itself produces a separate deliverable, changes external state, or the user explicitly asks for a separate verification phase.".to_owned());
                    lines.push("A planning worker invocation must finish by calling exactly one allowed terminal API.".to_owned());
                    append_planning_worker_schema_contracts(&mut lines);
                }
                "artifact" => {
                    lines.push("This is an artifact worker invocation. Execute only the bound checkpoint and keep work scoped to that checkpoint.".to_owned());
                    lines.push("Treat `bound_checkpoint` in `invocation_context` as the authoritative artifact contract, including its deliverables and acceptance criteria.".to_owned());
                    lines.push("Make the minimal change needed, run the relevant `acceptance.verification_steps`, and satisfy the `acceptance.expected_outcomes` before submitting the candidate commit or declaring blocked.".to_owned());
                    lines.push("`acceptance.expected_outcomes` are binding requirements, not optional prose.".to_owned());
                    lines.push("When the checkpoint requires exact content, line-count, or formatting outcomes, inspect the full diff or file content against `HEAD` or another stable baseline instead of relying only on spot checks like `tail`.".to_owned());
                    lines.push("If `acceptance.verification_steps` and `acceptance.expected_outcomes` conflict or the verification would certify the wrong artifact, declare blocked instead of submitting.".to_owned());
                    lines.push("If direct file-edit helpers fail under the nested executor sandbox, fall back to shell-based repository edits in the bound worktree instead of retrying ad hoc private edit paths.".to_owned());
                    lines.push("Once the checkpoint contract is satisfied, stop exploring and transition immediately into submission.".to_owned());
                    lines.push("Use this exact finish sequence: run the checkpoint verification, stage the checkpoint deliverables, create the candidate commit in the current repository, resolve the candidate SHA with `git rev-parse HEAD`, then call `submit-candidate-commit`.".to_owned());
                    lines.push("Ignore executor-created untracked runtime directories when judging checkpoint scope; rely on the checkpoint deliverables and tracked-file changes.".to_owned());
                    lines.push("Create the candidate commit in the bound worktree's existing repository so reviewers can inspect the same commit SHA.".to_owned());
                    lines.push("Do not repoint `.git`, set an alternate `GIT_DIR`, or copy git metadata into `/tmp` to manufacture a private candidate commit.".to_owned());
                    lines.push("An artifact worker invocation must finish by calling exactly one allowed terminal API.".to_owned());
                    append_artifact_worker_schema_contracts(&mut lines);
                }
                other => bail!("unsupported worker stage {other} in invocation context"),
            }
            lines.push(
                "Inspect `invocation_context.review_history.latest_result` before submitting a terminal API."
                    .to_owned(),
            );
            lines.push(
                "If `invocation_context.review_history.latest_result` is not null, revise against that review result before resubmitting."
                    .to_owned(),
            );
            lines.push(
                "Address reviewer `blocking_issues` before resubmitting and incorporate `nonblocking_issues` when they materially improve the bound work without expanding scope."
                    .to_owned(),
            );
        }
        "reviewer" => {
            let review_kind = required_str(invocation_context, "review_kind")?;
            lines.push(String::new());
            lines.push("## Workflow".to_owned());
            lines.push("Review only the bound `review_target` in `invocation_context`; do not create a new plan or implementation.".to_owned());
            match review_kind {
                "checkpoint" => {
                    lines.push("This is a checkpoint reviewer invocation. Review the submitted plan revision, not the current implementation state of the worktree.".to_owned());
                    lines.push("Reject plans that split routine verification into standalone checkpoints without a separate deliverable, external state change, or explicit user request.".to_owned());
                    lines.push("Prefer plans where verification is attached to the artifact-producing checkpoint rather than appended as a trailing verification-only checkpoint.".to_owned());
                    lines.push("Reject plans whose checkpoints omit deliverables or acceptance metadata, or that use acceptance metadata only to restate a standalone verification-only checkpoint.".to_owned());
                    lines.push("Reject plans whose verification steps are malformed, not directly executable as written, or fail to check the contract they claim to prove.".to_owned());
                    lines.push("Reject plans when `acceptance.verification_steps` would certify an artifact that does not actually satisfy `acceptance.expected_outcomes`.".to_owned());
                    lines.push("Reject exact-content or newline-sensitive plans unless the verification literally checks the full required bytes or text, including the intended trailing newline behavior.".to_owned());
                    append_checkpoint_reviewer_schema_contracts(&mut lines);
                }
                "artifact" => {
                    lines.push("This is an artifact reviewer invocation. Review the bound checkpoint artifact and candidate commit only.".to_owned());
                    lines.push("Use the checkpoint deliverables and acceptance metadata in `review_target` as the authoritative review contract.".to_owned());
                    lines.push("`acceptance.expected_outcomes` are binding review criteria, not optional prose.".to_owned());
                    lines.push("Reject when the candidate's actual bytes, text, or formatting contradict exact-content or newline-sensitive expected outcomes.".to_owned());
                    lines.push("Reject when `acceptance.verification_steps` and `acceptance.expected_outcomes` do not prove the same artifact.".to_owned());
                    append_artifact_reviewer_schema_contracts(&mut lines);
                }
                other => bail!("unsupported review_kind {other} in invocation context"),
            }
            lines.push(
                "A reviewer invocation must finish by calling exactly one allowed terminal API."
                    .to_owned(),
            );
        }
        _ => unreachable!(),
    }

    Ok(lines.join("\n"))
}

fn append_planning_worker_schema_contracts(lines: &mut Vec<String>) {
    lines.push(String::new());
    lines.push("## Planning Worker Schema Contracts".to_owned());
    lines.push(
        "`invocation_context.review_history.latest_result` is either `null` or a revision-guidance object with `review_round_id`, `review_kind`, `round_status`, `target_type`, `target_ref`, `target_metadata`, `summary`, `blocking_issues`, and `nonblocking_issues`."
            .to_owned(),
    );
    lines.push(
        "Each `blocking_issues[]` or `nonblocking_issues[]` entry is a JSON object with non-empty `summary`, `rationale`, and `expected_revision`."
            .to_owned(),
    );
    lines.push(
        "- Minimal `invocation_context.review_history.latest_result` example: `{\"review_round_id\":\"review-1\",\"review_kind\":\"checkpoint\",\"round_status\":\"rejected\",\"target_type\":\"plan_revision\",\"target_ref\":\"plan-2\",\"target_metadata\":{},\"summary\":\"revise the plan\",\"blocking_issues\":[{\"summary\":\"Define rollback criteria\",\"rationale\":\"reviewers need an explicit rollback path\",\"expected_revision\":\"add rollback criteria to the affected checkpoint\"}],\"nonblocking_issues\":[{\"summary\":\"Prefer one validation command\",\"rationale\":\"one canonical command reduces ambiguity\",\"expected_revision\":\"document one canonical validation command\"}]}`"
            .to_owned(),
    );
    lines.push(
        "`--checkpoints-json` must be a JSON array of checkpoint objects. Each checkpoint object must contain non-empty `title`, `kind`, `deliverables`, and `acceptance` fields."
            .to_owned(),
    );
    lines.push(
        "For `--checkpoints-json`, `kind` currently must be `artifact`, `deliverables` must be a non-empty array, and each deliverable must be an object with non-empty `path` and `type`."
            .to_owned(),
    );
    lines.push(
        "In `--checkpoints-json`, `deliverables[].type` currently must be `file`; `type` currently must be `file`, so `type = file` is the only supported deliverable type."
            .to_owned(),
    );
    lines.push(
        "Each checkpoint `acceptance` object must include non-empty `verification_steps` and non-empty `expected_outcomes` arrays."
            .to_owned(),
    );
    lines.push(
        "- Minimal `--checkpoints-json` example: `[{\"title\":\"Implement feature X\",\"kind\":\"artifact\",\"deliverables\":[{\"path\":\"src/foo.rs\",\"type\":\"file\"}],\"acceptance\":{\"verification_steps\":[\"cargo test foo::bar\"],\"expected_outcomes\":[\"feature X works for case Y\"]}}]`"
            .to_owned(),
    );
    append_improvement_opportunity_contract(lines);
    append_worker_blocked_contract(lines);
    append_timeout_extension_contract(lines);
}

fn append_artifact_worker_schema_contracts(lines: &mut Vec<String>) {
    lines.push(String::new());
    lines.push("## Artifact Worker Schema Contracts".to_owned());
    lines.push(
        "`bound_checkpoint` is the authoritative checkpoint object. Expect `checkpoint_id`, `sequence_index`, `title`, `kind`, `deliverables`, and `acceptance`."
            .to_owned(),
    );
    lines.push(
        "`bound_checkpoint.deliverables` is an array of file deliverables shaped like `{\"path\":\"src/foo.rs\",\"type\":\"file\"}`. `bound_checkpoint.acceptance` contains `verification_steps` and `expected_outcomes` arrays."
            .to_owned(),
    );
    lines.push(
        "- Minimal `bound_checkpoint` example: `{\"checkpoint_id\":\"checkpoint-1\",\"sequence_index\":0,\"title\":\"Implement feature X\",\"kind\":\"artifact\",\"deliverables\":[{\"path\":\"src/foo.rs\",\"type\":\"file\"}],\"acceptance\":{\"verification_steps\":[\"cargo test foo::bar\"],\"expected_outcomes\":[\"feature X works for case Y\"]}}`"
            .to_owned(),
    );
    lines.push(
        "`invocation_context.review_history.latest_result` is either `null` or a revision-guidance object with `review_round_id`, `review_kind`, `round_status`, `target_type`, `target_ref`, `target_metadata`, `summary`, `blocking_issues`, and `nonblocking_issues`."
            .to_owned(),
    );
    lines.push(
        "Each `blocking_issues[]` or `nonblocking_issues[]` entry is a JSON object with non-empty `summary`, `rationale`, and `expected_revision`."
            .to_owned(),
    );
    lines.push(
        "- Minimal `invocation_context.review_history.latest_result` example: `{\"review_round_id\":\"review-2\",\"review_kind\":\"artifact\",\"round_status\":\"rejected\",\"target_type\":\"checkpoint_id\",\"target_ref\":\"checkpoint-1\",\"target_metadata\":{},\"summary\":\"revise the candidate\",\"blocking_issues\":[{\"summary\":\"Preserve failure semantics\",\"rationale\":\"the candidate changes failure behavior\",\"expected_revision\":\"restore the original failure path\"}],\"nonblocking_issues\":[{\"summary\":\"Tighten regression coverage\",\"rationale\":\"focused tests make the fix easier to audit\",\"expected_revision\":\"add a regression test for the failure path\"}]}`"
            .to_owned(),
    );
    lines.push(
        "change_summary_json must be a JSON object. Use the canonical object shape `{\"headline\":\"short change summary\",\"files\":[\"path/to/file\"]}` with a non-empty `headline` string and a `files` array of changed paths."
            .to_owned(),
    );
    lines.push(
        "- Minimal `change_summary_json` example: `{\"headline\":\"implement feature X\",\"files\":[\"src/foo.rs\"]}`"
            .to_owned(),
    );
    append_improvement_opportunity_contract(lines);
    append_worker_blocked_contract(lines);
    append_timeout_extension_contract(lines);
}

fn append_checkpoint_reviewer_schema_contracts(lines: &mut Vec<String>) {
    lines.push(String::new());
    lines.push("## Checkpoint Reviewer Schema Contracts".to_owned());
    lines.push(
        "`review_target` is the authoritative checkpoint review payload. Expect a JSON object with `type`, `ref`, `plan_revision`, and `checkpoints`, plus optional `notes` from the submitted plan revision."
            .to_owned(),
    );
    lines.push(
        "For checkpoint reviews, `review_target.type` is the submitted target type such as `plan_revision`, `review_target.ref` is the submitted target ref such as `plan-1`, and `review_target.checkpoints` is the submitted `--checkpoints-json` array for that revision."
            .to_owned(),
    );
    lines.push(
        "- Minimal `review_target` example: `{\"type\":\"plan_revision\",\"ref\":\"plan-1\",\"plan_revision\":1,\"checkpoints\":[{\"title\":\"Implement feature X\",\"kind\":\"artifact\",\"deliverables\":[{\"path\":\"src/foo.rs\",\"type\":\"file\"}],\"acceptance\":{\"verification_steps\":[\"cargo test foo::bar\"],\"expected_outcomes\":[\"feature X works for case Y\"]}}],\"notes\":\"reviewable plan\"}`"
            .to_owned(),
    );
    append_reviewer_submission_contracts(lines);
}

fn append_artifact_reviewer_schema_contracts(lines: &mut Vec<String>) {
    lines.push(String::new());
    lines.push("## Artifact Reviewer Schema Contracts".to_owned());
    lines.push(
        "`review_target` is the authoritative artifact review payload. Expect a JSON object with `type`, `ref`, `checkpoint_id`, `sequence_index`, `title`, `kind`, `deliverables`, `acceptance`, and `candidate_commit_sha`."
            .to_owned(),
    );
    lines.push(
        "For artifact reviews, `review_target.type` is `checkpoint_id`, `review_target.ref` repeats the checkpoint id, `deliverables` uses file deliverables shaped like `{\"path\":\"src/foo.rs\",\"type\":\"file\"}`, `acceptance` contains `verification_steps` and `expected_outcomes`, and `candidate_commit_sha` is the candidate commit under review."
            .to_owned(),
    );
    lines.push(
        "- Minimal `review_target` example: `{\"type\":\"checkpoint_id\",\"ref\":\"checkpoint-1\",\"checkpoint_id\":\"checkpoint-1\",\"sequence_index\":0,\"title\":\"Implement feature X\",\"kind\":\"artifact\",\"deliverables\":[{\"path\":\"src/foo.rs\",\"type\":\"file\"}],\"acceptance\":{\"verification_steps\":[\"cargo test foo::bar\"],\"expected_outcomes\":[\"feature X works for case Y\"]},\"candidate_commit_sha\":\"abc123def456abc123def456abc123def456abcd\"}`"
            .to_owned(),
    );
    append_reviewer_submission_contracts(lines);
}

fn append_reviewer_submission_contracts(lines: &mut Vec<String>) {
    lines.push("`decision` currently must be either `approve` or `reject`.".to_owned());
    lines.push(
        "Review submissions require a non-blank `summary` that concisely states the approval or rejection outcome."
            .to_owned(),
    );
    lines.push(
        "`--blocking-issues-json` and `--nonblocking-issues-json` must each be a JSON array of issue objects with non-empty `summary`, `rationale`, and `expected_revision`."
            .to_owned(),
    );
    lines.push(
        "- Minimal review issue entry: `{\"summary\":\"Define rollback criteria\",\"rationale\":\"reviewers need an explicit rollback path\",\"expected_revision\":\"add rollback criteria to the affected submission\"}`"
            .to_owned(),
    );
    append_improvement_opportunity_contract(lines);
    lines.push(
        "`approve` must not include any `blocking_issues`; approve reviews cannot include blocking_issues."
            .to_owned(),
    );
    append_review_blocked_contract(lines);
    append_timeout_extension_contract(lines);
}

fn append_improvement_opportunity_contract(lines: &mut Vec<String>) {
    lines.push(
        "`--improvement-opportunities-json` must be a JSON array of objects with non-empty `summary`, `rationale`, and `suggested_follow_up`."
            .to_owned(),
    );
    lines.push(
        "- Minimal `--improvement-opportunities-json` entry: `{\"summary\":\"remove dead branch\",\"rationale\":\"the fallback is no longer reachable after this change\",\"suggested_follow_up\":\"delete the obsolete helper in a later cleanup\"}`"
            .to_owned(),
    );
}

fn append_worker_blocked_contract(lines: &mut Vec<String>) {
    lines.push(
        "`declare-worker-blocked` requires non-empty `summary`, `rationale`, and `why_unrecoverable` values. Use it only when you cannot produce a valid terminal submission from the current invocation."
            .to_owned(),
    );
    lines.push(
        "- Minimal blocked payload: `{\"summary\":\"missing required secret\",\"rationale\":\"the checkpoint depends on credentials that are not present in the worktree\",\"why_unrecoverable\":\"this invocation cannot fetch or synthesize the missing secret\"}`"
            .to_owned(),
    );
}

fn append_review_blocked_contract(lines: &mut Vec<String>) {
    lines.push(
        "`declare-review-blocked` requires non-empty `summary`, `rationale`, and `why_unrecoverable` values. Use it only when you cannot complete a valid review decision from the current invocation."
            .to_owned(),
    );
    lines.push(
        "- Minimal blocked payload: `{\"summary\":\"required review target is unavailable\",\"rationale\":\"the invocation cannot inspect the referenced candidate commit\",\"why_unrecoverable\":\"this reviewer invocation cannot recover the missing review input\"}`"
            .to_owned(),
    );
}

fn append_timeout_extension_contract(lines: &mut Vec<String>) {
    lines.push(
        "`request-timeout-extension` is advisory. Request a larger timeout only when you have concrete progress evidence and still need more time to finish this same invocation."
            .to_owned(),
    );
    lines.push(
        "`requested_timeout_sec` must be greater than zero and strictly greater than the current timeout for this invocation."
            .to_owned(),
    );
    lines.push(
        "Timeout retries are only honored when `requested_timeout_sec` is greater than the current timeout and at most 4x the current timeout for this invocation."
            .to_owned(),
    );
    lines.push(
        "`progress_summary` and `rationale` must each be concrete, substantive, non-identical statements; each must contain at least five words and at least 24 non-whitespace characters."
            .to_owned(),
    );
    lines.push(
        "- Minimal timeout request example: `{\"requested_timeout_sec\":120,\"progress_summary\":\"completed the repository scan and drafted the remaining steps\",\"rationale\":\"the current timeout is too short for the remaining implementation and verification work\"}`"
            .to_owned(),
    );
}

fn terminal_api_cli_forms(actor_role: &str, invocation_context: &Value) -> Result<Vec<String>> {
    match actor_role {
        "worker" => {
            let stage = required_str(invocation_context, "stage")?;
            match stage {
                "planning" => Ok(vec![
                    "invocation_context.bundle_bin submit-checkpoint-plan --invocation-context-path invocation_context.invocation_context_path --submission-id <submission_id> --checkpoints-json <json_array> --improvement-opportunities-json <json_array> [--notes <notes>]".to_owned(),
                    "invocation_context.bundle_bin declare-worker-blocked --invocation-context-path invocation_context.invocation_context_path --submission-id <submission_id> --summary <summary> --rationale <rationale> --why-unrecoverable <why_unrecoverable>".to_owned(),
                ]),
                "artifact" => Ok(vec![
                    "invocation_context.bundle_bin submit-candidate-commit --invocation-context-path invocation_context.invocation_context_path --submission-id <submission_id> --candidate-commit-sha <candidate_commit_sha> --change-summary-json <json_object> --improvement-opportunities-json <json_array> [--notes <notes>]".to_owned(),
                    "invocation_context.bundle_bin declare-worker-blocked --invocation-context-path invocation_context.invocation_context_path --submission-id <submission_id> --summary <summary> --rationale <rationale> --why-unrecoverable <why_unrecoverable>".to_owned(),
                ]),
                other => bail!("unsupported worker stage {other} in invocation context"),
            }
        }
        "reviewer" => {
            let review_kind = required_str(invocation_context, "review_kind")?;
            match review_kind {
                "checkpoint" => Ok(vec![
                    "invocation_context.bundle_bin submit-checkpoint-review --invocation-context-path invocation_context.invocation_context_path --submission-id <submission_id> --decision <approve|reject> --summary <summary> --blocking-issues-json <json_array> --nonblocking-issues-json <json_array> --improvement-opportunities-json <json_array> [--notes <notes>]".to_owned(),
                    "invocation_context.bundle_bin declare-review-blocked --invocation-context-path invocation_context.invocation_context_path --submission-id <submission_id> --summary <summary> --rationale <rationale> --why-unrecoverable <why_unrecoverable>".to_owned(),
                ]),
                "artifact" => Ok(vec![
                    "invocation_context.bundle_bin submit-artifact-review --invocation-context-path invocation_context.invocation_context_path --submission-id <submission_id> --decision <approve|reject> --summary <summary> --blocking-issues-json <json_array> --nonblocking-issues-json <json_array> --improvement-opportunities-json <json_array> [--notes <notes>]".to_owned(),
                    "invocation_context.bundle_bin declare-review-blocked --invocation-context-path invocation_context.invocation_context_path --submission-id <submission_id> --summary <summary> --rationale <rationale> --why-unrecoverable <why_unrecoverable>".to_owned(),
                ]),
                other => bail!("unsupported review_kind {other} in invocation context"),
            }
        }
        other => bail!("unsupported actor_role {other} in invocation context"),
    }
}

fn parse_role_front_matter(markdown: &str) -> Result<RoleFrontMatter> {
    let (front_matter, _) = split_role_markdown(markdown)?;
    Ok(toml::from_str(front_matter).context("failed to parse role front matter")?)
}

fn extract_role_body(markdown: &str) -> Result<String> {
    let (_, body) = split_role_markdown(markdown)?;
    Ok(body.trim().to_owned())
}

fn split_role_markdown(markdown: &str) -> Result<(&str, &str)> {
    let mut sections = markdown.splitn(3, "---");
    let prefix = sections.next().unwrap_or_default();
    let front_matter = sections
        .next()
        .ok_or_else(|| anyhow!("missing role front matter"))?;
    let body = sections
        .next()
        .ok_or_else(|| anyhow!("missing role body after front matter"))?;
    if !prefix.trim().is_empty() {
        bail!("unexpected content before role front matter");
    }
    Ok((front_matter, body))
}

pub fn resolve_executor_command(
    executor_profile: &ExecutorProfile,
    bundle_bin: &Path,
    workspace_root: &Path,
    worktree_path: &str,
    invocation_context_path: &Path,
    bypass_sandbox: bool,
) -> Result<(Vec<String>, &'static str)> {
    // Executor templates stay declarative in the installed bundle until invocation-specific paths are known.
    let (args, args_variant) = if bypass_sandbox {
        (
            executor_profile
                .bypass_sandbox_args
                .as_ref()
                .ok_or_else(|| {
                    anyhow!(
                        "executor profile missing bypass_sandbox_args for bypass_sandbox launch"
                    )
                })?,
            "bypass_sandbox",
        )
    } else {
        (&executor_profile.args, "default")
    };
    let mut command = Vec::with_capacity(1 + args.len());
    command.push(resolve_template_value(
        &executor_profile.command,
        bundle_bin,
        workspace_root,
        worktree_path,
        invocation_context_path,
    )?);
    for arg in args {
        resolve_template_value(
            arg,
            bundle_bin,
            workspace_root,
            worktree_path,
            invocation_context_path,
        )
        .map(|resolved| command.push(resolved))?;
    }
    Ok((command, args_variant))
}

pub fn resolve_executor_env_policy(
    executor_profile: &ExecutorProfile,
    bypass_sandbox: bool,
) -> &'static str {
    if bypass_sandbox && executor_profile.bypass_sandbox_inherit_env {
        "inherit_all"
    } else {
        "allowlist"
    }
}

pub fn resolve_executor_cwd(cwd: &str, workspace_root: &Path, worktree_path: &str) -> String {
    match cwd {
        "worktree" => worktree_path.to_owned(),
        "workspace" => workspace_root.display().to_string(),
        other => other.to_owned(),
    }
}

fn resolve_template_value(
    value: &str,
    bundle_bin: &Path,
    workspace_root: &Path,
    worktree_path: &str,
    invocation_context_path: &Path,
) -> Result<String> {
    let codex_home = std::env::var("CODEX_HOME").ok().or_else(|| {
        std::env::var("HOME")
            .ok()
            .map(|home| Path::new(&home).join(".codex").display().to_string())
    });
    let worktree_git_dir = if value.contains("{worktree_git_dir}") {
        Some(resolve_worktree_git_dir(worktree_path)?)
    } else {
        None
    };
    Ok(value
        .replace("{bundle_bin}", &bundle_bin.display().to_string())
        .replace("{workspace_root}", &workspace_root.display().to_string())
        .replace("{worktree_path}", worktree_path)
        .replace(
            "{invocation_context_path}",
            &invocation_context_path.display().to_string(),
        )
        .replace("{codex_home}", codex_home.as_deref().unwrap_or(""))
        .replace(
            "{worktree_git_dir}",
            worktree_git_dir.as_deref().unwrap_or(""),
        ))
}

fn resolve_worktree_git_dir(worktree_path: &str) -> Result<String> {
    let git_metadata_path = Path::new(worktree_path).join(".git");
    if git_metadata_path.is_dir() {
        return Ok(git_metadata_path.display().to_string());
    }
    let git_pointer = fs::read_to_string(&git_metadata_path).with_context(|| {
        format!(
            "failed to read worktree git metadata pointer {}",
            git_metadata_path.display()
        )
    })?;
    let pointer = git_pointer
        .trim()
        .strip_prefix("gitdir:")
        .map(str::trim)
        .ok_or_else(|| {
            anyhow!(
                "expected gitdir pointer in {}",
                git_metadata_path.display()
            )
        })?;
    let resolved = Path::new(pointer);
    let resolved = if resolved.is_absolute() {
        resolved.to_path_buf()
    } else {
        Path::new(worktree_path).join(resolved)
    };
    Ok(resolved.display().to_string())
}

#[cfg(test)]
mod tests {
    use std::env;
    use std::fs;
    use std::path::Path;
    use std::time::{SystemTime, UNIX_EPOCH};

    use anyhow::Result;

    use super::{ExecutorProfile, resolve_executor_command};

    #[test]
    fn resolve_executor_command_uses_the_resolved_bundle_binary_path() -> Result<()> {
        let executor_profile = ExecutorProfile {
            kind: "local_command".to_owned(),
            command: "{bundle_bin}".to_owned(),
            args: vec![
                "mock-executor".to_owned(),
                "worker".to_owned(),
                "{invocation_context_path}".to_owned(),
            ],
            bypass_sandbox_args: None,
            bypass_sandbox_inherit_env: false,
            cwd: "worktree".to_owned(),
            timeout_sec: 60,
            transcript_capture: "stdio".to_owned(),
            env_allow: None,
        };
        let bundle_bin = Path::new("/tmp/target/debug/loopy-submit-loop");
        let invocation_context_path = Path::new("/tmp/workspace/.loopy/invocations/inv-1.json");

        let (command, args_variant) = resolve_executor_command(
            &executor_profile,
            bundle_bin,
            Path::new("/tmp/workspace"),
            "/tmp/workspace/.loopy/worktrees/loop",
            invocation_context_path,
            false,
        )?;

        assert_eq!(args_variant, "default");
        assert_eq!(command[0], bundle_bin.display().to_string());
        assert_eq!(command[1], "mock-executor");
        assert_eq!(command[2], "worker");
        assert_eq!(command[3], invocation_context_path.display().to_string());

        Ok(())
    }

    #[test]
    fn resolve_executor_command_supports_worktree_git_dir_placeholder() -> Result<()> {
        let unique_suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)?
            .as_nanos();
        let workspace = env::temp_dir().join(format!(
            "loopy-submit-loop-bundle-worktree-gitdir-{unique_suffix}"
        ));
        if workspace.exists() {
            fs::remove_dir_all(&workspace)?;
        }
        fs::create_dir_all(&workspace)?;
        let worktree = workspace.join(".loopy/worktrees/loop");
        fs::create_dir_all(&worktree)?;
        let gitdir = workspace.join(".loopy/git-common-loop/worktrees/loop");
        fs::create_dir_all(&gitdir)?;
        fs::write(worktree.join(".git"), format!("gitdir: {}\n", gitdir.display()))?;

        let executor_profile = ExecutorProfile {
            kind: "local_command".to_owned(),
            command: "codex".to_owned(),
            args: vec![
                "exec".to_owned(),
                "--add-dir".to_owned(),
                "{worktree_git_dir}".to_owned(),
                "-C".to_owned(),
                "{worktree_path}".to_owned(),
            ],
            bypass_sandbox_args: None,
            bypass_sandbox_inherit_env: false,
            cwd: "worktree".to_owned(),
            timeout_sec: 60,
            transcript_capture: "stdio".to_owned(),
            env_allow: None,
        };
        let invocation_context_path = workspace.join(".loopy/invocations/inv-1.json");

        let (command, args_variant) = resolve_executor_command(
            &executor_profile,
            Path::new("/tmp/target/debug/loopy-submit-loop"),
            &workspace,
            worktree
                .to_str()
                .expect("temp worktree path should be valid utf-8"),
            &invocation_context_path,
            false,
        )?;

        assert_eq!(args_variant, "default");
        assert_eq!(command[3], gitdir.display().to_string());
        fs::remove_dir_all(&workspace)?;

        Ok(())
    }
}
