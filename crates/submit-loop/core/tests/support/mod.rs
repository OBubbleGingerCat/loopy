use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use anyhow::{Context, Result, bail};
use loopy::{
    CheckpointAcceptance, CheckpointDeliverable, CheckpointPlanItem, OpenLoopRequest,
    OpenReviewRoundRequest, PrepareWorktreeRequest, ReviewKind, Runtime,
    StartReviewerInvocationRequest, StartWorkerInvocationRequest, SubmitArtifactReviewRequest,
    SubmitCandidateCommitRequest, SubmitCheckpointPlanRequest, SubmitCheckpointReviewRequest,
    WorkerStage,
};
use rusqlite::{Connection, params};
use serde_json::json;
use tempfile::TempDir;
use uuid::Uuid;

#[allow(dead_code)]
pub fn checkpoint(title: &str) -> CheckpointPlanItem {
    let slug = title
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    let deliverable_path = format!("artifacts/{slug}.txt");
    CheckpointPlanItem {
        title: title.to_owned(),
        kind: "artifact".to_owned(),
        deliverables: vec![CheckpointDeliverable {
            path: deliverable_path.clone(),
            deliverable_type: "file".to_owned(),
        }],
        acceptance: CheckpointAcceptance {
            verification_steps: vec![format!("test -f {deliverable_path}")],
            expected_outcomes: vec![format!("{title} deliverable is present")],
        },
    }
}

#[allow(dead_code)]
pub fn checkpoint_json(title: &str) -> String {
    serde_json::to_string(&vec![checkpoint(title)]).expect("checkpoint fixture should serialize")
}

#[allow(dead_code)]
pub fn command_help_output(bin_path: &Path, command: &str) -> Result<String> {
    let output = Command::new(bin_path)
        .args([command, "--help"])
        .output()
        .with_context(|| format!("failed to run {} {} --help", bin_path.display(), command))?;
    if !output.status.success() {
        bail!(
            "{} {} --help failed\nstdout:\n{}\nstderr:\n{}",
            bin_path.display(),
            command,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(String::from_utf8(output.stdout).context("help output was not utf-8")?)
}

#[allow(dead_code)]
pub fn required_long_flags_from_help(help_output: &str) -> Result<Vec<String>> {
    let usage_line = help_output
        .lines()
        .find(|line| line.trim_start().starts_with("Usage:"))
        .context("help output missing Usage line")?;
    Ok(usage_line
        .split_whitespace()
        .filter(|token| token.starts_with("--"))
        .map(str::to_owned)
        .collect())
}

#[allow(dead_code)]
pub fn ensure_prompt_covers_required_help_flags(
    bin_path: &Path,
    prompt: &str,
    command: &str,
) -> Result<()> {
    let help_output = command_help_output(bin_path, command)?;
    let required_flags = required_long_flags_from_help(&help_output)?;
    let command_lines = prompt
        .lines()
        .filter(|line| line.contains(command))
        .collect::<Vec<_>>();
    if command_lines.is_empty() {
        bail!("prompt did not mention command `{command}`");
    }
    let joined_lines = command_lines.join("\n");
    let missing = required_flags
        .iter()
        .filter(|flag| !joined_lines.contains(flag.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        bail!(
            "prompt lines for `{command}` missed required flags {missing:?}\nhelp:\n{help_output}\nprompt lines:\n{joined_lines}"
        );
    }
    Ok(())
}

#[allow(dead_code)]
pub fn materialize_loop_worktree_with_mirrored_gitdir(
    workspace_root: &Path,
    branch: &str,
    label: &str,
) -> Result<PathBuf> {
    let worktree_path = workspace_root.join(".loopy").join("worktrees").join(label);
    let mirror_path = workspace_root
        .join(".loopy")
        .join(format!("git-common-{label}"));
    if let Some(parent) = worktree_path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::create_dir_all(&mirror_path)?;

    let copy_output = Command::new("cp")
        .args(["-a", ".git/.", mirror_path.to_str().unwrap()])
        .current_dir(workspace_root)
        .output()
        .context("failed to copy primary gitdir into mirrored fallback")?;
    if !copy_output.status.success() {
        bail!(
            "cp -a .git/. {} failed\nstdout:\n{}\nstderr:\n{}",
            mirror_path.display(),
            String::from_utf8_lossy(&copy_output.stdout),
            String::from_utf8_lossy(&copy_output.stderr)
        );
    }

    let branch_exists_output = Command::new("git")
        .args([
            "show-ref",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ])
        .current_dir(workspace_root)
        .output()
        .context("failed to inspect mirrored fallback branch state")?;
    let mut args = vec![
        format!("--git-dir={}", mirror_path.display()),
        format!("--work-tree={}", workspace_root.display()),
        "worktree".to_owned(),
        "add".to_owned(),
    ];
    if branch_exists_output.status.success() {
        args.push(worktree_path.to_str().unwrap().to_owned());
        args.push(branch.to_owned());
    } else {
        args.push("-b".to_owned());
        args.push(branch.to_owned());
        args.push(worktree_path.to_str().unwrap().to_owned());
        args.push("HEAD".to_owned());
    }
    let output = Command::new("git")
        .args(args.iter().map(String::as_str))
        .current_dir(workspace_root)
        .output()
        .context("failed to materialize mirrored-gitdir worktree")?;
    if !output.status.success() {
        bail!(
            "git worktree add with mirrored gitdir failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(worktree_path)
}

#[allow(dead_code)]
#[derive(Debug)]
pub struct AcceptedLoopFixture {
    pub loop_id: String,
    pub branch: String,
    pub label: String,
    pub checkpoint_id: String,
    pub accepted_commit_sha: String,
}

#[allow(dead_code)]
pub fn accept_single_checkpoint_loop(
    runtime: &Runtime,
    workspace_root: &Path,
    summary: &str,
    deliverable_path: &str,
    deliverable_contents: &str,
) -> Result<AcceptedLoopFixture> {
    let opened = runtime.open_loop(OpenLoopRequest {
        summary: summary.to_owned(),
        task_type: "coding-task".to_owned(),
        context: Some("exercise caller finalize handoff".to_owned()),
        planning_worker: None,
        artifact_worker: None,
        checkpoint_reviewers: Some(vec!["mock".to_owned()]),
        artifact_reviewers: Some(vec!["mock".to_owned()]),
        constraints: Some(json!({})),
        bypass_sandbox: Some(false),
        coordinator_prompt: "You are the coordinator.".to_owned(),
    })?;

    let prepared = runtime.prepare_worktree(PrepareWorktreeRequest {
        loop_id: opened.loop_id.clone(),
    })?;
    let worktree_path = PathBuf::from(
        prepared["path"]
            .as_str()
            .context("prepare-worktree response missing path")?,
    );

    let planning = runtime.start_worker_invocation(StartWorkerInvocationRequest {
        loop_id: opened.loop_id.clone(),
        stage: WorkerStage::Planning,
        checkpoint_id: None,
    })?;
    runtime.submit_checkpoint_plan(SubmitCheckpointPlanRequest {
        invocation_context_path: invocation_context_path(workspace_root, &planning.invocation_id),
        submission_id: "plan-submit".to_owned(),
        checkpoints: vec![CheckpointPlanItem {
            title: "Feature checkpoint".to_owned(),
            kind: "artifact".to_owned(),
            deliverables: vec![CheckpointDeliverable {
                path: deliverable_path.to_owned(),
                deliverable_type: "file".to_owned(),
            }],
            acceptance: CheckpointAcceptance {
                verification_steps: vec![format!("test -f {deliverable_path}")],
                expected_outcomes: vec![format!("{deliverable_path} exists")],
            },
        }],
        improvement_opportunities: None,
        notes: None,
    })?;

    let checkpoint_review = runtime.open_review_round(OpenReviewRoundRequest {
        loop_id: opened.loop_id.clone(),
        review_kind: ReviewKind::Checkpoint,
        target_type: "plan_revision".to_owned(),
        target_ref: "plan-1".to_owned(),
    })?;
    let checkpoint_reviewer =
        runtime.start_reviewer_invocation(StartReviewerInvocationRequest {
            loop_id: opened.loop_id.clone(),
            review_round_id: checkpoint_review.review_round_id,
            review_slot_id: checkpoint_review.review_slot_ids[0].clone(),
        })?;
    runtime.submit_checkpoint_review(SubmitCheckpointReviewRequest {
        invocation_context_path: invocation_context_path(
            workspace_root,
            &checkpoint_reviewer.invocation_id,
        ),
        submission_id: "checkpoint-review-approve".to_owned(),
        decision: "approve".to_owned(),
        blocking_issues: vec![],
        nonblocking_issues: None,
        improvement_opportunities: None,
        summary: "approved".to_owned(),
        notes: None,
    })?;

    let checkpoint_id: String = {
        let connection = Connection::open(workspace_root.join(".loopy/loopy.db"))?;
        connection.query_row(
            "SELECT checkpoint_id FROM SUBMIT_LOOP__checkpoint_current WHERE loop_id = ?1",
            params![opened.loop_id.clone()],
            |row| row.get(0),
        )?
    };

    let artifact_worker = runtime.start_worker_invocation(StartWorkerInvocationRequest {
        loop_id: opened.loop_id.clone(),
        stage: WorkerStage::Artifact,
        checkpoint_id: Some(checkpoint_id.clone()),
    })?;
    let deliverable = worktree_path.join(deliverable_path);
    if let Some(parent) = deliverable.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&deliverable, deliverable_contents)?;
    git(&worktree_path, &["add", deliverable_path])?;
    git(&worktree_path, &["commit", "-m", "implement checkpoint"])?;
    let accepted_commit_sha = git_output(&worktree_path, &["rev-parse", "HEAD"])?;
    runtime.submit_candidate_commit(SubmitCandidateCommitRequest {
        invocation_context_path: invocation_context_path(
            workspace_root,
            &artifact_worker.invocation_id,
        ),
        submission_id: "candidate-submit".to_owned(),
        candidate_commit_sha: accepted_commit_sha.clone(),
        change_summary: json!({
            "headline": "Implemented feature checkpoint",
            "files": [deliverable_path],
        }),
        improvement_opportunities: None,
        notes: None,
    })?;

    let artifact_review = runtime.open_review_round(OpenReviewRoundRequest {
        loop_id: opened.loop_id.clone(),
        review_kind: ReviewKind::Artifact,
        target_type: "checkpoint_id".to_owned(),
        target_ref: checkpoint_id.clone(),
    })?;
    let artifact_reviewer = runtime.start_reviewer_invocation(StartReviewerInvocationRequest {
        loop_id: opened.loop_id.clone(),
        review_round_id: artifact_review.review_round_id,
        review_slot_id: artifact_review.review_slot_ids[0].clone(),
    })?;
    runtime.submit_artifact_review(SubmitArtifactReviewRequest {
        invocation_context_path: invocation_context_path(
            workspace_root,
            &artifact_reviewer.invocation_id,
        ),
        submission_id: "artifact-review-approve".to_owned(),
        decision: "approve".to_owned(),
        blocking_issues: vec![],
        nonblocking_issues: None,
        improvement_opportunities: None,
        summary: "approved".to_owned(),
        notes: None,
    })?;

    Ok(AcceptedLoopFixture {
        loop_id: opened.loop_id,
        branch: opened.branch,
        label: opened.label,
        checkpoint_id,
        accepted_commit_sha,
    })
}

#[allow(dead_code)]
pub fn invocation_context_path(workspace_root: &Path, invocation_id: &str) -> PathBuf {
    workspace_root
        .join(".loopy")
        .join("invocations")
        .join(format!("{invocation_id}.json"))
}

#[allow(dead_code)]
pub fn repo_root() -> &'static PathBuf {
    static REPO_ROOT: OnceLock<PathBuf> = OnceLock::new();
    REPO_ROOT
        .get_or_init(|| {
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .ancestors()
                .nth(3)
                .expect("core crate should live under <repo>/crates/submit-loop/core")
                .to_path_buf()
        })
}

#[allow(dead_code)]
pub fn submit_loop_source_root() -> &'static PathBuf {
    static SUBMIT_LOOP_SOURCE_ROOT: OnceLock<PathBuf> = OnceLock::new();
    SUBMIT_LOOP_SOURCE_ROOT.get_or_init(|| repo_root().join("skills").join("submit-loop"))
}

#[allow(dead_code)]
pub fn install_bundle_into_workspace(workspace_root: &Path) -> Result<PathBuf> {
    let install_root = workspace_root
        .join(".loopy")
        .join("installed-skills")
        .join("loopy-submit-loop");
    let repo_root = repo_root();
    let output = Command::new("bash")
        .arg("scripts/install-submit-loop-skill.sh")
        .arg(&install_root)
        .env("CARGO_NET_OFFLINE", "true")
        .current_dir(&repo_root)
        .output()
        .context("failed to run install-submit-loop-skill.sh")?;
    if !output.status.success() {
        bail!(
            "installer failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    install_fake_codex_command(workspace_root, &install_root)?;
    Ok(install_root)
}

fn install_fake_codex_command(workspace_root: &Path, install_root: &Path) -> Result<()> {
    let fake_bin_dir = workspace_root.join(".loopy").join("fake-bin");
    fs::create_dir_all(&fake_bin_dir)?;
    let fake_codex = fake_bin_dir.join("codex");
    fs::write(
        &fake_codex,
        "#!/bin/bash\nwhile IFS= read -r _; do :; done\nprintf '{}\\n'\n",
    )?;
    let chmod_output = Command::new("chmod")
        .args([
            "755",
            fake_codex.to_str().context("non-utf8 fake codex path")?,
        ])
        .output()
        .context("failed to chmod fake codex command")?;
    if !chmod_output.status.success() {
        bail!(
            "chmod fake codex failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&chmod_output.stdout),
            String::from_utf8_lossy(&chmod_output.stderr)
        );
    }

    let manifest_path = install_root.join("submit-loop.toml");
    let manifest = fs::read_to_string(&manifest_path)?;
    let fake_codex_str = fake_codex
        .to_str()
        .context("non-utf8 fake codex path")?
        .to_owned();
    let updated = manifest.replace(
        "command = \"codex\"",
        &format!("command = \"{fake_codex_str}\""),
    );
    if updated == manifest {
        bail!(
            "failed to rewrite codex executor commands in {}",
            manifest_path.display()
        );
    }
    fs::write(&manifest_path, updated)?;
    Ok(())
}

#[allow(dead_code)]
pub fn git_workspace() -> Result<TempDir> {
    let workspace = tempfile::tempdir()?;
    git(workspace.path(), &["init", "--initial-branch=main"])?;
    git(workspace.path(), &["config", "user.name", "Codex"])?;
    git(
        workspace.path(),
        &["config", "user.email", "codex@example.com"],
    )?;
    fs::write(workspace.path().join("README.md"), "seed\n")?;
    git(workspace.path(), &["add", "README.md"])?;
    git(workspace.path(), &["commit", "-m", "seed"])?;
    Ok(workspace)
}

#[allow(dead_code)]
pub fn git_output(cwd: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .with_context(|| format!("failed to run git {}", args.join(" ")))?;
    if !output.status.success() {
        bail!(
            "git {} failed\nstdout:\n{}\nstderr:\n{}",
            args.join(" "),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}

#[allow(dead_code)]
pub fn git(cwd: &Path, args: &[&str]) -> Result<()> {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .with_context(|| format!("failed to run git {}", args.join(" ")))?;
    if !output.status.success() {
        bail!(
            "git {} failed\nstdout:\n{}\nstderr:\n{}",
            args.join(" "),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(())
}

#[allow(dead_code)]
pub fn inject_pending_review_round_opened_event(
    workspace_root: &Path,
    loop_id: &str,
    review_kind: &str,
    reviewer_role_id: &str,
) -> Result<(String, String)> {
    let connection = Connection::open(workspace_root.join(".loopy/loopy.db"))?;
    let loop_seq: i64 = connection.query_row(
        "SELECT COALESCE(MAX(loop_seq), 0) + 1 FROM CORE__events WHERE loop_id = ?1",
        [loop_id],
        |row| row.get(0),
    )?;
    let review_round_id = format!("review-{}", Uuid::now_v7());
    let review_slot_id = format!("slot-{}", Uuid::now_v7());
    let payload = json!({
        "review_round_id": review_round_id,
        "review_kind": review_kind,
        "round_status": "pending",
        "target_type": "plan_revision",
        "target_ref": "plan-1",
        "target_metadata": {},
        "slot_state": [{
            "review_slot_id": review_slot_id,
            "reviewer_role_id": reviewer_role_id,
            "status": "pending",
        }],
    });
    let recorded_at = "2026-04-13T00:00:00Z";
    connection.execute(
        r#"
        INSERT INTO CORE__events (
            loop_id,
            loop_seq,
            event_name,
            payload_json,
            occurred_at,
            recorded_at
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
        "#,
        params![
            loop_id,
            loop_seq,
            "SUBMIT_LOOP__review_round_opened",
            serde_json::to_string(&payload)?,
            recorded_at,
            recorded_at
        ],
    )?;
    Ok((review_round_id, review_slot_id))
}
