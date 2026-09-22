//! Keeping a dictation's recording, when the user has asked for it.
//!
//! Exists so a recognizer other than the one in production can be compared
//! against real dictation offline. Whisper pays a fixed encoder cost per
//! thirty-second window, so a one-second utterance costs nearly as much as a
//! twenty-five second one, and the candidates that escape that are untestable
//! without audio to replay.
//!
//! The corpus lives in the research checkout beside the traces it joins to,
//! under a `.gitignore` entry committed before this module existed. Transcripts
//! are already tracked in that repository's history; recordings must never be.
//!
//! Nothing here runs unless `Settings::retain_audio` is set.

use std::path::{Path, PathBuf};

/// What the worker made of a recording, reduced to what the corpus records.
///
/// The two rejections are kept as deliberately as the successes: they are the
/// utterances where the application threw away words that were really said,
/// which is the comparison most likely to change which recognizer runs.
pub enum Outcome<'a> {
    Transcribed { transcript: &'a str, model: &'a str },
    /// Silero found no speech. There is no transcript, and that is the record.
    NoSpeech,
    /// Whisper distrusted its own decode. There is no transcript either, and
    /// whether a candidate produces one is the whole question.
    Unintelligible,
}

/// One line of the manifest: which recording, and what production made of it.
///
/// Deliberately this small. The audio's file stem is the `trace_id` already
/// written into `localflow_traces.jsonl`, so outcome, rejection reason, device,
/// energy, timings and the timestamp all join from there by that id. Copying
/// them here would be two records that can disagree about one utterance. The
/// transcript is the only thing the traces do not hold, and it is the baseline
/// a candidate gets scored against.
#[derive(serde::Serialize)]
struct Entry<'a> {
    trace_id: &'a str,
    /// Absent for a rejection, which is the fact worth recording about it.
    #[serde(skip_serializing_if = "Option::is_none")]
    transcript: Option<&'a str>,
    /// Which recognizer produced `transcript`, so a corpus collected across a
    /// change of production model is not ambiguous about its own baseline.
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<&'a str>,
}

/// Keep this recording, or say why it could not be kept.
///
/// Returns `Ok(())` only when the audio is in the corpus and the manifest
/// describes it. On any failure the corpus is left exactly as it was, so it
/// never holds a recording nothing can be scored against.
///
/// The caller deletes the recording either way. That is the point: a retention
/// failure must not leave audio behind in a cache the user believes is swept.
pub fn retain(audio_path: &Path, outcome: &Outcome) -> Result<(), String> {
    let corpus = corpus_dir()?;
    std::fs::create_dir_all(&corpus)
        .map_err(|error| format!("could not create {}: {error}", corpus.display()))?;

    let trace_id = audio_path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .ok_or_else(|| format!("{} has no usable name", audio_path.display()))?;
    let name = audio_path
        .file_name()
        .ok_or_else(|| format!("{} has no file name", audio_path.display()))?;
    let destination = corpus.join(name);

    // Copied rather than renamed: the corpus and the app's cache need not be on
    // one volume, and a rename across volumes fails. A median dictation is
    // about 160 KB, so there is nothing to win by being clever about it.
    std::fs::copy(audio_path, &destination)
        .map_err(|error| format!("could not copy audio to {}: {error}", destination.display()))?;

    // Written after the audio is in place, so a manifest line never describes a
    // recording that is not there...
    if let Err(error) = append(&corpus, trace_id, outcome) {
        // The audio arrived and its description did not. A recording with no
        // transcript beside it cannot be scored, so it is retained voice for no
        // benefit and goes back out rather than sitting there.
        if let Err(cleanup) = std::fs::remove_file(&destination) {
            return Err(format!(
                "{error}, and the copied audio at {} could not be removed either: {cleanup}",
                destination.display()
            ));
        }
        return Err(error);
    }
    Ok(())
}

/// Where the corpus lives: beside the traces it joins to, in the research
/// checkout the worker already writes into.
fn corpus_dir() -> Result<PathBuf, String> {
    let root = crate::router::research_root().map_err(|error| error.to_string())?;
    Ok(root.join("data").join("corpus"))
}

fn append(corpus: &Path, trace_id: &str, outcome: &Outcome) -> Result<(), String> {
    use std::io::Write;

    let entry = match outcome {
        Outcome::Transcribed { transcript, model } => {
            Entry { trace_id, transcript: Some(transcript), model: Some(model) }
        }
        Outcome::NoSpeech | Outcome::Unintelligible => {
            Entry { trace_id, transcript: None, model: None }
        }
    };
    let mut line = serde_json::to_string(&entry)
        .map_err(|error| format!("could not encode the manifest entry: {error}"))?;
    line.push('\n');

    let path = corpus.join("manifest.jsonl");
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|error| format!("could not open {}: {error}", path.display()))?;
    file.write_all(line.as_bytes())
        .map_err(|error| format!("could not write {}: {error}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `retain` resolves the corpus from the research checkout, which a test
    /// must not write into, so the pieces that do not need that path are
    /// exercised directly against a scratch directory.
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join(format!("privacyflow-retention-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn manifest(corpus: &Path) -> String {
        std::fs::read_to_string(corpus.join("manifest.jsonl")).unwrap()
    }

    /// The join key is the whole contract with the traces: the manifest's
    /// `trace_id` has to be the audio's file stem, because that is the id
    /// `localflow_traces.jsonl` already records.
    #[test]
    fn a_transcript_is_recorded_against_the_audios_own_trace_id() {
        let corpus = scratch("transcribed");
        append(
            &corpus,
            "utterance-20260923T101500.250Z",
            &Outcome::Transcribed { transcript: "keep this", model: "large-v3-turbo" },
        )
        .unwrap();

        let line = manifest(&corpus);
        assert!(line.contains(r#""trace_id":"utterance-20260923T101500.250Z""#), "{line}");
        assert!(line.contains(r#""transcript":"keep this""#), "{line}");
        assert!(line.contains(r#""model":"large-v3-turbo""#), "{line}");
    }

    /// A rejection's record is that there is no transcript. An empty string
    /// would read as a recognizer that returned nothing, which is a different
    /// claim from one that refused.
    #[test]
    fn a_rejection_records_no_transcript_rather_than_an_empty_one() {
        let corpus = scratch("rejected");
        append(&corpus, "utterance-20260923T101501.000Z", &Outcome::NoSpeech).unwrap();

        let line = manifest(&corpus);
        assert!(!line.contains("transcript"), "a rejection has no transcript: {line}");
        assert!(!line.contains("model"), "and nothing produced one: {line}");
    }

    /// One line per utterance, appended. A corpus is built over days of
    /// ordinary use, so a write that replaced the file would lose all of it.
    #[test]
    fn each_utterance_appends_one_line() {
        let corpus = scratch("append");
        append(&corpus, "one", &Outcome::NoSpeech).unwrap();
        append(&corpus, "two", &Outcome::Unintelligible).unwrap();
        append(
            &corpus,
            "three",
            &Outcome::Transcribed { transcript: "third", model: "large-v3-turbo" },
        )
        .unwrap();

        let lines: Vec<_> = manifest(&corpus).lines().map(str::to_owned).collect();
        assert_eq!(lines.len(), 3, "appended, not overwritten");
        assert!(lines[2].contains("third"));
    }
}
