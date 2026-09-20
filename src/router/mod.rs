use crate::state::Route;
use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

#[derive(Debug, Clone)]
pub struct RouteResult {
    pub route: Route,
    pub elapsed_ms: u128,
}

#[derive(Debug, Clone)]
pub struct InferenceResult {
    pub transcript: String,
    pub asr_ms: u128,
    pub route: RouteResult,
    pub shadow_warning: Option<String>,
}

/// The resident bridge to the exact research setup: mlx-whisper large-v3-turbo
/// plus the `scaling_run/checkpoints/pool_300` Kev checkpoint. This is a narrow
/// MVP seam, not a generic model-hosting protocol.
pub struct KevWorker {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
}

impl KevWorker {
    pub fn start() -> Result<Self> {
        let research = research_root()?;
        let python = research.join(".venv-kev/bin/python");
        let script = research.join("scripts/localflow_worker.py");
        if !python.is_file() || !script.is_file() {
            return Err(anyhow!(
                "LocalFlow research runtime is unavailable at {}",
                research.display()
            ));
        }

        let mut child = Command::new(python)
            .arg(script)
            .current_dir(&research)
            .env("HF_HUB_OFFLINE", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .context("Could not start the resident pool_300 inference worker")?;
        let input = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("Inference worker stdin unavailable"))?;
        let mut output = BufReader::new(
            child
                .stdout
                .take()
                .ok_or_else(|| anyhow!("Inference worker stdout unavailable"))?,
        );

        #[derive(Deserialize)]
        struct Ready {
            ready: bool,
            error: Option<String>,
        }
        let mut line = String::new();
        if output.read_line(&mut line)? == 0 {
            return Err(anyhow!("Inference worker exited before becoming ready"));
        }
        let ready: Ready =
            serde_json::from_str(&line).context("Inference worker sent invalid startup JSON")?;
        if !ready.ready {
            return Err(anyhow!(
                "Inference worker failed to load: {}",
                ready.error.unwrap_or_else(|| "unknown error".to_owned())
            ));
        }
        Ok(Self {
            child,
            input,
            output,
        })
    }

    pub fn transcribe_and_route(&mut self, audio_path: &Path) -> Result<InferenceResult> {
        #[derive(Serialize)]
        struct Request<'a> {
            audio_path: &'a str,
        }
        #[derive(Deserialize)]
        struct Response {
            transcript: Option<String>,
            asr_ms: Option<f64>,
            route: Option<Route>,
            router_ms: Option<f64>,
            shadow_error: Option<String>,
            error: Option<String>,
        }

        let path = audio_path
            .to_str()
            .ok_or_else(|| anyhow!("Audio path is not valid UTF-8"))?;
        serde_json::to_writer(&mut self.input, &Request { audio_path: path })?;
        self.input.write_all(b"\n")?;
        self.input.flush()?;
        let mut line = String::new();
        if self.output.read_line(&mut line)? == 0 {
            return Err(anyhow!("Inference worker closed its output"));
        }
        let response: Response =
            serde_json::from_str(&line).context("Inference worker sent invalid JSON")?;
        if let Some(error) = response.error {
            return Err(anyhow!(error));
        }
        Ok(InferenceResult {
            transcript: response
                .transcript
                .ok_or_else(|| anyhow!("Inference worker omitted transcript"))?,
            asr_ms: response
                .asr_ms
                .ok_or_else(|| anyhow!("Inference worker omitted ASR latency"))?
                .round() as u128,
            route: RouteResult {
                route: response
                    .route
                    .ok_or_else(|| anyhow!("Inference worker omitted route"))?,
                elapsed_ms: response
                    .router_ms
                    .ok_or_else(|| anyhow!("Inference worker omitted router latency"))?
                    .round() as u128,
            },
            shadow_warning: response.shadow_error,
        })
    }
}

impl Drop for KevWorker {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

fn research_root() -> Result<PathBuf> {
    let app_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let parent = app_root
        .parent()
        .ok_or_else(|| anyhow!("LocalFlow has no parent directory"))?;
    Ok(parent.join("localflow-research"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kev_route_names_match_the_worker_protocol() {
        assert_eq!(
            serde_json::from_str::<Route>("\"LIGHT_CLEANUP\"").unwrap(),
            Route::LightCleanup
        );
        assert_eq!(
            serde_json::to_string(&Route::PassThrough).unwrap(),
            "\"PASS_THROUGH\""
        );
    }
}
