mod config;
mod stats;
mod test;
mod ui;

use config::Config;
use stats::Stats;
use test::{results::Results, Test};
use ui::TestView;

use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::{generate, Shell};
use crossterm::{
    self, cursor,
    event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    execute, terminal,
};
use rand::{seq::SliceRandom, thread_rng};
use ratatui::{backend::CrosstermBackend, terminal::Terminal};
use rust_embed::RustEmbed;
use std::{
    ffi::OsString,
    fs,
    io::{self, BufRead},
    num,
    path::PathBuf,
    str,
};

#[derive(RustEmbed)]
#[folder = "resources/runtime"]
struct Resources;

#[derive(Debug, Parser)]
#[command(about, version)]
struct Opt {
    /// Read test contents from the specified file, or "-" for stdin
    #[arg(value_name = "PATH")]
    contents: Option<PathBuf>,

    #[arg(short, long)]
    debug: bool,

    /// Specify word count
    #[arg(short, long, value_name = "N", default_value = "50")]
    words: num::NonZeroUsize,

    /// Use config file
    #[arg(short, long, value_name = "PATH")]
    config: Option<PathBuf>,

    /// Specify test language in file
    #[arg(long, value_name = "PATH")]
    language_file: Option<PathBuf>,

    /// Specify test language
    #[arg(short, long, value_name = "LANG")]
    language: Option<String>,

    /// List installed languages
    #[arg(long)]
    list_languages: bool,

    /// Disable backtracking to completed words
    #[arg(long)]
    no_backtrack: bool,

    /// Enable sudden death mode to restart on first error
    #[arg(long)]
    sudden_death: bool,

    /// Disable backspace
    #[arg(long)]
    no_backspace: bool,

    /// Don't bias the test toward letters you often mistype
    #[arg(long)]
    no_adaptive: bool,

    /// How strongly to favour words with your weak letters (overrides config)
    #[arg(long, value_name = "N")]
    adaptive_strength: Option<f64>,

    /// Don't read or update the mistake history for this run
    #[arg(long)]
    no_stats: bool,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Generate shell completions
    Completions {
        /// Shell to generate completions for
        shell: Shell,
    },
    /// Show your all-time per-letter accuracy
    Stats {
        /// Forget all recorded mistakes
        #[arg(long)]
        reset: bool,
    },
}

impl Opt {
    fn gen_contents(&self, stats: &Stats) -> Option<Vec<String>> {
        match &self.contents {
            Some(path) => {
                let lines: Vec<String> = if path.as_os_str() == "-" {
                    std::io::stdin()
                        .lock()
                        .lines()
                        .map_while(Result::ok)
                        .collect()
                } else {
                    let file = fs::File::open(path).expect("Error reading language file.");
                    io::BufReader::new(file)
                        .lines()
                        .map_while(Result::ok)
                        .collect()
                };

                Some(lines.iter().map(String::from).collect())
            }
            None => {
                let lang_name = self
                    .language
                    .clone()
                    .unwrap_or_else(|| self.config().default_language);

                let bytes: Vec<u8> = self
                    .language_file
                    .as_ref()
                    .map(fs::read)
                    .and_then(Result::ok)
                    .or_else(|| fs::read(self.language_dir().join(&lang_name)).ok())
                    .or_else(|| {
                        Resources::get(&format!("language/{}", &lang_name))
                            .map(|f| f.data.into_owned())
                    })?;

                let language: Vec<&str> = str::from_utf8(&bytes)
                    .expect("Language file had non-utf8 encoding.")
                    .lines()
                    .collect();

                Some(pick_words(
                    &language,
                    self.words.get(),
                    stats,
                    self.adaptive_strength(),
                ))
            }
        }
    }

    /// Effective adaptive strength after CLI overrides (0 disables biasing)
    fn adaptive_strength(&self) -> f64 {
        if self.no_adaptive {
            return 0.0;
        }
        let config = self.config();
        match self.adaptive_strength {
            Some(s) => s,
            None if config.adaptive => config.adaptive_strength,
            None => 0.0,
        }
    }

    /// Build a test, annotating it with the letters it is weighted toward
    fn new_test(&self, contents: Vec<String>, stats: &Stats) -> Test {
        let mut test = Test::new(
            contents,
            !self.no_backtrack,
            self.sudden_death,
            !self.no_backspace,
        );
        if self.contents.is_none() && self.adaptive_strength() > 0.0 {
            test.focus_letters = stats.weakest(5).into_iter().map(|(c, _)| c).collect();
        }
        test
    }

    /// Where per-letter mistake history is stored
    fn stats_path(&self) -> PathBuf {
        self.config_dir().join("stats.toml")
    }

    fn load_stats(&self) -> Stats {
        if self.no_stats {
            Stats::default()
        } else {
            Stats::load(&self.stats_path())
        }
    }

    /// Configuration
    fn config(&self) -> Config {
        fs::read(
            self.config
                .clone()
                .unwrap_or_else(|| self.config_dir().join("config.toml")),
        )
        .map(|bytes| {
            toml::from_str(str::from_utf8(&bytes).unwrap_or_default())
                .expect("Configuration was ill-formed.")
        })
        .unwrap_or_default()
    }

    /// Installed languages under config directory
    fn languages(&self) -> io::Result<impl Iterator<Item = OsString>> {
        let builtin = Resources::iter().filter_map(|name| {
            name.strip_prefix("language/")
                .map(ToOwned::to_owned)
                .map(OsString::from)
        });

        let configured = self
            .language_dir()
            .read_dir()
            .into_iter()
            .flatten()
            .map_while(Result::ok)
            .map(|e| e.file_name());

        Ok(builtin.chain(configured))
    }

    /// Config directory
    fn config_dir(&self) -> PathBuf {
        dirs::config_dir()
            .expect("Failed to find config directory.")
            .join("ttyper")
    }

    /// Language directory under config directory
    fn language_dir(&self) -> PathBuf {
        self.config_dir().join("language")
    }
}

/// Choose `count` words from `language`, favouring words that contain letters
/// the user often misses. Words are drawn without replacement until the list
/// runs out, then drawn again, so short word lists still fill the test.
fn pick_words(language: &[&str], count: usize, stats: &Stats, strength: f64) -> Vec<String> {
    let mut rng = thread_rng();
    let mut contents: Vec<String> = Vec::with_capacity(count);
    if language.is_empty() {
        return contents;
    }
    while contents.len() < count {
        let batch = (count - contents.len()).min(language.len());
        let picked: Vec<&str> = if strength > 0.0 {
            language
                .choose_multiple_weighted(&mut rng, batch, |w| stats.word_weight(w, strength))
                .expect("word weights are always positive and finite")
                .copied()
                .collect()
        } else {
            language.choose_multiple(&mut rng, batch).copied().collect()
        };
        contents.extend(picked.into_iter().map(ToOwned::to_owned));
    }
    contents.shuffle(&mut rng);
    contents
}

/// Words for a "practice missed words" round: each distinct missed word up to
/// 5 times, never more words than a normal test. Without the dedupe and cap,
/// repeated practice rounds snowball (every missed copy spawned 5 more).
fn practice_set(missed: &[String], max_words: usize) -> Vec<String> {
    let mut distinct: Vec<&String> = Vec::new();
    for w in missed {
        if !distinct.contains(&w) {
            distinct.push(w);
        }
    }
    let repeats = (max_words / distinct.len().max(1)).clamp(1, 5);
    distinct
        .iter()
        .flat_map(|w| std::iter::repeat_n((*w).clone(), repeats))
        .take(max_words)
        .collect()
}

/// Fold a finished test into the persistent stats and build its results view.
fn finish_test(test: &Test, stats: &mut Stats, opt: &Opt) -> Results {
    let mut results = Results::from(test);
    stats.record(&test.letter_results);
    if !opt.no_stats && !test.letter_results.is_empty() {
        if let Err(e) = stats.save(&opt.stats_path()) {
            if opt.debug {
                eprintln!("Failed to save stats: {e}");
            }
        }
    }
    results.weak_letters = stats.weakest(5);
    results
}

fn print_stats(stats: &Stats) {
    if stats.letters.is_empty() {
        println!("No typing history yet. Finish a test and come back.");
        return;
    }
    println!("tests recorded: {}", stats.tests);
    println!("{:<6} {:>9} {:>8} {:>8}", "letter", "attempts", "misses", "accuracy");
    let mut rows: Vec<(&String, &stats::LetterStat)> = stats.letters.iter().collect();
    rows.sort_by(|a, b| {
        let aa = a.1.accuracy().unwrap_or(1.0);
        let ba = b.1.accuracy().unwrap_or(1.0);
        aa.partial_cmp(&ba).unwrap_or(std::cmp::Ordering::Equal)
    });
    for (letter, stat) in rows {
        println!(
            "{:<6} {:>9.1} {:>8.1} {:>7.1}%",
            letter,
            stat.attempts,
            stat.misses,
            stat.accuracy().unwrap_or(1.0) * 100.0
        );
    }
    println!("(counts decay over time so recent tests weigh more)");
}

enum State {
    Test(Test),
    Results(Results),
}

impl State {
    fn render_into<B: ratatui::backend::Backend>(
        &self,
        terminal: &mut Terminal<B>,
        config: &Config,
    ) -> io::Result<()> {
        match self {
            State::Test(test) => {
                terminal.draw(|f| {
                    let area = f.size();
                    f.render_widget(
                        config.theme.apply_to(TestView {
                            test,
                            show_input: config.show_input,
                        }),
                        area,
                    );
                    if config.show_input {
                        // Position cursor at end of input for IME composition support
                        let input_area = ui::input_area(test, area);
                        let inner_x = input_area.x + 1;
                        let inner_y = input_area.y + 1;
                        let progress_width = ratatui::text::Line::from(
                            test.words[test.current_word].progress.as_str(),
                        )
                        .width() as u16;
                        let max_cursor_x = input_area.right().saturating_sub(2);
                        f.set_cursor((inner_x + progress_width).min(max_cursor_x), inner_y);
                    } else if let Some((x, y)) =
                        ui::TestLayout::new(test, area, false).caret(test)
                    {
                        // thin bar caret sits just before the next letter
                        f.set_cursor(x, y);
                    }
                })?;
            }
            State::Results(results) => {
                terminal.draw(|f| {
                    f.render_widget(config.theme.apply_to(results), f.size());
                })?;
            }
        }
        Ok(())
    }
}

fn main() -> io::Result<()> {
    let opt = Opt::parse();
    if opt.debug {
        dbg!(&opt);
    }

    let config = opt.config();
    if opt.debug {
        dbg!(&config);
    }

    match opt.command {
        Some(Command::Completions { shell }) => {
            generate(shell, &mut Opt::command(), "ttyper", &mut io::stdout());
            return Ok(());
        }
        Some(Command::Stats { reset }) => {
            if reset {
                match fs::remove_file(opt.stats_path()) {
                    Ok(()) => println!("Mistake history cleared."),
                    Err(e) if e.kind() == io::ErrorKind::NotFound => {
                        println!("No mistake history to clear.")
                    }
                    Err(e) => return Err(e),
                }
            } else {
                print_stats(&Stats::load(&opt.stats_path()));
            }
            return Ok(());
        }
        None => {}
    }

    let mut stats = opt.load_stats();

    if opt.list_languages {
        opt.languages()
            .unwrap()
            .for_each(|name| println!("{}", name.to_str().expect("Ill-formatted language name.")));

        return Ok(());
    }

    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend)?;
    let contents = opt
        .gen_contents(&stats)
        .expect("Couldn't get test contents. Make sure the specified language actually exists.");

    if contents.is_empty() {
        eprintln!("Error: the provided file or language contains no words to type.");
        eprintln!("If you specified a file, make sure it isn't empty.");
        std::process::exit(1);
    }

    terminal::enable_raw_mode()?;
    execute!(
        io::stdout(),
        cursor::Hide,
        cursor::SavePosition,
        cursor::SetCursorStyle::SteadyBar,
        terminal::EnterAlternateScreen,
    )?;
    terminal.clear()?;

    let mut state = State::Test(opt.new_test(contents, &stats));

    state.render_into(&mut terminal, &config)?;
    loop {
        let event = event::read()?;

        // handle exit controls
        match event {
            Event::Key(KeyEvent {
                code: KeyCode::Char('c'),
                kind: KeyEventKind::Press,
                modifiers: KeyModifiers::CONTROL,
                ..
            }) => break,
            Event::Key(KeyEvent {
                code: KeyCode::Esc,
                kind: KeyEventKind::Press,
                modifiers: KeyModifiers::NONE,
                ..
            }) => match state {
                State::Test(ref test) => {
                    state = State::Results(finish_test(test, &mut stats, &opt));
                }
                State::Results(_) => break,
            },
            _ => {}
        }

        match state {
            State::Test(ref mut test) => {
                if let Event::Key(key) = event {
                    test.handle_key(key);
                    if test.complete {
                        state = State::Results(finish_test(test, &mut stats, &opt));
                    }
                }
            }
            State::Results(ref result) => match event {
                Event::Key(KeyEvent {
                    code: KeyCode::Char('r'),
                    kind: KeyEventKind::Press,
                    modifiers: KeyModifiers::NONE,
                    ..
                }) => {
                    let new_contents = opt.gen_contents(&stats).expect(
                        "Couldn't get test contents. Make sure the specified language actually exists.",
                    );
                    if new_contents.is_empty() {
                        continue;
                    }
                    state = State::Test(opt.new_test(new_contents, &stats));
                }
                Event::Key(KeyEvent {
                    code: KeyCode::Char('p'),
                    kind: KeyEventKind::Press,
                    modifiers: KeyModifiers::NONE,
                    ..
                }) => {
                    if result.missed_words.is_empty() {
                        continue;
                    }
                    let mut practice_words = practice_set(&result.missed_words, opt.words.get());
                    practice_words.shuffle(&mut thread_rng());
                    state = State::Test(Test::new(
                        practice_words,
                        !opt.no_backtrack,
                        opt.sudden_death,
                        !opt.no_backspace,
                    ));
                }
                Event::Key(KeyEvent {
                    code: KeyCode::Char('q'),
                    kind: KeyEventKind::Press,
                    modifiers: KeyModifiers::NONE,
                    ..
                }) => break,
                _ => {}
            },
        }

        state.render_into(&mut terminal, &config)?;
    }

    terminal::disable_raw_mode()?;
    execute!(
        io::stdout(),
        cursor::RestorePosition,
        cursor::SetCursorStyle::DefaultUserShape,
        cursor::Show,
        terminal::LeaveAlternateScreen,
    )?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn make_opt(path: PathBuf) -> Opt {
        Opt {
            contents: Some(path),
            debug: false,
            words: num::NonZeroUsize::new(50).unwrap(),
            config: None,
            language_file: None,
            language: None,
            list_languages: false,
            no_backtrack: false,
            sudden_death: false,
            no_backspace: false,
            no_adaptive: false,
            adaptive_strength: None,
            no_stats: true,
            command: None,
        }
    }

    #[test]
    fn gen_contents_empty_file_returns_empty_vec() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.txt");
        fs::File::create(&path).unwrap();

        let contents = make_opt(path).gen_contents(&Stats::default()).unwrap();
        assert!(contents.is_empty(), "empty file should produce empty vec");
    }

    #[test]
    fn gen_contents_nonempty_file_returns_words() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("words.txt");
        let mut f = fs::File::create(&path).unwrap();
        writeln!(f, "hello world rust").unwrap();

        let contents = make_opt(path).gen_contents(&Stats::default()).unwrap();
        assert!(!contents.is_empty(), "non-empty file should produce words");
    }

    #[test]
    fn practice_set_dedupes_and_caps() {
        let missed: Vec<String> = ["cat", "cat", "cat", "dog"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let set = practice_set(&missed, 50);
        assert_eq!(set.len(), 10);
        assert_eq!(set.iter().filter(|w| *w == "cat").count(), 5);

        let many: Vec<String> = (0..30).map(|i| format!("w{i}")).collect();
        let set = practice_set(&many, 50);
        assert_eq!(set.len(), 30);
        let set = practice_set(&many, 20);
        assert_eq!(set.len(), 20);
    }

    #[test]
    fn pick_words_fills_requested_count() {
        let lang = ["a", "b", "c"];
        let words = pick_words(&lang, 10, &Stats::default(), 4.0);
        assert_eq!(words.len(), 10);
        let words = pick_words(&lang, 2, &Stats::default(), 0.0);
        assert_eq!(words.len(), 2);
    }

    #[test]
    fn pick_words_prefers_weak_letters() {
        let mut stats = Stats::default();
        // miss 'z' a lot, never miss anything else
        let mut events: Vec<(char, bool)> = (0..40).map(|_| ('z', false)).collect();
        events.extend("abcdefghijklmnopqrstuvwxy".chars().map(|c| (c, true)));
        stats.record(&events);

        let lang = ["zzz", "abc", "def", "ghi", "jkl", "mno", "pqr", "stu", "vwx"];
        let mut hits = 0;
        for _ in 0..200 {
            if pick_words(&lang, 1, &stats, 10.0)[0] == "zzz" {
                hits += 1;
            }
        }
        // uniform chance would be ~22 of 200
        assert!(hits > 60, "zzz chosen only {hits} times out of 200");
    }
}
