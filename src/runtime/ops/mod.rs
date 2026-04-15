// Operation modules own write-side runtime workflows; shared state/query helpers stay outside ops.

use super::*;

const MIN_TIMEOUT_EVIDENCE_WORDS: usize = 5;
const MIN_TIMEOUT_EVIDENCE_NON_WS_CHARS: usize = 24;

pub(crate) fn ensure_timeout_extension_request_shape(
    requested_timeout_sec: i64,
    progress_summary: &str,
    rationale: &str,
) -> Result<()> {
    if requested_timeout_sec <= 0 {
        bail!("requested_timeout_sec must be greater than zero");
    }
    let (progress_summary, rationale) =
        normalize_timeout_request_evidence(progress_summary, rationale);
    if !timeout_request_text_is_substantive(progress_summary.as_str()) {
        bail!(
            "progress_summary must describe concrete progress with at least five words and 24 non-whitespace characters"
        );
    }
    if !timeout_request_text_is_substantive(rationale.as_str()) {
        bail!(
            "rationale must explain the need for more time with at least five words and 24 non-whitespace characters"
        );
    }
    if progress_summary == rationale {
        bail!("progress_summary and rationale must differ");
    }
    Ok(())
}

pub(crate) fn timeout_request_has_progress_evidence(
    progress_summary: &str,
    rationale: &str,
) -> bool {
    let (progress_summary, rationale) =
        normalize_timeout_request_evidence(progress_summary, rationale);
    timeout_request_text_is_substantive(progress_summary.as_str())
        && timeout_request_text_is_substantive(rationale.as_str())
        && progress_summary != rationale
}

fn timeout_request_text_is_substantive(value: &str) -> bool {
    value.split_whitespace().count() >= MIN_TIMEOUT_EVIDENCE_WORDS
        && value.chars().filter(|ch| !ch.is_whitespace()).count()
            >= MIN_TIMEOUT_EVIDENCE_NON_WS_CHARS
}

fn normalize_timeout_request_text(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn normalize_timeout_request_evidence(progress_summary: &str, rationale: &str) -> (String, String) {
    (
        normalize_timeout_request_text(progress_summary),
        normalize_timeout_request_text(rationale),
    )
}

pub(crate) mod caller_finalize;
pub(crate) mod invocation;
pub(crate) mod r#loop;
pub(crate) mod submissions;
