pub mod results;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use std::fmt;
use std::time::Instant;

pub struct TestEvent {
    pub time: Instant,
    pub key: KeyEvent,
    pub correct: Option<bool>,
}

pub fn is_missed_word_event(event: &TestEvent) -> bool {
    event.correct != Some(true)
}

impl fmt::Debug for TestEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TestEvent")
            .field("time", &String::from("Instant { ... }"))
            .field("key", &self.key)
            .finish()
    }
}

#[derive(Debug)]
pub struct TestWord {
    pub text: String,
    pub progress: String,
    pub events: Vec<TestEvent>,
}

impl From<String> for TestWord {
    fn from(string: String) -> Self {
        TestWord {
            text: string,
            progress: String::new(),
            events: Vec::new(),
        }
    }
}

impl From<&str> for TestWord {
    fn from(string: &str) -> Self {
        Self::from(string.to_string())
    }
}

#[derive(Debug)]
pub struct Test {
    pub words: Vec<TestWord>,
    pub current_word: usize,
    pub complete: bool,
    pub backtracking_enabled: bool,
    pub sudden_death_enabled: bool,
    pub backspace_enabled: bool,
    /// (expected letter, typed correctly) for every first attempt at a letter
    pub letter_results: Vec<(char, bool)>,
}

impl Test {
    pub fn new(
        words: Vec<String>,
        backtracking_enabled: bool,
        sudden_death_enabled: bool,
        backspace_enabled: bool,
    ) -> Self {
        Self {
            words: words.into_iter().map(TestWord::from).collect(),
            current_word: 0,
            complete: false,
            backtracking_enabled,
            sudden_death_enabled,
            backspace_enabled,
            letter_results: Vec::new(),
        }
    }

    /// Record how the user did on the letter they were expected to type next.
    /// Only counts when the word was correct so far, so that one typo does not
    /// cascade into a miss on every following letter.
    fn note_letter(letter_results: &mut Vec<(char, bool)>, word: &TestWord, typed: Option<char>) {
        if !word.text.starts_with(&word.progress[..]) {
            return;
        }
        if let Some(expected) = word.text.chars().nth(word.progress.chars().count()) {
            if !expected.is_whitespace() {
                letter_results.push((expected, typed == Some(expected)));
            }
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) {
        if key.kind != KeyEventKind::Press {
            return;
        }

        let word = &mut self.words[self.current_word];
        match key.code {
            KeyCode::Char(' ') | KeyCode::Enter => {
                if word.text.chars().nth(word.progress.len()) == Some(' ') {
                    word.progress.push(' ');
                    word.events.push(TestEvent {
                        time: Instant::now(),
                        correct: Some(true),
                        key,
                    })
                } else if !word.progress.is_empty() || word.text.is_empty() {
                    let correct = word.text == word.progress;
                    if !correct {
                        // skipped the rest of the word: the next expected letter was missed
                        Self::note_letter(&mut self.letter_results, word, None);
                    }
                    if self.sudden_death_enabled && !correct {
                        self.reset();
                    } else {
                        word.events.push(TestEvent {
                            time: Instant::now(),
                            correct: Some(correct),
                            key,
                        });
                        self.next_word();
                    }
                }
            }
            KeyCode::Backspace => {
                if word.progress.is_empty() && self.backtracking_enabled && self.backspace_enabled {
                    self.last_word();
                } else if self.backspace_enabled {
                    word.events.push(TestEvent {
                        time: Instant::now(),
                        correct: Some(!word.text.starts_with(&word.progress[..])),
                        key,
                    });
                    word.progress.pop();
                }
            }
            // CTRL-BackSpace and CTRL-W
            KeyCode::Char('h') | KeyCode::Char('w')
                if key.modifiers.contains(KeyModifiers::CONTROL) =>
            {
                if self.words[self.current_word].progress.is_empty() {
                    self.last_word();
                }

                let word = &mut self.words[self.current_word];

                word.events.push(TestEvent {
                    time: Instant::now(),
                    correct: None,
                    key,
                });
                word.progress.clear();
            }
            KeyCode::Char(c) => {
                Self::note_letter(&mut self.letter_results, word, Some(c));
                word.progress.push(c);
                let correct = word.text.starts_with(&word.progress[..]);
                if self.sudden_death_enabled && !correct {
                    self.reset();
                } else {
                    word.events.push(TestEvent {
                        time: Instant::now(),
                        correct: Some(correct),
                        key,
                    });
                    if word.progress == word.text && self.current_word == self.words.len() - 1 {
                        self.complete = true;
                        self.current_word = 0;
                    }
                }
            }
            _ => {}
        };
    }

    fn last_word(&mut self) {
        if self.current_word != 0 {
            self.current_word -= 1;
        }
    }

    fn next_word(&mut self) {
        if self.current_word == self.words.len() - 1 {
            self.complete = true;
            self.current_word = 0;
        } else {
            self.current_word += 1;
        }
    }

    fn reset(&mut self) {
        self.words.iter_mut().for_each(|word: &mut TestWord| {
            word.progress.clear();
            word.events.clear();
        });
        self.current_word = 0;
        self.complete = false;
    }
}

#[cfg(test)]
mod letter_tracking {
    use super::*;

    fn press(test: &mut Test, c: char) {
        test.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
    }

    #[test]
    fn records_expected_letter_and_correctness() {
        let mut test = Test::new(vec!["cat".into(), "dog".into()], true, false, true);
        press(&mut test, 'c');
        press(&mut test, 'x'); // expected 'a', missed
        test.handle_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        press(&mut test, 'a');
        press(&mut test, 't');
        press(&mut test, ' ');
        press(&mut test, 'd');
        press(&mut test, ' '); // skipped "og": 'o' counts as missed
        assert_eq!(
            test.letter_results,
            vec![
                ('c', true),
                ('a', false),
                ('a', true),
                ('t', true),
                ('d', true),
                ('o', false)
            ]
        );
    }

    #[test]
    fn does_not_cascade_after_a_typo() {
        let mut test = Test::new(vec!["cat".into()], true, false, true);
        press(&mut test, 'x'); // expected 'c', missed
        press(&mut test, 'a'); // word already wrong: not counted against 'a'
        press(&mut test, 't');
        assert_eq!(test.letter_results, vec![('c', false)]);
    }
}
