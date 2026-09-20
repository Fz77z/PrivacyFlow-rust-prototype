# LocalFlow MVP

A macOS-first local dictation app. The current milestone is deliberately narrow:

> Hold **Right Option** → speak → release → mlx-whisper transcribes locally →
> the exact warmed `pool_300` Kev checkpoint selects `PASS_THROUGH` → text is
> pasted at the current cursor.

## What is wired now

LocalFlow uses the existing sibling `../localflow-research` project directly:

- ASR: `mlx-community/whisper-large-v3-turbo` through its existing `mlx-whisper` integration.
- Router: `experiments/scaling_run/checkpoints/pool_300`, with the same `v3_operational` prompt, fp32 MPS inference, LoRA adapter, and pointer head used by the scaling report.
- Research log: the existing `data/shadow/utterances.jsonl` and `data/shadow/BATCHES.json` machinery. It records transcript, Kev route/probabilities/latency, rules prediction, ASR metadata, provenance, and batch.

The resident Python process loads and warms both models once. It is a narrow MVP bridge, not a plugin system or final serving architecture.

## Run

The repositories must remain siblings:

```text
Desktop/
├── localflow/
└── localflow-research/
```

The research runtime and the already-downloaded Whisper model are used without configuration:

```sh
cargo run --release
```

Grant LocalFlow microphone, Accessibility, and Input Monitoring permissions when macOS asks. Then focus an editable application, hold Right Option while speaking, and release it.

The worker sets `HF_HUB_OFFLINE=1`; it does not download models or send dictated content over the network. Captured WAV audio is temporary and is removed after inference.

## Current scope

Only `PASS_THROUGH` is inserted. A non-PASS route is shown as an explicit error rather than being sent through the earlier placeholder cleanup logic. `LIGHT_CLEANUP`, `TRANSFORM`, and `COMPLEX` are intentionally deferred until their real local processor is connected.

Every successfully transcribed utterance is still written through the existing shadow collector before insertion. A burned batch is rejected before it can be appended; LocalFlow reports the collection problem without silently creating or contaminating an evaluation batch.

The debug window shows the ASR transcript, selected route, final output, and latency breakdown.

## Testing

See [TESTING.md](TESTING.md) for the intentionally small FAST, CONFIDENCE, and DEEP validation paths.
