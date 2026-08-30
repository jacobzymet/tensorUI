//! Local llama-server management: detect binary, list HF cache, spawn mmap CPU runs.

use std::{
    env, fs,
    net::TcpListener,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, UNIX_EPOCH},
};

use serde::Serialize;

const DEFAULT_PORT: u16 = 8080;
const PROVIDER_ID: &str = "local-llama-server";
const PROVIDER_NAME: &str = "Local LLM";

#[derive(Debug, Clone, Serialize)]
pub struct LlamaServerInstall {
    pub installed: bool,
    pub path: Option<String>,
    pub version: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CachedModel {
    pub path: String,
    pub name: String,
    pub size_bytes: u64,
    pub modified_at: Option<u64>,
    /// Best-effort `org/repo:quant` guess from the filename, when recognizable.
    pub hf_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RunningLocalLlm {
    pub pid: u32,
    pub port: u16,
    pub host: String,
    pub base_url: String,
    pub model: String,
    pub mmap: bool,
    pub threads: u32,
    pub command: String,
}

#[derive(Default)]
pub struct LocalLlmManager {
    child: Option<Child>,
    meta: Option<RunningLocalLlm>,
}

impl std::fmt::Debug for LocalLlmManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalLlmManager")
            .field("running", &self.meta)
            .finish()
    }
}

impl LocalLlmManager {
    pub fn status(&mut self) -> Option<RunningLocalLlm> {
        self.reap_if_exited();
        self.meta.clone()
    }

    pub fn stop(&mut self) -> Result<(), String> {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        self.meta = None;
        Ok(())
    }

    fn reap_if_exited(&mut self) {
        let dead = match self.child.as_mut() {
            Some(child) => matches!(child.try_wait(), Ok(Some(_))),
            None => {
                self.meta = None;
                return;
            }
        };
        if dead {
            self.child = None;
            self.meta = None;
        }
    }
}

pub fn detect_llama_server() -> LlamaServerInstall {
    match find_llama_server() {
        Ok(path) => {
            let version = probe_version(&path);
            LlamaServerInstall {
                installed: true,
                path: Some(path.display().to_string()),
                version,
                error: None,
            }
        }
        Err(error) => LlamaServerInstall {
            installed: false,
            path: None,
            version: None,
            error: Some(error),
        },
    }
}

pub fn list_cached_models() -> Result<Vec<CachedModel>, String> {
    let root = llama_cache_dir()?;
    if !root.is_dir() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    collect_gguf(&root, &root, 0, &mut out)?;
    out.sort_by(|a, b| {
        b.modified_at
            .unwrap_or(0)
            .cmp(&a.modified_at.unwrap_or(0))
            .then_with(|| {
                a.name
                    .to_ascii_lowercase()
                    .cmp(&b.name.to_ascii_lowercase())
            })
    });
    Ok(out)
}

pub struct StartLocalLlm {
    pub hf: Option<String>,
    pub model_path: Option<String>,
    pub mmap: bool,
    pub port: Option<u16>,
    pub threads: Option<u32>,
    pub host: Option<String>,
}

pub fn start_local_llm(
    manager: &mut LocalLlmManager,
    req: StartLocalLlm,
) -> Result<RunningLocalLlm, String> {
    manager.reap_if_exited();
    if manager.meta.is_some() {
        return Err("A local LLM is already running. Stop it first.".into());
    }

    let bin = find_llama_server()?;
    let host = req
        .host
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("127.0.0.1")
        .to_string();
    let port = pick_port(req.port.unwrap_or(DEFAULT_PORT))?;
    let threads = req
        .threads
        .filter(|n| *n > 0)
        .unwrap_or_else(default_threads);

    let (model_label, model_args) =
        resolve_model_args(req.hf.as_deref(), req.model_path.as_deref())?;
    let args = build_args(&model_args, &host, port, threads, req.mmap);
    let command = format_command(&bin, &args);

    let mut child = Command::new(&bin)
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("Could not start llama-server: {error}"))?;

    let pid = child.id();
    let base_url = format!("http://{host}:{port}/v1");

    // Give the process a moment; full model load can take minutes — UI will poll health.
    std::thread::sleep(Duration::from_millis(400));
    if let Ok(Some(status)) = child.try_wait() {
        return Err(format!(
            "llama-server exited immediately ({status}). Check the model id/path and that the port is free."
        ));
    }

    let meta = RunningLocalLlm {
        pid,
        port,
        host,
        base_url,
        model: model_label,
        mmap: req.mmap,
        threads,
        command,
    };
    manager.child = Some(child);
    manager.meta = Some(meta.clone());
    Ok(meta)
}

pub fn provider_id() -> &'static str {
    PROVIDER_ID
}

pub fn provider_name() -> &'static str {
    PROVIDER_NAME
}

fn resolve_model_args(
    hf: Option<&str>,
    model_path: Option<&str>,
) -> Result<(String, Vec<String>), String> {
    let hf = hf.map(str::trim).filter(|s| !s.is_empty());
    let model_path = model_path.map(str::trim).filter(|s| !s.is_empty());
    match (hf, model_path) {
        (Some(hf), _) => {
            let id = normalize_hf_id(hf)?;
            Ok((id.clone(), vec!["-hf".into(), id]))
        }
        (None, Some(path)) => {
            let p = PathBuf::from(path);
            if !p.is_file() {
                return Err(format!("Model file not found: {path}"));
            }
            let label = p
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or(path)
                .to_string();
            Ok((label, vec!["-m".into(), p.display().to_string()]))
        }
        (None, None) => {
            Err("Provide a Hugging Face model (org/repo:quant) or choose a cached GGUF.".into())
        }
    }
}

/// Accept `org/repo`, `org/repo:QUANT`, optional `hf.co/` prefix.
pub fn normalize_hf_id(raw: &str) -> Result<String, String> {
    let mut s = raw.trim().to_string();
    for prefix in [
        "https://huggingface.co/",
        "http://huggingface.co/",
        "huggingface.co/",
        "hf.co/",
    ] {
        if let Some(rest) = s.strip_prefix(prefix) {
            s = rest.to_string();
            break;
        }
    }
    s = s.trim_matches('/').to_string();
    if let Some((repo, _)) = s.split_once("/tree/") {
        s = repo.to_string();
    }
    if let Some((repo, _)) = s.split_once("/blob/") {
        s = repo.to_string();
    }
    let (repo, quant) = match s.split_once(':') {
        Some((repo, quant)) => (repo.trim(), Some(quant.trim())),
        None => (s.as_str(), None),
    };
    let parts: Vec<_> = repo.split('/').filter(|p| !p.is_empty()).collect();
    if parts.len() != 2
        || parts
            .iter()
            .any(|p| p.contains([' ', '\\', '\t', '\n']) || *p == "." || *p == "..")
    {
        return Err(
            "Use Hugging Face form org/repo or org/repo:QUANT (example: ggml-org/gpt-oss-120b-GGUF:Q4_K_M)."
                .into(),
        );
    }
    let mut out = format!("{}/{}", parts[0], parts[1]);
    if let Some(q) = quant {
        if q.is_empty() || q.contains(['/', ' ', '\\']) {
            return Err("Quant after ':' looks invalid (example: Q4_K_M).".into());
        }
        out.push(':');
        out.push_str(q);
    }
    Ok(out)
}

fn build_args(
    model_args: &[String],
    host: &str,
    port: u16,
    threads: u32,
    mmap: bool,
) -> Vec<String> {
    let mut args: Vec<String> = model_args.to_vec();
    if mmap {
        // High RAM-efficiency CPU mmap profile (user-specified).
        args.extend(
            [
                "--device",
                "none",
                "--n-gpu-layers",
                "0",
                "--fit",
                "off",
                "--mmap",
                "--no-repack",
                "--warmup",
                "--flash-attn",
                "on",
                "--cache-type-k",
                "q8_0",
                "--cache-type-v",
                "q8_0",
                "--ctx-size",
                "8192",
                "--batch-size",
                "8",
                "--ubatch-size",
                "8",
                "--parallel",
                "1",
                "--cache-ram",
                "0",
                "--ctx-checkpoints",
                "0",
                "--no-mmproj",
                "--jinja",
            ]
            .into_iter()
            .map(str::to_string),
        );
    } else {
        args.extend(
            ["--ctx-size", "8192", "--jinja", "--flash-attn", "on"]
                .into_iter()
                .map(str::to_string),
        );
    }
    args.extend([
        "--threads".into(),
        threads.to_string(),
        "--metrics".into(),
        "--host".into(),
        host.into(),
        "--port".into(),
        port.to_string(),
    ]);
    args
}

fn format_command(bin: &Path, args: &[String]) -> String {
    let mut parts = vec![shell_quote(&bin.display().to_string())];
    for arg in args {
        parts.push(shell_quote(arg));
    }
    parts.join(" ")
}

fn shell_quote(value: &str) -> String {
    if value.is_empty() {
        return "\"\"".into();
    }
    if value
        .chars()
        .any(|c| c.is_whitespace() || matches!(c, '"' | '\'' | '\\'))
    {
        format!("\"{}\"", value.replace('\"', "\\\""))
    } else {
        value.to_string()
    }
}

fn default_threads() -> u32 {
    std::thread::available_parallelism()
        .map(|n| n.get() as u32)
        .unwrap_or(4)
        .saturating_sub(2)
        .max(1)
}

fn pick_port(preferred: u16) -> Result<u16, String> {
    if preferred == 0 {
        return Err("Port must be between 1 and 65535.".into());
    }
    if port_free(preferred) {
        return Ok(preferred);
    }
    for candidate in preferred.saturating_add(1)..=preferred.saturating_add(19) {
        if candidate == preferred {
            continue;
        }
        if port_free(candidate) {
            return Ok(candidate);
        }
    }
    Err(format!(
        "Port {preferred} (and the next few) are in use. Stop the other process or pick another port."
    ))
}

fn port_free(port: u16) -> bool {
    TcpListener::bind(("127.0.0.1", port)).is_ok()
}

fn find_llama_server() -> Result<PathBuf, String> {
    if let Ok(custom) = env::var("TENSORUI_LLAMA_SERVER") {
        let path = PathBuf::from(custom.trim());
        if is_executable(&path) {
            return Ok(path);
        }
        return Err(format!(
            "TENSORUI_LLAMA_SERVER is set but not executable: {}",
            path.display()
        ));
    }

    if let Some(path) = which("llama-server") {
        return Ok(path);
    }
    // Windows builds sometimes ship as llama-server.exe via `where`.
    if let Some(path) = which("llama-server.exe") {
        return Ok(path);
    }

    for candidate in common_install_paths() {
        if is_executable(&candidate) {
            return Ok(candidate);
        }
    }

    Err(
        "llama-server not found on PATH. Install llama.cpp and ensure `llama-server` is available, or set TENSORUI_LLAMA_SERVER to the binary."
            .into(),
    )
}

fn common_install_paths() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(home) = env::var_os("HOME").or_else(|| env::var_os("USERPROFILE")) {
        let home = PathBuf::from(home);
        out.push(home.join("llama.cpp/build/bin/llama-server"));
        out.push(home.join("llama.cpp/build/bin/Release/llama-server.exe"));
        out.push(home.join("llama.cpp/build/bin/llama-server.exe"));
        out.push(home.join(".local/bin/llama-server"));
    }
    if let Some(local) = env::var_os("LOCALAPPDATA") {
        let local = PathBuf::from(local);
        out.push(local.join("llama.cpp/llama-server.exe"));
        out.push(local.join("Programs/llama.cpp/llama-server.exe"));
    }
    #[cfg(target_os = "macos")]
    {
        out.push(PathBuf::from("/opt/homebrew/bin/llama-server"));
        out.push(PathBuf::from("/usr/local/bin/llama-server"));
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        out.push(PathBuf::from("/usr/local/bin/llama-server"));
        out.push(PathBuf::from("/usr/bin/llama-server"));
    }
    out
}

fn which(name: &str) -> Option<PathBuf> {
    #[cfg(windows)]
    let output = Command::new("where").arg(name).output().ok()?;
    #[cfg(not(windows))]
    let output = Command::new("which").arg(name).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let first = text.lines().next()?.trim();
    if first.is_empty() {
        return None;
    }
    let path = PathBuf::from(first);
    is_executable(&path).then_some(path)
}

fn is_executable(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }
    #[cfg(windows)]
    {
        true
    }
    #[cfg(not(windows))]
    {
        use std::os::unix::fs::PermissionsExt;
        path.metadata()
            .map(|m| m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
}

fn probe_version(path: &Path) -> Option<String> {
    let output = Command::new(path).arg("--version").output().ok()?;
    let text = if output.stdout.is_empty() {
        String::from_utf8_lossy(&output.stderr).to_string()
    } else {
        String::from_utf8_lossy(&output.stdout).to_string()
    };
    let line = text.lines().next()?.trim();
    if line.is_empty() {
        None
    } else {
        Some(line.chars().take(120).collect())
    }
}

pub fn llama_cache_dir() -> Result<PathBuf, String> {
    if let Ok(custom) = env::var("LLAMA_CACHE") {
        let path = PathBuf::from(custom.trim());
        if !path.as_os_str().is_empty() {
            return Ok(path);
        }
    }
    #[cfg(windows)]
    {
        let local = env::var("LOCALAPPDATA")
            .map_err(|_| "LOCALAPPDATA is not set; cannot locate llama.cpp cache.".to_string())?;
        Ok(PathBuf::from(local).join("llama.cpp"))
    }
    #[cfg(target_os = "macos")]
    {
        let home = env::var("HOME").map_err(|_| "HOME is not set".to_string())?;
        Ok(PathBuf::from(home).join("Library/Caches/llama.cpp"))
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        if let Ok(xdg) = env::var("XDG_CACHE_HOME") {
            return Ok(PathBuf::from(xdg).join("llama.cpp"));
        }
        let home = env::var("HOME").map_err(|_| "HOME is not set".to_string())?;
        Ok(PathBuf::from(home).join(".cache/llama.cpp"))
    }
    #[cfg(not(any(windows, unix)))]
    {
        Err("Unsupported platform for llama.cpp cache discovery.".into())
    }
}

fn collect_gguf(
    root: &Path,
    dir: &Path,
    depth: usize,
    out: &mut Vec<CachedModel>,
) -> Result<(), String> {
    if depth > 6 || out.len() >= 200 {
        return Ok(());
    }
    let entries =
        fs::read_dir(dir).map_err(|error| format!("Could not read {}: {error}", dir.display()))?;
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            collect_gguf(root, &path, depth + 1, out)?;
            continue;
        }
        if !file_type.is_file() {
            continue;
        }
        let name = match path.file_name().and_then(|s| s.to_str()) {
            Some(name) if name.to_ascii_lowercase().ends_with(".gguf") => name.to_string(),
            _ => continue,
        };
        let meta = entry.metadata().ok();
        let size_bytes = meta.as_ref().map(|m| m.len()).unwrap_or(0);
        let modified_at = meta
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs());
        let hf_id = guess_hf_id(&name);
        out.push(CachedModel {
            path: path.display().to_string(),
            name,
            size_bytes,
            modified_at,
            hf_id,
        });
    }
    let _ = root;
    Ok(())
}

/// Best-effort reverse of llama.cpp HF cache naming into `org/repo:quant`.
fn guess_hf_id(filename: &str) -> Option<String> {
    let stem = filename
        .strip_suffix(".gguf")
        .or_else(|| filename.strip_suffix(".GGUF"))
        .unwrap_or(filename);
    // Patterns like: ggml-org_gpt-oss-120b-GGUF_Q4_K_M or owner_repo_file-Q4_K_M
    let quant_re = regex_lite_quant(stem)?;
    let (body, quant) = quant_re;
    let parts: Vec<&str> = body.split('_').collect();
    if parts.len() >= 2 {
        // owner_repo_rest → owner/repo:quant when rest looks like model name noise
        let owner = parts[0];
        let repo = parts[1];
        if !owner.is_empty() && !repo.is_empty() {
            return Some(format!("{owner}/{repo}:{quant}"));
        }
    }
    None
}

fn regex_lite_quant(stem: &str) -> Option<(&str, &str)> {
    // Split on last _ or - before a quant-looking token.
    const QUANTS: &[&str] = &[
        "Q2_K", "Q3_K_S", "Q3_K_M", "Q3_K_L", "Q4_0", "Q4_1", "Q4_K_S", "Q4_K_M", "Q5_0", "Q5_1",
        "Q5_K_S", "Q5_K_M", "Q6_K", "Q8_0", "IQ1_S", "IQ1_M", "IQ2_XXS", "IQ2_XS", "IQ2_S",
        "IQ2_M", "IQ3_XXS", "IQ3_XS", "IQ3_S", "IQ3_M", "IQ4_XS", "IQ4_NL", "F16", "F32", "BF16",
    ];
    let upper = stem.to_ascii_uppercase();
    for quant in QUANTS {
        let q = quant.to_ascii_uppercase();
        for sep in ['_', '-'] {
            let needle = format!("{sep}{q}");
            if let Some(idx) = upper.rfind(&needle)
                && idx + needle.len() == upper.len()
            {
                return Some((&stem[..idx], *quant));
            }
        }
        if upper == q {
            return Some(("", *quant));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_hf_ids() {
        assert_eq!(
            normalize_hf_id("ggml-org/gpt-oss-120b-GGUF:Q4_K_M").unwrap(),
            "ggml-org/gpt-oss-120b-GGUF:Q4_K_M"
        );
        assert_eq!(
            normalize_hf_id("https://huggingface.co/ggml-org/gpt-oss-120b-GGUF").unwrap(),
            "ggml-org/gpt-oss-120b-GGUF"
        );
        assert!(normalize_hf_id("not-a-repo").is_err());
    }

    #[test]
    fn rejects_ephemeral_port_zero() {
        assert!(pick_port(0).is_err());
    }

    #[test]
    fn mmap_args_include_profile_flags() {
        let args = build_args(
            &["-hf".into(), "org/model:Q4_K_M".into()],
            "127.0.0.1",
            8080,
            14,
            true,
        );
        assert!(args.iter().any(|a| a == "--mmap"));
        assert!(args.iter().any(|a| a == "--no-repack"));
        assert!(
            args.windows(2)
                .any(|w| w[0] == "--device" && w[1] == "none")
        );
        assert!(
            args.windows(2)
                .any(|w| w[0] == "--n-gpu-layers" && w[1] == "0")
        );
        assert!(args.windows(2).any(|w| w[0] == "--threads" && w[1] == "14"));
    }

    #[test]
    fn guesses_hf_from_cache_name() {
        assert_eq!(
            guess_hf_id("ggml-org_gpt-oss-120b-GGUF_Q4_K_M.gguf").as_deref(),
            Some("ggml-org/gpt-oss-120b-GGUF:Q4_K_M")
        );
    }
}
