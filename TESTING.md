# LocalFlow test strategy

The test suite is intentionally small. A test belongs here only when it protects an important state invariant, an externally observable behavior, a real regression, or a boundary contract.

Do not add tests just because a function or module was added. Before adding one, state the plausible incorrect behavior it would catch.

## Test levels

### FAST — normal edit loop

Run `cargo check`, then the affected focused test when one exists:

```sh
cargo check
cargo test router::tests
cargo test state::tests
```

These tests are pure and do not start ASR, Kev, audio capture, a GUI, or a macOS event tap.

### CONFIDENCE — meaningful checkpoint

Run the normal suite once after a logical change set:

```sh
cargo test
```

It must remain fast and deterministic. Do not make real models, microphone access, global hotkeys, or external processes prerequisites for this command.

The S1-mini drift fixtures in `localflow-research/tests/fixtures/s1_rewrites.json` record what the shipped engine produces, not what is ideal.
Two entries carry a `known_issue` note because the frozen output is wrong and is frozen anyway; the test exists to notice change, not to certify correctness.
Regenerate them only after reading the diff.

When changing the real worker or shadow integration, also run LocalFlow research's own invariant suite:

```sh
(cd ../localflow-research && .venv/bin/python -m pytest -q)
```

The S1-mini tests load a real 0.6B model and are skipped unless asked for.
They need `.venv-kev`, which is the environment that has torch and transformers:

```sh
(cd ../localflow-research && LOCALFLOW_MODEL_TESTS=1 .venv-kev/bin/python -m pytest tests/test_normalizer.py -q)
```

Vendored model repositories are intentionally excluded from that command: they have independent environments and are DEEP validation, not LocalFlow's normal suite.

### DEEP VALIDATION — intentional manual acceptance

Run this only when changing platform integration, audio, ASR, model-worker integration, or release packaging:

```sh
cargo build --release
cargo run --release
```

Then focus a normal editable application, hold Right Option, dictate, release, and confirm that final text is inserted at its cursor. Check the console's Activity tab for ASR, route, output, and latency. When shadow collection is enabled, also confirm the local JSONL record has the intended existing batch identifier.

This requires microphone, Accessibility, and Input Monitoring permissions and must never run by default in CI or an agent edit loop.

Driving the worker end to end writes real records into `data/shadow/utterances.jsonl` and increments the batch manifest, because shadow collection is not conditional.
Synthetic or TTS audio used for testing therefore contaminates a log whose whole purpose is to be ground truth.
Point the worker at a throwaway log, or remove the records and correct the batch count afterwards.

## Future expensive tests

If a real-model, ASR, platform, or E2E test becomes necessary to prevent a concrete regression, mark it `#[ignore = "requires local model/platform setup"]`. Document its setup beside the test and run it explicitly with:

```sh
cargo test -- --ignored
```

Do not add broad mock stacks or benchmark/model initialization to the normal suite. Prefer testing pure parsing, state, research invariants, and worker protocol behavior at lower-cost boundaries.

## Protected behavior today

- Starting a new recording clears stale transcript, output, route, and error state.
- Kev’s JSONL route names match the application-worker protocol.
- The worker protocol preserves Kev's four route names.
- Holding the left Option key does not mask the right Option key's release, which would otherwise leave the microphone recording indefinitely.
- The audio callback downmixes to mono without taking a lock or allocating on the real-time thread.
- A second instance cannot take the lock, because two instances would install two HID event taps and race to paste.
- A worker that stops answering fails the utterance explicitly and fails every later utterance immediately, instead of blocking the pipeline thread forever.
- Startup deletes utterance audio that a previous run left behind, so a crash cannot quietly make "audio is temporary" false.
- The wedge timeout always outgrows the audio it covers, so a slow transcription is never mistaken for a wedged worker and does not poison a healthy one.
- Captured audio is resampled to the 16 kHz Whisper expects, because handing it the device's 48 kHz samples transcribes a chipmunk rather than failing.
- `PASS_THROUGH` never invokes the text processor.
- `COMPLEX`, a processor failure, and an empty rewrite each fail explicitly rather than falling back to inserting the raw transcript.
- S1-mini's control line rejects values outside the sets the model was trained on.
- A rewrite that changes the currency the speaker named loses the utterance instead of inserting it, because a wrong amount in a payment field is worse than no text. Non-currency senses of "pounds", matching symbols, and two currencies in one sentence all still insert normally.
- S1-mini's rewrites have not drifted from the recorded engine, checked against 53 real dictation transcripts from the shadow log.
- The MLX runtime guards fire explicitly: a missing converted model, a drifted `mlx-lm` pin, and a `transformers` version outside the vendored kev router's range each fail with a message naming the fix.

The existing research project owns batch lifecycle. Its `test_burned_batch_rejects_a_record_before_it_reaches_the_log` protects the high-severity burn invariant and should be run when touching `localflow-research/src/dictation_router/shadow.py`:

```sh
cd ../localflow-research
.venv/bin/python -m pytest tests/test_shadow.py -q
```
