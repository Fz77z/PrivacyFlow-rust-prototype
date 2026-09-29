//! Joining a finished dictation onto whatever the cursor is already sitting
//! after.
//!
//! Dictating twice in a row used to produce `out.Needs space`, because the
//! transcript was pasted exactly as the router produced it and nothing looked
//! at the destination. The rule here supplies the missing space.

/// Characters that close a clause and therefore want a space after them.
/// Opening brackets are deliberately absent: `(` must stay welded to the word
/// it opens.
const CLOSING: &str = "!%),.:;>?]}";

/// Characters that are a word separator when they stand alone but part of a
/// token when they are wedged against one. A `-` opening a list item wants a
/// space after it; the `/` in a file path and the `+` in `2+2` do not.
const AMBIGUOUS: &str = r"\/|-&*+=~";

/// Whether a character belongs to a script that writes without spaces between
/// words, where separating the join would be wrong rather than helpful.
fn is_spaceless_script(character: char) -> bool {
    matches!(character,
        '\u{3040}'..='\u{309F}'   // Hiragana
        | '\u{30A0}'..='\u{30FF}' // Katakana
        | '\u{3400}'..='\u{4DBF}' // Han, extension A
        | '\u{4E00}'..='\u{9FFF}' // Han
    )
}

/// Whether text following `previous` needs a space inserted before it.
fn needs_leading_space(previous: char, before_previous: Option<char>) -> bool {
    // Asked before the alphanumeric question below, which Hiragana and Han
    // would otherwise answer yes to.
    if is_spaceless_script(previous) {
        return false;
    }
    if previous.is_alphanumeric() || CLOSING.contains(previous) {
        return true;
    }
    // An ambiguous character separates words only when it stands alone, so
    // what sits behind it decides. Nothing behind it means it opens the line.
    if AMBIGUOUS.contains(previous) {
        return before_previous.is_none_or(|character| character == ' ');
    }
    false
}

/// Place `text` after `previous`, the character the cursor currently sits
/// after, adding a separating space when the join would otherwise run two
/// words together.
///
/// `previous` is `None` when PrivacyFlow has no idea what precedes the cursor,
/// which is the common case: the first dictation into a field, or any
/// dictation after the user has typed or clicked. No information means no
/// modification, so the transcript is inserted exactly as it arrived rather
/// than guessed at.
///
/// `before_previous` disambiguates the characters in `AMBIGUOUS`, which mean
/// different things depending on whether they stand alone.
pub fn join(previous: Option<char>, before_previous: Option<char>, text: &str) -> String {
    let Some(previous) = previous else {
        return text.to_owned();
    };
    if needs_leading_space(previous, before_previous) {
        return format!(" {text}");
    }
    text.to_owned()
}

/// What PrivacyFlow knows about the character in front of the cursor, which is
/// only ever what PrivacyFlow itself put there.
///
/// macOS can be asked what surrounds the cursor, but many applications answer
/// nothing and the question costs an accessibility round trip. This knows the
/// answer for free in the one case that matters, two dictations in a row, and
/// admits to knowing nothing the moment anything else could have moved the
/// cursor.
#[derive(Debug, Default)]
pub struct CursorMemory {
    placed: Option<Placed>,
}

/// The tail of the last dictation PrivacyFlow placed, and where it placed it.
#[derive(Debug, Clone, Copy)]
struct Placed {
    pid: i32,
    previous: char,
    before_previous: Option<char>,
}

impl CursorMemory {
    /// Record what a successful insertion left behind the cursor. Only ever
    /// called for text that actually reached the document.
    pub fn remember(&mut self, pid: i32, inserted: &str) {
        let mut tail = inserted.chars().rev();
        self.placed = tail.next().map(|previous| Placed {
            pid,
            previous,
            before_previous: tail.next(),
        });
    }

    /// Give up the memory, because something that could have moved the cursor
    /// happened. Forgetting is not a failure: it returns PrivacyFlow to
    /// inserting text exactly as it arrives, which is what it always did.
    pub fn forget(&mut self) {
        self.placed = None;
    }

    /// What sits behind the cursor in `pid`, as far as this knows.
    ///
    /// A different application means the cursor is somewhere this never wrote,
    /// so the memory does not apply even though nothing has invalidated it.
    pub fn recall(&self, pid: i32) -> (Option<char>, Option<char>) {
        match self.placed {
            Some(placed) if placed.pid == pid => (Some(placed.previous), placed.before_previous),
            _ => (None, None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tail_of_an_insertion_is_what_the_next_one_joins_onto() {
        let mut memory = CursorMemory::default();
        memory.remember(42, "Needs space.");
        assert_eq!(memory.recall(42), (Some('.'), Some('e')));
    }

    #[test]
    fn a_different_application_is_a_cursor_this_never_wrote_to() {
        let mut memory = CursorMemory::default();
        memory.remember(42, "Needs space.");
        assert_eq!(memory.recall(99), (None, None));
    }

    #[test]
    fn typing_or_clicking_in_between_gives_up_the_memory() {
        let mut memory = CursorMemory::default();
        memory.remember(42, "Needs space.");
        memory.forget();
        assert_eq!(memory.recall(42), (None, None));
    }

    #[test]
    fn nothing_is_known_about_the_cursor_so_the_transcript_is_untouched() {
        assert_eq!(join(None, None, "Needs space."), "Needs space.");
    }

    #[test]
    fn a_second_dictation_after_a_full_stop_gains_a_space() {
        // The reported bug: `...what goes out.Needs space.`
        assert_eq!(join(Some('.'), Some('t'), "Needs space."), " Needs space.");
    }

    #[test]
    fn a_dictation_continuing_mid_word_gains_a_space() {
        assert_eq!(join(Some('t'), Some('u'), "and then"), " and then");
    }

    #[test]
    fn a_dictation_after_a_digit_gains_a_space() {
        assert_eq!(join(Some('7'), Some('1'), "items"), " items");
    }

    #[test]
    fn a_space_already_there_is_not_doubled() {
        assert_eq!(join(Some(' '), Some('.'), "Needs space."), "Needs space.");
    }

    #[test]
    fn an_opening_bracket_stays_welded_to_what_follows_it() {
        assert_eq!(join(Some('('), Some(' '), "aside"), "aside");
    }

    #[test]
    fn a_closing_bracket_gains_a_space() {
        assert_eq!(join(Some(')'), Some('e'), "and then"), " and then");
    }

    #[test]
    fn a_path_separator_wedged_against_a_word_does_not_gain_a_space() {
        assert_eq!(join(Some('/'), Some('c'), "privacyflow"), "privacyflow");
    }

    #[test]
    fn a_list_dash_standing_alone_gains_a_space() {
        assert_eq!(join(Some('-'), Some(' '), "first item"), " first item");
    }

    #[test]
    fn a_dash_opening_the_line_gains_a_space() {
        assert_eq!(join(Some('-'), None, "first item"), " first item");
    }

    #[test]
    fn japanese_does_not_gain_a_space() {
        // CJK writes without spaces between words, so inserting one is wrong
        // even though the preceding character is a letter.
        assert_eq!(join(Some('す'), Some('で'), "それから"), "それから");
    }
}
