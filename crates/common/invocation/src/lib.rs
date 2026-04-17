use std::env;
use std::io::{Read, Write};
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow, bail, Context};

#[cfg(unix)]
const SIGKILL: i32 = 9;

#[cfg(unix)]
unsafe extern "C" {
    fn kill(pid: i32, sig: i32) -> i32;
    fn setpgid(pid: i32, pgid: i32) -> i32;
}

// Owns neutral child-process invocation mechanics only.

pub fn run_local_command(
    command: &[String],
    cwd: &Path,
    stdin_payload: Option<&str>,
    timeout_sec: i64,
    env_policy: &str,
    env_allow: &[String],
) -> Result<std::process::Output> {
    let (program, args) = command
        .split_first()
        .ok_or_else(|| anyhow!("executor command cannot be empty"))?;
    let timeout_sec = u64::try_from(timeout_sec)
        .map_err(|_| anyhow!("executor timeout_sec must be non-negative"))?;
    let resolved_program = resolve_executable_path(program)?;
    let mut child_command = Command::new(&resolved_program);
    child_command
        .args(args)
        .current_dir(cwd)
        .stdin(if stdin_payload.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    match env_policy {
        "allowlist" => {
            child_command.env_clear();
            if let Some(path) = env::var_os("PATH") {
                child_command.env("PATH", path);
            }
            child_command.envs(env_allow.iter().filter_map(|name| {
                env::var_os(name).map(|value| (name.as_str().to_owned(), value))
            }));
        }
        "inherit_all" => {}
        other => bail!("unsupported executor env_policy {other}"),
    }
    #[cfg(unix)]
    unsafe {
        child_command.pre_exec(|| {
            if setpgid(0, 0) == -1 {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(())
            }
        });
    }
    let mut child = child_command
        .spawn()
        .with_context(|| format!("failed to run {program} in {}", cwd.display()))?;
    let stdout_reader = spawn_pipe_reader(
        child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("failed to capture stdout for {program}"))?,
        program,
        "stdout",
    );
    let stderr_reader = spawn_pipe_reader(
        child
            .stderr
            .take()
            .ok_or_else(|| anyhow!("failed to capture stderr for {program}"))?,
        program,
        "stderr",
    );
    if let Some(stdin_payload) = stdin_payload {
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("failed to capture stdin for {program}"))?;
        stdin
            .write_all(stdin_payload.as_bytes())
            .with_context(|| format!("failed to write stdin for {program}"))?;
        drop(stdin);
    }
    let deadline = Instant::now() + Duration::from_secs(timeout_sec);
    loop {
        if let Some(status) = child
            .try_wait()
            .with_context(|| format!("failed to poll {program} in {}", cwd.display()))?
        {
            let stdout = join_pipe_reader(stdout_reader, program, "stdout")?;
            let stderr = join_pipe_reader(stderr_reader, program, "stderr")?;
            return Ok(std::process::Output {
                status,
                stdout,
                stderr,
            });
        }
        if Instant::now() >= deadline {
            if child
                .try_wait()
                .with_context(|| format!("failed to poll {program} in {}", cwd.display()))?
                .is_none()
            {
                terminate_timed_out_child(&mut child, program)?;
            }
            let _ = child
                .wait()
                .with_context(|| format!("failed to reap timed out executor {program}"))?;
            let _ = join_pipe_reader(stdout_reader, program, "stdout")?;
            let _ = join_pipe_reader(stderr_reader, program, "stderr")?;
            bail!(
                "executor {program} timed out after {}s in {}",
                timeout_sec,
                cwd.display()
            );
        }
        thread::sleep(Duration::from_millis(50));
    }
}

fn spawn_pipe_reader<R>(
    mut reader: R,
    program: &str,
    stream_name: &str,
) -> thread::JoinHandle<Result<Vec<u8>>>
where
    R: Read + Send + 'static,
{
    let program = program.to_owned();
    let stream_name = stream_name.to_owned();
    thread::spawn(move || {
        let mut output = Vec::new();
        reader
            .read_to_end(&mut output)
            .with_context(|| format!("failed to read {stream_name} for {program}"))?;
        Ok(output)
    })
}

fn join_pipe_reader(
    reader: thread::JoinHandle<Result<Vec<u8>>>,
    program: &str,
    stream_name: &str,
) -> Result<Vec<u8>> {
    match reader.join() {
        Ok(result) => result,
        Err(_) => bail!("reader thread panicked while draining {stream_name} for {program}"),
    }
}

fn resolve_executable_path(program: &str) -> Result<PathBuf> {
    let program_path = Path::new(program);
    if program_path.is_absolute() || program_path.components().count() > 1 {
        return Ok(program_path.to_path_buf());
    }
    let path = env::var_os("PATH")
        .ok_or_else(|| anyhow!("PATH is not set while resolving executable {program}"))?;
    for directory in env::split_paths(&path) {
        let candidate = directory.join(program);
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    bail!("failed to resolve executable path for {program}")
}

fn terminate_timed_out_child(child: &mut std::process::Child, program: &str) -> Result<()> {
    #[cfg(unix)]
    {
        let process_group_id = i32::try_from(child.id())
            .map_err(|_| anyhow!("executor pid exceeded i32 range for {program}"))?;
        if unsafe { kill(-process_group_id, SIGKILL) } == 0 {
            return Ok(());
        }
    }
    child
        .kill()
        .with_context(|| format!("failed to terminate timed out executor {program}"))
}
