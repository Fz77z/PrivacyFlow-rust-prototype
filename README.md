# LocalFlow MVP

A macOS-first local dictation app. The current milestone is deliberately narrow:

> Hold **Right Option** → speak → release → mlx-whisper transcribes locally →
> the warmed `pool_300` Kev checkpoint picks a route → the text is inserted
> raw, or rewritten locally by S1-mini first, at the current cursor.

## What is wired now

LocalFlow uses the existing sibling `../localflow-research` project directly:

- ASR: `mlx-community/whisper-large-v3-turbo` through its existing `mlx-whisper` integration.
- Router: `experiments/scaling_run/checkpoints/pool_300`, with the same `v3_operational` prompt, fp32 MPS inference, LoRA adapter, and pointer head used by the scaling report.
- Text processing: `superwhisper/s1-mini`, a 0.6B Qwen3 fine-tune that rewrites a raw ASR transcript as finished written text, run locally through transformers on MPS and kept resident beside the other two models.
- Research log: the existing `data/shadow/utterances.jsonl` and `data/shadow/BATCHES.json` machinery. It records transcript, Kev route/probabilities/latency, rules prediction, ASR metadata, provenance, and batch.

The resident Python process loads and warms all three models once. It is a narrow MVP bridge, not a plugin system or final serving architecture.

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

## Insertion behavior

Insertion puts the transcript on the system pasteboard and synthesizes Cmd-V.

The transcript is left on the pasteboard afterwards.
Restoring the previous clipboard on a timer raced the target application's asynchronous paste handling, which could insert the older clipboard contents instead of the dictated text.

LocalFlow records which application was frontmost when dictation started and refuses to insert if a different application is frontmost when the text is ready.

## Current scope

Kev stays strictly a router.
It answers what kind of processing a transcript needs, and never rewrites text itself.

`PASS_THROUGH` inserts the raw ASR transcript.
`LIGHT_CLEANUP` and `TRANSFORM` send the transcript through S1-mini and insert its rewrite.
They remain distinct routes for evaluation even though they currently share one processor.
`COMPLEX` is intentionally unimplemented and fails with an explicit error rather than falling back to the raw transcript.

A processor failure, or an empty rewrite, is reported as an error.
LocalFlow never quietly inserts the raw transcript when processing was supposed to happen.

Every successfully transcribed utterance is still written through the existing shadow collector before insertion. A burned batch is rejected before it can be appended; LocalFlow reports the collection problem without silently creating or contaminating an evaluation batch.

The debug window shows the ASR transcript, selected route, final output, and latency breakdown.

## Testing

See [TESTING.md](TESTING.md) for the intentionally small FAST, CONFIDENCE, and DEEP validation paths.
