//! Pinned external promptGen mechanism. No image, provider, session or artifact
//! store authority lives here.
use crate::semantic_mask::{
    ProcessGroupIdentity, capture_process_group, cleanup_process_group, configure_process_group,
    wait_for_leader_exit,
};
use agent_contract::compilation::{CompilationOutcome, CompilationStatus};
use agent_contract::json::JsonValue;
use agent_core::{
    CompiledImageBackground, CompiledImageDetail, CompiledImageOutput, CompiledImagePrompt,
    MAX_COMPILE_INPUT_BYTES, PromptGenSpec, prompt_content_sha256,
};
use sha2::{Digest, Sha256};
use std::{
    ffi::OsString,
    fs::{File, OpenOptions},
    io::Read,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    process::{Child, Command},
};
use tokio_util::sync::CancellationToken;

pub(crate) const MAX_PROMPTGEN_STDOUT_BYTES: usize = 1024 * 1024;
const MAX_PROMPTGEN_STDERR_BYTES: usize = 64 * 1024;
const MAX_EXECUTABLE_BYTES: usize = 128 * 1024 * 1024;
const MAX_CODEX_EXECUTABLE_BYTES: u64 = 512 * 1024 * 1024;
const PIPE_DRAIN_TIMEOUT: Duration = Duration::from_secs(2);

pub(crate) struct EditPaths<'a> {
    pub input: &'a Path,
    pub source: &'a Path,
    pub output: &'a Path,
    pub receipt: &'a Path,
}

#[derive(Debug, Clone)]
pub struct PromptGenAdapter {
    executable: PathBuf,
    sha256: String,
    timeout_ms: u64,
    codex_runtime: Option<PinnedCodexRuntime>,
}

#[derive(Debug, Clone)]
struct PinnedCodexRuntime {
    executable: PathBuf,
    sha256: String,
    home: PathBuf,
}

#[derive(Debug)]
pub(crate) struct InvocationError {
    /// True only after the image-edit process was spawned. Unknown outcomes at
    /// this point must use the session's existing `ResolutionRequired` path.
    pub dispatched: bool,
    pub message: String,
}
impl InvocationError {
    fn before(message: impl Into<String>) -> Self {
        Self {
            dispatched: false,
            message: message.into(),
        }
    }
}

struct ChildGroup {
    child: Child,
    identity: ProcessGroupIdentity,
}
impl Drop for ChildGroup {
    fn drop(&mut self) {
        // Once reaped child.id() is None, so no recycled process-group ID can
        // be signalled. Before then, cancellation of our future kills the group.
        #[cfg(unix)]
        if self.child.id().is_some() {
            crate::semantic_mask::force_process_group(self.identity);
        }
    }
}

impl PromptGenAdapter {
    pub fn from_spec(spec: &PromptGenSpec) -> Result<Self, String> {
        if !spec.executable.is_absolute()
            || spec.timeout_ms == 0
            || spec.timeout_ms > agent_core::PROMPTGEN_MAX_TIMEOUT_MS
        {
            return Err(
                "promptGen requires an absolute executable and a bounded timeout".to_owned(),
            );
        }
        validate_executable(&spec.executable, &spec.executable_sha256)?;
        let executable = spec
            .executable
            .canonicalize()
            .map_err(|error| error.to_string())?;
        validate_executable(&executable, &spec.executable_sha256)?;
        let codex_runtime = spec
            .codex_runtime
            .as_ref()
            .map(|runtime| {
                if !runtime.executable.is_absolute() || !runtime.home.is_absolute() {
                    return Err(
                        "PromptGen Codex runtime requires absolute executable and home paths"
                            .to_owned(),
                    );
                }
                verify_executable_identity(
                    &runtime.executable,
                    &runtime.executable_sha256,
                    MAX_CODEX_EXECUTABLE_BYTES,
                    "Codex executable",
                )?;
                let executable = runtime
                    .executable
                    .canonicalize()
                    .map_err(|error| error.to_string())?;
                Ok(PinnedCodexRuntime {
                    executable,
                    sha256: runtime.executable_sha256.to_ascii_lowercase(),
                    home: runtime.home.clone(),
                })
            })
            .transpose()?;
        Ok(Self {
            executable,
            sha256: spec.executable_sha256.to_ascii_lowercase(),
            timeout_ms: spec.timeout_ms,
            codex_runtime,
        })
    }
    #[must_use]
    pub fn sha256(&self) -> &str {
        &self.sha256
    }

    pub(crate) fn codex_sha256(&self) -> Option<&str> {
        self.codex_runtime
            .as_ref()
            .map(|runtime| runtime.sha256.as_str())
    }

    pub(crate) async fn compile_image(
        &self,
        input: &str,
        timeout_ms: u64,
        cancellation: &CancellationToken,
    ) -> Result<CompiledImagePrompt, String> {
        validate_input(input)?;
        let work = tempfile::tempdir().map_err(|error| error.to_string())?;
        let work_path = work
            .path()
            .canonicalize()
            .map_err(|error| error.to_string())?;
        let path = work_path.join("input.json");
        std::fs::write(&path, input).map_err(|error| error.to_string())?;
        let (bytes, outcome) = self
            .prompt_only(&path, &work_path, timeout_ms, cancellation)
            .await?;
        compiled_image_from_outcome(input, &bytes, outcome)
    }

    pub(crate) async fn preflight_edit(
        &self,
        input: &str,
        path: &Path,
        timeout_ms: u64,
        cancellation: &CancellationToken,
    ) -> Result<String, String> {
        validate_input(input)?;
        std::fs::write(path, input).map_err(|error| error.to_string())?;
        let work = path.parent().ok_or("missing private promptGen directory")?;
        let (_, mut outcome) = self
            .prompt_only(path, work, timeout_ms, cancellation)
            .await?;
        take_image_prompt(&mut outcome, "preflight")
    }

    async fn prompt_only(
        &self,
        input: &Path,
        work: &Path,
        timeout_ms: u64,
        cancellation: &CancellationToken,
    ) -> Result<(Vec<u8>, CompilationOutcome), String> {
        let args = vec![
            "image".into(),
            "--input".into(),
            input.as_os_str().to_owned(),
            "--mode".into(),
            "prompt-only".into(),
            "--format".into(),
            "json".into(),
        ];
        let bytes = self
            .invoke(args, work, timeout_ms, cancellation, false)
            .await
            .map_err(|error| error.message)?;
        let outcome: CompilationOutcome = serde_json::from_slice(&bytes)
            .map_err(|error| format!("invalid image compiler result: {error}"))?;
        Ok((bytes, outcome))
    }

    pub(crate) async fn dispatch_edit(
        &self,
        paths: EditPaths<'_>,
        timeout_ms: u64,
        cancellation: &CancellationToken,
    ) -> Result<(), InvocationError> {
        let EditPaths {
            input,
            source,
            output,
            receipt,
        } = paths;
        let work = input
            .parent()
            .ok_or_else(|| InvocationError::before("missing private edit directory"))?;
        let codex = self.codex_runtime.as_ref().ok_or_else(|| {
            InvocationError::before("semantic editing requires a pinned PromptGen Codex runtime")
        })?;
        verify_executable_identity(
            &codex.executable,
            &codex.sha256,
            MAX_CODEX_EXECUTABLE_BYTES,
            "Codex executable",
        )
        .map_err(InvocationError::before)?;
        let seconds = timeout_ms.min(self.timeout_ms).div_ceil(1000).max(1);
        let args = vec![
            "image".into(),
            "--input".into(),
            input.as_os_str().to_owned(),
            "--mode".into(),
            "codex-imagegen".into(),
            "--format".into(),
            "json".into(),
            "--result".into(),
            receipt.as_os_str().to_owned(),
            "--image-output".into(),
            output.as_os_str().to_owned(),
            "--reference-image".into(),
            source.as_os_str().to_owned(),
            "--timeout-seconds".into(),
            seconds.to_string().into(),
            "--max-fidelity-attempts".into(),
            "1".into(),
            "--codex-binary".into(),
            codex.executable.as_os_str().to_owned(),
            "--codex-sha256".into(),
            codex.sha256.clone().into(),
            "--codex-home".into(),
            codex.home.as_os_str().to_owned(),
        ];
        self.invoke(args, work, timeout_ms, cancellation, true)
            .await
            .map(|_| ())
    }

    async fn invoke(
        &self,
        args: Vec<OsString>,
        work: &Path,
        timeout_ms: u64,
        cancellation: &CancellationToken,
        external_effect: bool,
    ) -> Result<Vec<u8>, InvocationError> {
        validate_executable(&self.executable, &self.sha256).map_err(InvocationError::before)?;
        if cancellation.is_cancelled() || timeout_ms == 0 {
            return Err(InvocationError::before(
                "promptGen cancelled before dispatch",
            ));
        }
        if !cfg!(unix) {
            return Err(InvocationError::before(
                "promptGen process-group execution requires a Unix host",
            ));
        }
        let mut command = Command::new(&self.executable);
        command
            .args(args)
            .current_dir(work)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("LC_ALL", "C")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        configure_process_group(&mut command);
        let mut child = command
            .spawn()
            .map_err(|error| InvocationError::before(format!("promptGen spawn failed: {error}")))?;
        let dispatched = external_effect;
        let identity = match capture_process_group(&child) {
            Ok(identity) => identity,
            Err(error) => {
                let _ = child.start_kill();
                let _ = tokio::time::timeout(PIPE_DRAIN_TIMEOUT, child.wait()).await;
                return Err(InvocationError {
                    dispatched,
                    message: format!("promptGen group identity unproven: {error}"),
                });
            }
        };
        let mut group = ChildGroup { child, identity };
        let stdout = group.child.stdout.take().ok_or_else(|| InvocationError {
            dispatched,
            message: "promptGen stdout unavailable".to_owned(),
        })?;
        let stderr = group.child.stderr.take().ok_or_else(|| InvocationError {
            dispatched,
            message: "promptGen stderr unavailable".to_owned(),
        })?;
        let pipes = async {
            tokio::try_join!(
                bounded_pipe(stdout, MAX_PROMPTGEN_STDOUT_BYTES),
                bounded_pipe(stderr, MAX_PROMPTGEN_STDERR_BYTES)
            )
        };
        tokio::pin!(pipes);
        let lifecycle = async {
            tokio::try_join!(wait_for_leader_exit(identity), &mut pipes).map(|((), output)| output)
        };
        let result = tokio::select! {
            biased;
            () = cancellation.cancelled() => Err("promptGen cancelled".to_owned()),
            value = tokio::time::timeout(Duration::from_millis(timeout_ms.min(self.timeout_ms)), lifecycle) => match value {
                Ok(result) => result,
                Err(_) => Err("promptGen timed out".to_owned()),
            }
        };
        let cleanup = cleanup_process_group(&mut group.child, identity).await;
        let report = cleanup.map_err(|error| InvocationError {
            dispatched,
            message: format!("promptGen cleanup unproven: {error}"),
        })?;
        let (stdout, stderr) = result.map_err(|message| InvocationError {
            dispatched,
            message,
        })?;
        if !report.status.success() {
            // Never forward raw child diagnostics: they may contain user input,
            // provider messages or a credential. Preserve only their digest.
            return Err(InvocationError {
                dispatched,
                message: format!(
                    "promptGen exited {}; stderrSha256={}",
                    report.status,
                    prompt_content_sha256(&stderr)
                ),
            });
        }
        Ok(stdout)
    }
}

fn take_image_prompt(outcome: &mut CompilationOutcome, stage: &str) -> Result<String, String> {
    outcome.validate()?;
    if outcome.status == CompilationStatus::Invalid {
        return Err(format!(
            "promptGen image {stage} did not produce a valid image compilation"
        ));
    }
    let prompt = outcome
        .prompt
        .take()
        .ok_or_else(|| format!("promptGen image {stage} returned no prompt"))?;
    if prompt.trim().is_empty()
        || prompt.len() > agent_core::MAX_COMPILED_PROMPT_BYTES
        || prompt.contains('\0')
    {
        return Err(format!(
            "promptGen image {stage} prompt is empty or exceeds bounds"
        ));
    }
    Ok(prompt)
}

fn compiled_image_from_outcome(
    input: &str,
    encoded: &[u8],
    mut outcome: CompilationOutcome,
) -> Result<CompiledImagePrompt, String> {
    let prompt = take_image_prompt(&mut outcome, "compile")?;
    let request = outcome
        .metadata
        .get("request")
        .ok_or("promptGen compilation metadata has no normalized request")?;
    let request_object = request
        .as_object()
        .ok_or("promptGen normalized request is not an object")?;
    if request_object.get("task_mode").and_then(JsonValue::as_str) != Some("generate") {
        return Err("promptgen_compile accepts generate-mode image requests only; use image_edit_semantic for edit effects".to_owned());
    }
    if request_object
        .get("references")
        .and_then(JsonValue::as_array)
        .is_some_and(|references| !references.is_empty())
    {
        return Err(
            "promptgen_compile generation artifacts cannot require reference images".to_owned(),
        );
    }
    let output = request_object
        .get("output")
        .and_then(JsonValue::as_object)
        .ok_or("promptGen normalized request has no output contract")?;
    if output.get("backend").and_then(JsonValue::as_str) != Some("codex-subscription")
        || output.get("format").and_then(JsonValue::as_str) != Some("png")
    {
        return Err(
            "promptGen output contract is unsupported by the selected image tool".to_owned(),
        );
    }
    let width = u32::try_from(
        output
            .get("width")
            .and_then(JsonValue::as_u64)
            .ok_or("promptGen output width is missing")?,
    )
    .map_err(|error| error.to_string())?;
    let height = u32::try_from(
        output
            .get("height")
            .and_then(JsonValue::as_u64)
            .ok_or("promptGen output height is missing")?,
    )
    .map_err(|error| error.to_string())?;
    let background = serde_json::from_value::<CompiledImageBackground>(serde_json::Value::String(
        output
            .get("background")
            .and_then(JsonValue::as_str)
            .ok_or("promptGen output background is missing")?
            .to_owned(),
    ))
    .map_err(|error| format!("invalid promptGen output background: {error}"))?;
    let detail = serde_json::from_value::<CompiledImageDetail>(serde_json::Value::String(
        output
            .get("detail")
            .and_then(JsonValue::as_str)
            .ok_or("promptGen output detail is missing")?
            .to_owned(),
    ))
    .map_err(|error| format!("invalid promptGen output detail: {error}"))?;
    let status = outcome
        .status
        .successful()
        .ok_or("promptGen compilation did not succeed")?;
    let normalized = serde_json::to_vec(request).map_err(|error| error.to_string())?;
    let compiled = CompiledImagePrompt {
        status,
        input_sha256: prompt_content_sha256(input.as_bytes()),
        normalized_input_sha256: prompt_content_sha256(&normalized),
        compiler_outcome_sha256: prompt_content_sha256(encoded),
        prompt_sha256: prompt_content_sha256(prompt.as_bytes()),
        prompt,
        output: CompiledImageOutput {
            width,
            height,
            background,
            detail,
        },
    };
    compiled.validate()?;
    Ok(compiled)
}

fn validate_input(input: &str) -> Result<(), String> {
    if input.trim().is_empty() || input.len() > MAX_COMPILE_INPUT_BYTES || input.contains('\0') {
        return Err("promptGen input must be non-empty and bounded".to_owned());
    }
    Ok(())
}
fn validate_executable(path: &Path, sha256: &str) -> Result<(), String> {
    verify_executable_identity(
        path,
        sha256,
        MAX_EXECUTABLE_BYTES as u64,
        "promptGen executable",
    )
}

fn verify_executable_identity(
    path: &Path,
    expected_sha256: &str,
    maximum_bytes: u64,
    label: &str,
) -> Result<(), String> {
    if expected_sha256.len() != 64
        || !expected_sha256
            .bytes()
            .all(|value| value.is_ascii_hexdigit())
    {
        return Err(format!("invalid {label} SHA-256 identity"));
    }
    let before = std::fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    if !before.is_file() || before.file_type().is_symlink() {
        return Err(format!("{label} must be a regular non-symlink file"));
    }
    if before.len() == 0 || before.len() > maximum_bytes {
        return Err(format!("{label} size is outside its configured bound"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if before.permissions().mode() & 0o111 == 0 {
            return Err(format!("{label} is not executable"));
        }
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(
            (rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK)
                .bits()
                .cast_signed(),
        );
    }
    let file = options.open(path).map_err(|error| error.to_string())?;
    let opened = file.metadata().map_err(|error| error.to_string())?;
    if !opened.is_file() || opened.len() != before.len() {
        return Err(format!("{label} changed while opening"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if before.dev() != opened.dev() || before.ino() != opened.ino() {
            return Err(format!("{label} identity changed while opening"));
        }
    }
    let mut input = file.take(maximum_bytes + 1);
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 64 * 1024].into_boxed_slice();
    let mut total = 0_u64;
    loop {
        let count = input.read(&mut buffer).map_err(|error| error.to_string())?;
        if count == 0 {
            break;
        }
        total = total
            .checked_add(count as u64)
            .ok_or_else(|| format!("{label} size overflow"))?;
        if total > maximum_bytes {
            return Err(format!("{label} exceeds its configured byte bound"));
        }
        hasher.update(&buffer[..count]);
    }
    if total != opened.len() {
        return Err(format!("{label} changed while hashing"));
    }
    let after = std::fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    if !after.is_file()
        || after.file_type().is_symlink()
        || after.len() != opened.len()
        || after.modified().ok() != opened.modified().ok()
    {
        return Err(format!("{label} changed during identity verification"));
    }
    let actual_sha256 = format!("{:x}", hasher.finalize());
    if !actual_sha256.eq_ignore_ascii_case(expected_sha256) {
        return Err(format!("{label} digest mismatch"));
    }
    Ok(())
}

pub(crate) fn read_regular_bounded(path: &Path, maximum: usize) -> Result<Vec<u8>, String> {
    let metadata = std::fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err("evidence must be a regular non-symlink file".to_owned());
    }
    let mut options = File::options();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(
            (rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK)
                .bits()
                .cast_signed(),
        );
    }
    let file = options.open(path).map_err(|error| error.to_string())?;
    let metadata = file.metadata().map_err(|error| error.to_string())?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > maximum as u64 {
        return Err("evidence is empty, non-regular or exceeds its byte bound".to_owned());
    }
    let mut bytes = Vec::new();
    file.take(maximum as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.is_empty() || bytes.len() > maximum {
        return Err("evidence changed outside its byte bound".to_owned());
    }
    Ok(bytes)
}
async fn bounded_pipe(reader: impl AsyncRead + Unpin, maximum: usize) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    reader
        .take(maximum as u64 + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|error| error.to_string())?;
    if bytes.len() > maximum {
        return Err("promptGen process output exceeds its byte bound".to_owned());
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_contract::compilation::{Diagnostic, PromptKind};
    use agent_core::PromptGenCodexRuntimeSpec;

    fn prompt_outcome(prompt: Option<String>, diagnostics: Vec<Diagnostic>) -> CompilationOutcome {
        CompilationOutcome::new(
            PromptKind::Image,
            prompt,
            diagnostics,
            JsonValue::object([] as [(&str, JsonValue); 0]),
        )
    }

    #[test]
    fn image_prompt_accepts_valid_and_warning_results() {
        for stage in ["compile", "preflight"] {
            for diagnostics in [
                Vec::new(),
                vec![Diagnostic::warning(
                    "fixture-warning",
                    "$",
                    "fixture warning",
                )],
            ] {
                let mut outcome = prompt_outcome(Some("image prompt".to_owned()), diagnostics);
                assert_eq!(
                    take_image_prompt(&mut outcome, stage),
                    Ok("image prompt".to_owned())
                );
            }
            let mut outcome = prompt_outcome(
                Some("a".repeat(agent_core::MAX_COMPILED_PROMPT_BYTES)),
                Vec::new(),
            );
            assert!(take_image_prompt(&mut outcome, stage).is_ok());
        }
    }

    #[test]
    fn image_prompt_rejects_missing_or_out_of_bounds_prompts_with_stage_errors() {
        for stage in ["compile", "preflight"] {
            let mut missing = prompt_outcome(None, Vec::new());
            assert_eq!(
                take_image_prompt(&mut missing, stage),
                Err(format!("promptGen image {stage} returned no prompt"))
            );
            for prompt in [
                String::new(),
                " \n".to_owned(),
                "image\0prompt".to_owned(),
                "a".repeat(agent_core::MAX_COMPILED_PROMPT_BYTES + 1),
            ] {
                let mut outcome = prompt_outcome(Some(prompt), Vec::new());
                assert_eq!(
                    take_image_prompt(&mut outcome, stage),
                    Err(format!(
                        "promptGen image {stage} prompt is empty or exceeds bounds"
                    ))
                );
            }
        }
    }

    #[test]
    fn image_prompt_preserves_invalid_and_inconsistent_outcome_errors() {
        for stage in ["compile", "preflight"] {
            let mut invalid = prompt_outcome(
                None,
                vec![Diagnostic::error("fixture-error", "$", "fixture error")],
            );
            assert_eq!(
                take_image_prompt(&mut invalid, stage),
                Err(format!(
                    "promptGen image {stage} did not produce a valid image compilation"
                ))
            );
            invalid.status = CompilationStatus::Valid;
            assert_eq!(
                take_image_prompt(&mut invalid, stage),
                Err("compilation status disagrees with its diagnostic evidence".to_owned())
            );
            let mut invalid_metadata = prompt_outcome(Some("image prompt".to_owned()), Vec::new());
            invalid_metadata.metadata = JsonValue::Null;
            assert_eq!(
                take_image_prompt(&mut invalid_metadata, stage),
                Err("compilation metadata must be an object".to_owned())
            );
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn prompt_only_callers_preserve_edit_preflight_and_generate_artifact_contracts()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::PermissionsExt;

        for mode in ["edit", "generate"] {
            let root = tempfile::tempdir()?;
            let work = root.path().canonicalize()?;
            let outcome = serde_json::json!({
                "kind": "image",
                "status": "valid",
                "prompt": "image prompt",
                "diagnostics": [],
                "metadata": {
                    "request": {
                        "task_mode": mode,
                        "references": [],
                        "output": {
                            "backend": "codex-subscription",
                            "format": "png",
                            "width": 1024,
                            "height": 1024,
                            "detail": "medium",
                            "background": "opaque"
                        }
                    }
                }
            });
            let encoded = serde_json::to_string_pretty(&outcome)?;
            // Process/argv fixture only: it never invokes an image provider.
            let script = format!(
                "#!/bin/sh\nset -eu\n[ \"$#\" -eq 7 ]\n[ \"$1\" = image ]\n[ \"$2\" = --input ]\n[ \"$(cat \"$3\")\" = '{{}}' ]\n[ \"$PWD\" = \"$(dirname \"$3\")\" ]\n[ \"$4\" = --mode ]\n[ \"$5\" = prompt-only ]\n[ \"$6\" = --format ]\n[ \"$7\" = json ]\ncat <<'OUTCOME'\n{encoded}\nOUTCOME\n"
            );
            let executable = work.join("promptgen-fixture");
            std::fs::write(&executable, &script)?;
            std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700))?;
            let adapter = PromptGenAdapter::from_spec(&PromptGenSpec {
                executable,
                executable_sha256: prompt_content_sha256(script.as_bytes()),
                timeout_ms: 1_000,
                codex_runtime: None,
            })?;
            let cancellation = CancellationToken::new();
            let input_path = work.join("edit.json");
            assert_eq!(
                adapter
                    .preflight_edit("{}", &input_path, 1_000, &cancellation)
                    .await?,
                "image prompt"
            );
            assert_eq!(std::fs::read_to_string(input_path)?, "{}");
            let compiled = adapter.compile_image("{}", 1_000, &cancellation).await;
            if mode == "edit" {
                assert_eq!(
                    compiled,
                    Err("promptgen_compile accepts generate-mode image requests only; use image_edit_semantic for edit effects".to_owned())
                );
            } else {
                let compiled = compiled?;
                assert_eq!(compiled.prompt, "image prompt");
                assert_eq!(
                    compiled.compiler_outcome_sha256,
                    prompt_content_sha256(format!("{encoded}\n").as_bytes())
                );
            }
        }
        Ok(())
    }

    #[test]
    fn image_compilation_binds_generate_request_prompt_and_delivery_intent() {
        let input = r#"{"task_mode":"generate","output":{"backend":"codex-subscription","format":"png","width":1536,"height":1024,"detail":"high","background":"opaque"}}"#;
        let outcome = CompilationOutcome {
            diagnostics: Vec::new(),
            kind: PromptKind::Image,
            metadata: serde_json::from_value(serde_json::json!({
                "request": {
                    "task_mode": "generate",
                    "references": [],
                    "output": {
                        "backend": "codex-subscription",
                        "format": "png",
                        "width": 1536,
                        "height": 1024,
                        "detail": "high",
                        "background": "opaque"
                    }
                }
            }))
            .expect("canonical metadata"),
            prompt: Some("compiled image prompt".to_owned()),
            status: CompilationStatus::Valid,
        };
        let encoded = serde_json::to_vec(&serde_json::json!({
            "kind":"image","status":"valid","prompt":"compiled image prompt","diagnostics":[],"metadata":outcome.metadata.clone()
        })).expect("outcome JSON");
        let compiled =
            compiled_image_from_outcome(input, &encoded, outcome).expect("compiled image");
        assert_eq!(
            compiled.output.provider_size().expect("provider size"),
            "1536x1024"
        );
        assert_eq!(compiled.output.detail, CompiledImageDetail::High);
        assert_eq!(
            compiled.input_sha256,
            prompt_content_sha256(input.as_bytes())
        );
        assert_eq!(
            compiled.compiler_outcome_sha256,
            prompt_content_sha256(&encoded)
        );
    }

    #[test]
    fn image_compilation_rejects_reference_or_edit_requests_before_artifact_creation() {
        for (mode, references) in [
            ("edit", serde_json::json!([])),
            ("generate", serde_json::json!([{"index":1,"role":"base"}])),
        ] {
            let outcome = CompilationOutcome {
                diagnostics: Vec::new(),
                kind: PromptKind::Image,
                metadata: serde_json::from_value(serde_json::json!({
                    "request": {
                        "task_mode": mode,
                        "references": references,
                        "output": {
                            "backend": "codex-subscription",
                            "format": "png",
                            "width": 1024,
                            "height": 1024,
                            "detail": "medium",
                            "background": "opaque"
                        }
                    }
                }))
                .expect("canonical metadata"),
                prompt: Some("prompt".to_owned()),
                status: CompilationStatus::Valid,
            };
            assert!(compiled_image_from_outcome("{}", b"{}", outcome).is_err());
        }
    }

    #[tokio::test]
    #[ignore = "requires built canonical PROMPTGEN_CONTRACT_BIN and PROMPTGEN_CONTRACT_INPUT; pure compile only"]
    async fn canonical_promptgen_binary_compiles_through_the_real_pinned_adapter()
    -> Result<(), Box<dyn std::error::Error>> {
        let executable = PathBuf::from(std::env::var("PROMPTGEN_CONTRACT_BIN")?).canonicalize()?;
        let input = std::fs::read_to_string(std::env::var("PROMPTGEN_CONTRACT_INPUT")?)?;
        let binary_bytes = read_regular_bounded(&executable, MAX_EXECUTABLE_BYTES)?;
        let adapter = PromptGenAdapter::from_spec(&PromptGenSpec {
            executable,
            executable_sha256: prompt_content_sha256(&binary_bytes),
            timeout_ms: 30_000,
            codex_runtime: None,
        })?;
        let compiled = adapter
            .compile_image(&input, 30_000, &CancellationToken::new())
            .await?;
        assert_ne!(compiled.prompt.trim(), "");
        assert_eq!(
            compiled.input_sha256,
            prompt_content_sha256(input.as_bytes())
        );
        let request: serde_json::Value = serde_json::from_str(&input)?;
        assert_eq!(
            u64::from(compiled.output.width),
            request["output"]["width"].as_u64().ok_or("input width")?
        );
        assert_eq!(
            u64::from(compiled.output.height),
            request["output"]["height"].as_u64().ok_or("input height")?
        );
        assert!(compiled.output.provider_size().is_ok());
        Ok(())
    }

    #[test]
    fn evidence_requires_nonempty_bounded_regular_file() -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        let path = root.path().join("evidence");
        std::fs::write(&path, b"abcd")?;
        assert_eq!(read_regular_bounded(&path, 4)?, b"abcd");
        assert!(read_regular_bounded(&path, 3).is_err());
        std::fs::write(&path, b"")?;
        assert!(read_regular_bounded(&path, 4).is_err());
        assert!(read_regular_bounded(root.path(), 4).is_err());
        assert!(read_regular_bounded(&root.path().join("missing"), 4).is_err());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn evidence_and_compiler_identity_reject_symlinks_and_binary_drift()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let root = tempfile::tempdir()?;
        let path = root.path().join("compiler-test-shim");
        let bytes = b"#!/bin/sh\nexit 0\n";
        std::fs::write(&path, bytes)?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))?;
        let link = root.path().join("symlink");
        symlink(&path, &link)?;
        assert!(read_regular_bounded(&link, 1024).is_err());
        let spec = PromptGenSpec {
            executable: path.clone(),
            executable_sha256: prompt_content_sha256(bytes),
            timeout_ms: 1000,
            codex_runtime: None,
        };
        assert!(PromptGenAdapter::from_spec(&spec).is_ok());
        assert!(
            PromptGenAdapter::from_spec(&PromptGenSpec {
                executable: link,
                ..spec.clone()
            })
            .is_err()
        );
        std::fs::write(&path, b"#!/bin/sh\nexit 1\n")?;
        assert!(validate_executable(&path, &spec.executable_sha256).is_err());
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn semantic_dispatch_pins_codex_and_does_not_forward_api_keys()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::PermissionsExt;

        let root = tempfile::tempdir()?;
        let args_log = root.path().join("args.log");
        let promptgen = root.path().join("promptgen-fixture");
        let promptgen_bytes = format!(
            "#!/bin/sh\nset -eu\nprintf '%s\\n' \"$@\" > '{}'\n[ -z \"${{OPENAI_API_KEY:-}}\" ]\n",
            args_log.display()
        );
        std::fs::write(&promptgen, &promptgen_bytes)?;
        std::fs::set_permissions(&promptgen, std::fs::Permissions::from_mode(0o700))?;
        let codex = root.path().join("codex-fixture");
        let codex_bytes = b"#!/bin/sh\nexit 0\n";
        std::fs::write(&codex, codex_bytes)?;
        std::fs::set_permissions(&codex, std::fs::Permissions::from_mode(0o700))?;
        let codex_sha256 = prompt_content_sha256(codex_bytes);
        let spec = PromptGenSpec {
            executable: promptgen.clone(),
            executable_sha256: prompt_content_sha256(promptgen_bytes.as_bytes()),
            timeout_ms: 1_000,
            codex_runtime: Some(PromptGenCodexRuntimeSpec {
                executable: codex.clone(),
                executable_sha256: codex_sha256.clone(),
                home: root.path().join("codex-home"),
            }),
        };
        let adapter = PromptGenAdapter::from_spec(&spec)?;
        assert_eq!(adapter.codex_sha256(), Some(codex_sha256.as_str()));

        let input = root.path().join("request.json");
        let source = root.path().join("source.png");
        let output = root.path().join("edited.png");
        let receipt = root.path().join("receipt.json");
        let token = CancellationToken::new();
        adapter
            .dispatch_edit(
                EditPaths {
                    input: &input,
                    source: &source,
                    output: &output,
                    receipt: &receipt,
                },
                1_000,
                &token,
            )
            .await
            .expect("fixture PromptGen dispatch should succeed");

        let args = std::fs::read_to_string(args_log)?;
        let canonical_codex = codex.canonicalize()?;
        assert!(args.contains("--mode\ncodex-imagegen\n"));
        assert!(args.contains("--max-fidelity-attempts\n1\n"));
        assert!(
            args.contains(&format!("--codex-binary\n{}\n", canonical_codex.display())),
            "unexpected PromptGen arguments: {args:?}"
        );
        assert!(args.contains(&format!("--codex-sha256\n{codex_sha256}\n")));
        assert!(args.contains(&format!(
            "--codex-home\n{}\n",
            root.path().join("codex-home").display()
        )));
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cancelled_before_spawn_is_not_external_dispatch()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir()?;
        let path = root.path().join("compiler-test-shim");
        let bytes = b"#!/bin/sh\nexit 0\n";
        std::fs::write(&path, bytes)?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))?;
        let adapter = PromptGenAdapter::from_spec(&PromptGenSpec {
            executable: path,
            executable_sha256: prompt_content_sha256(bytes),
            timeout_ms: 1000,
            codex_runtime: None,
        })?;
        let token = CancellationToken::new();
        token.cancel();
        let result = adapter
            .invoke(Vec::new(), root.path(), 1000, &token, true)
            .await;
        assert!(matches!(
            result,
            Err(InvocationError {
                dispatched: false,
                ..
            })
        ));
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn child_timeout_after_spawn_is_external_uncertainty()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir()?;
        let path = root.path().join("compiler-test-shim");
        // A process-lifecycle fixture only. It never calls an image provider.
        let bytes = b"#!/bin/sh\nsleep 10\n";
        std::fs::write(&path, bytes)?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))?;
        let adapter = PromptGenAdapter::from_spec(&PromptGenSpec {
            executable: path,
            executable_sha256: prompt_content_sha256(bytes),
            timeout_ms: 50,
            codex_runtime: None,
        })?;
        let result = adapter
            .invoke(Vec::new(), root.path(), 50, &CancellationToken::new(), true)
            .await;
        assert!(matches!(
            result,
            Err(InvocationError {
                dispatched: true,
                ..
            })
        ));
        Ok(())
    }
}
