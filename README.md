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

The research runtime and the already-downloaded Whisper model are used without configuration.

One local setup step is needed the first time, which converts S1-mini to the 8-bit MLX build LocalFlow runs.
It reads the weights already in this machine's Hugging Face cache and downloads nothing:

```sh
(cd ../localflow-research && .venv-kev/bin/python scripts/convert_s1_mlx.py)
cargo run --release
```

If the converted model is missing, the worker says so at startup and names that script rather than falling back to another engine.

Grant LocalFlow microphone, Accessibility, and Input Monitoring permissions when macOS asks.
The microphone prompt now appears at launch rather than at the first dictation, because the input stream is opened once at startup.
Opening the device costs over a hundred milliseconds, and paying that on each keypress used to come out of the first moments of speech.
The stream is paused as soon as it is opened and resumed only while you hold the hotkey, so the microphone is not live between dictations, or before the first one.
Then focus an editable application, hold Right Option while speaking, and release it.

The worker sets `HF_HUB_OFFLINE=1`; it does not download models or send dictated content over the network. Captured WAV audio is temporary and is removed after inference.

## Install as an app

LocalFlow can be built as a normal macOS application:

```sh
./scripts/build-app.sh
```

This installs `/Applications/LocalFlow.app`.

Installing to a fixed location matters more than it looks.
macOS grants Accessibility, Input Monitoring and Microphone permission per executable, so a binary that moves is treated as a different application and has to be granted them again.
Installing to a stable path is what gives the app a stable identity to grant those permissions to.

The signature matters for the same reason.
`scripts/build-app.sh` signs with a self-signed identity created once by `scripts/make-signing-cert.sh`, because an ad-hoc signature carries no identity at all and macOS then falls back to identifying the app by a hash of its binary.
Every rebuild changes that hash, so every rebuild looked like a different application with none of the previous one's permissions.
The certificate has nothing to do with trust or distribution; it exists so successive builds present the same identity to this machine.

### Two permissions have to be added by hand

LocalFlow prompts for the microphone and for nothing else.
The other two must be added manually in System Settings, Privacy and Security, using the plus button to select `/Applications/LocalFlow.app`.

| Permission | Needed for | Symptom when missing |
| --- | --- | --- |
| Input Monitoring | Seeing the hotkey while another application is focused | The hotkey only works while LocalFlow itself is focused |
| Accessibility | Synthesizing the Cmd-V that inserts the text | Dictation transcribes but nothing appears at the cursor |
| Microphone | Capturing audio. This one does prompt, on the first capture | Capture returns silence, reported as audio too short or quiet |

Neither of the first two announces itself.
The hotkey uses a listen-only event tap, and macOS does not reliably raise a prompt for those: it delivers a reduced event stream instead, so the app looks like it is working rather than like it is blocked.

Quit and relaunch LocalFlow after granting either one.
The event tap is created at startup and does not pick up a grant made while the app is running.

#### If a permission is listed and enabled but does not work

Remove the entry and add it again. Do not toggle it off first.

macOS records a permission against the signature the application had when the grant was made.
Rebuilding LocalFlow with a different signature, which is what happened when it moved from an ad-hoc signature to the self-signed one, leaves an entry that still displays as enabled while the current binary no longer matches what was recorded.

Toggling the switch off and on again does not fix this.
That only changes the stored decision on a record that already fails to match.
Select the entry, remove it with the minus button while it is still enabled, then add `/Applications/LocalFlow.app` again with the plus button, which writes a fresh record against the signature the app has now.

This presents as the app being unable to do the thing it has permission for.
Insertion is the clearest case: the text reaches the clipboard, a manual paste works, the app reports a successful insert, and nothing arrives at the cursor.

The same work launched from a terminal will appear to behave correctly, because a process started from a shell is attributed to that shell for permission purposes and borrows its grants.
That makes running from the terminal a misleading way to test this particular class of problem.

Now that the signing identity is stable this should not recur, since rebuilds present the same signature.

Do not run `tccutil reset` against these expecting a fresh prompt.
It removes the entry and nothing re-creates it, which leaves the app quietly unable to see the hotkey or to insert text.

The app has no Dock icon by design.
It is a floating widget, so it is quit from the capsule's right-click menu or from the console window.

The app still expects the research checkout at `~/Desktop/localflow-research`.
Set `LOCALFLOW_RESEARCH_ROOT` if it lives somewhere else.
If the runtime is missing, the console names the exact path that was searched and the file that was not found.

## Insertion behavior

Insertion puts the transcript on the system pasteboard and synthesizes Cmd-V.

The transcript is left on the pasteboard afterwards.
Restoring the previous clipboard on a timer raced the target application's asynchronous paste handling, which could insert the older clipboard contents instead of the dictated text.

LocalFlow records which application was frontmost when dictation started and refuses to insert if a different application is frontmost when the text is ready.
The transcript is on the pasteboard in that case, so the words are not lost; they need a paste.

The pasteboard is written before anything that can refuse, including the Accessibility check.
A dictation LocalFlow cannot place for you is exactly the one where you most need the text, so every path from that point leaves it on the clipboard.

## Interface

LocalFlow shows two windows.

The capsule is a small, frameless, always-on-top pill that shows the current state and, on failure, a short headline.
It has no title bar and no close button.
Drag its body to move it, and right-click it for a menu with "Open console" and "Quit LocalFlow".
Cmd-Q does not quit LocalFlow, because a frameless window has no menu bar for the shortcut to reach; quitting goes through that right-click menu or the console's Quit button instead.

The console is a conventional window, opened from the icon on the capsule's right side, with an Activity tab (dictation history and the full latency breakdown) and a Status tab (hotkey, worker state, model names, and paths).
It is a separate window from the capsule, so dictation is not blocked while it is open.
A red dot on the capsule's icon marks an unread failure and clears when the console is opened; hovering the icon shows the full failure message.

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

