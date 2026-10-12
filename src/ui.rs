use crate::config::Theme;

use super::test::{results, Test, TestWord};

use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Direction, Layout, Rect},
    symbols::Marker,
    text::{Line, Span, Text},
    widgets::{Axis, Block, Borders, Chart, Dataset, GraphType, Paragraph, Widget},
};
use results::Fraction;

// Convert CPS to WPM (clicks per second)
const WPM_PER_CPS: f64 = 12.0;

// Width of the moving average window for the WPM chart
const WPM_SMA_WIDTH: usize = 10;

#[derive(Clone)]
struct SizedBlock<'a> {
    block: Block<'a>,
    area: Rect,
}

impl SizedBlock<'_> {
    fn render(self, buf: &mut Buffer) {
        self.block.render(self.area, buf)
    }
}

trait UsedWidget: Widget {}
impl UsedWidget for Paragraph<'_> {}

trait DrawInner<T> {
    fn draw_inner(&self, content: T, buf: &mut Buffer);
}

impl DrawInner<&Line<'_>> for SizedBlock<'_> {
    fn draw_inner(&self, content: &Line, buf: &mut Buffer) {
        let inner = self.block.inner(self.area);
        buf.set_line(inner.x, inner.y, content, inner.width);
    }
}

impl<T: UsedWidget> DrawInner<T> for SizedBlock<'_> {
    fn draw_inner(&self, content: T, buf: &mut Buffer) {
        let inner = self.block.inner(self.area);
        content.render(inner, buf);
    }
}

pub trait ThemedWidget {
    fn render(self, area: Rect, buf: &mut Buffer, theme: &Theme);
}

pub struct Themed<'t, W: ?Sized> {
    theme: &'t Theme,
    widget: W,
}
impl<W: ThemedWidget> Widget for Themed<'_, W> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        self.widget.render(area, buf, self.theme)
    }
}
impl Theme {
    pub fn apply_to<W>(&self, widget: W) -> Themed<'_, W> {
        Themed {
            theme: self,
            widget,
        }
    }
}

/// Display width of a word in the prompt, including its trailing space.
fn word_cell_width(word: &TestWord) -> usize {
    word.text.chars().count().max(word.progress.chars().count()) + 1
}

/// Widest the text column gets, so long lines stay readable on wide terminals.
const MAX_COLUMN_WIDTH: u16 = 80;
/// Rows between prompt lines (2 = one blank row between lines).
const LINE_SPACING: u16 = 2;

/// Greedy word wrap: indices of the words on each line.
fn wrap_words(test: &Test, width: usize) -> Vec<Vec<usize>> {
    let mut lines: Vec<Vec<usize>> = vec![Vec::new()];
    let mut current = 0;
    for (i, word) in test.words.iter().enumerate() {
        let w = word_cell_width(word);
        if current + w > width && !lines.last().unwrap().is_empty() {
            lines.push(Vec::new());
            current = 0;
        }
        lines.last_mut().unwrap().push(i);
        current += w;
    }
    lines
}

/// Where everything goes on the typing screen.
pub struct TestLayout {
    /// Classic input box (zero height unless enabled)
    pub input: Rect,
    /// Text column; prompt lines start at its top, `LINE_SPACING` rows apart
    pub column: Rect,
    /// Word indices per wrapped line
    pub lines: Vec<Vec<usize>>,
    /// Single status row under the prompt
    pub status: Rect,
}

impl TestLayout {
    pub fn new(test: &Test, area: Rect, show_input: bool) -> Self {
        let width = area.width.saturating_sub(4).clamp(1, MAX_COLUMN_WIDTH);
        let x = area.x + (area.width - width) / 2;
        let lines = wrap_words(test, width as usize);

        let input_height = if show_input { 3 } else { 0 };
        let text_height = ((lines.len() as u16 - 1) * LINE_SPACING + 1)
            .min(area.height.saturating_sub(input_height + 3))
            .max(1);
        let total = input_height + text_height + 2;
        let top = area.y + area.height.saturating_sub(total) / 2;

        Self {
            input: Rect::new(x, top, width, input_height),
            column: Rect::new(x, top + input_height, width, text_height),
            lines,
            status: Rect::new(x, top + input_height + text_height + 1, width, 1),
        }
    }

    /// Terminal cell of the caret: just before the next letter to type.
    pub fn caret(&self, test: &Test) -> Option<(u16, u16)> {
        let (row, line) = self
            .lines
            .iter()
            .enumerate()
            .find(|(_, l)| l.contains(&test.current_word))?;
        let before: usize = line
            .iter()
            .take_while(|&&i| i != test.current_word)
            .map(|&i| word_cell_width(&test.words[i]))
            .sum();
        let word = &test.words[test.current_word];
        let offset = word
            .progress
            .chars()
            .count()
            .min(word.text.chars().count().max(word.progress.chars().count()));
        let y = self.column.y + row as u16 * LINE_SPACING;
        if y >= self.column.bottom() {
            return None;
        }
        Some((self.column.x + (before + offset) as u16, y))
    }
}

/// Where the classic input box sits (for terminal cursor placement).
pub fn input_area(test: &Test, area: Rect) -> Rect {
    TestLayout::new(test, area, true).input
}

/// The typing screen: the prompt text plus an optional classic input box.
pub struct TestView<'a> {
    pub test: &'a Test,
    pub show_input: bool,
}

impl ThemedWidget for TestView<'_> {
    fn render(self, area: Rect, buf: &mut Buffer, theme: &Theme) {
        let TestView { test, show_input } = self;
        buf.set_style(area, theme.default);

        let layout = TestLayout::new(test, area, show_input);

        // Input (classic layout only)
        if show_input {
            let input = SizedBlock {
                block: Block::default()
                    .title(Line::from(vec![Span::styled("Input", theme.title)]))
                    .borders(Borders::ALL)
                    .border_type(theme.border_type)
                    .border_style(theme.input_border),
                area: layout.input,
            };
            input.draw_inner(
                &Line::from(test.words[test.current_word].progress.clone()),
                buf,
            );
            input.render(buf);
        }

        // Prompt: plain text, one blank row between lines
        let words = words_to_spans(&test.words, test.current_word, theme);
        for (row, line) in layout.lines.iter().enumerate() {
            let y = layout.column.y + row as u16 * LINE_SPACING;
            if y >= layout.column.bottom() {
                break;
            }
            let spans: Vec<Span> = line.iter().flat_map(|&i| words[i].clone()).collect();
            buf.set_line(layout.column.x, y, &Line::from(spans), layout.column.width);
        }

        // Status row: wpm | acc | streak | progress | drilling
        let wpm = test
            .live_wpm()
            .map(|w| format!("{w:>3.0}"))
            .unwrap_or_else(|| "  -".into());
        let acc = test
            .live_accuracy()
            .map(|a| format!("{a:>3.0}%"))
            .unwrap_or_else(|| "  - ".into());
        let streak = test.streak();
        let streak_style = if streak >= 20 {
            theme.status_streak
        } else {
            theme.status_value
        };
        let mut spans = vec![
            Span::styled("wpm ", theme.status_label),
            Span::styled(wpm, theme.status_value),
            Span::styled("   acc ", theme.status_label),
            Span::styled(acc, theme.status_value),
            Span::styled("   streak ", theme.status_label),
            Span::styled(format!("{streak:<3}"), streak_style),
            Span::styled(
                format!("   {}/{}", test.current_word, test.words.len()),
                theme.status_label,
            ),
        ];
        if !test.focus_letters.is_empty() {
            let letters: String = test
                .focus_letters
                .iter()
                .map(char::to_string)
                .collect::<Vec<_>>()
                .join(" ");
            spans.push(Span::styled("   drilling ", theme.status_label));
            spans.push(Span::styled(letters, theme.status_streak));
        }
        let used: usize = spans.iter().map(|s| s.width()).sum();
        let bar_width = (layout.status.width as usize).saturating_sub(used + 3);
        if bar_width >= 8 {
            let done = bar_width * test.current_word / test.words.len();
            spans.push(Span::raw("   "));
            spans.push(Span::styled("━".repeat(done), theme.progress_done));
            spans.push(Span::styled("━".repeat(bar_width - done), theme.progress_todo));
        }
        buf.set_line(
            layout.status.x,
            layout.status.y,
            &Line::from(spans),
            layout.status.width,
        );
    }
}

fn words_to_spans<'a>(
    words: &'a [TestWord],
    current_word: usize,
    theme: &'a Theme,
) -> Vec<Vec<Span<'a>>> {
    let mut spans = Vec::new();

    for word in &words[..current_word] {
        let parts = split_typed_word(word);
        spans.push(word_parts_to_spans(parts, theme));
    }

    let parts_current = split_current_word(&words[current_word]);
    spans.push(word_parts_to_spans(parts_current, theme));

    for word in &words[current_word + 1..] {
        let parts = vec![(word.text.clone(), Status::Untyped)];
        spans.push(word_parts_to_spans(parts, theme));
    }
    spans
}

#[derive(PartialEq, Clone, Copy, Debug)]
enum Status {
    Correct,
    Incorrect,
    CurrentUntyped,
    CurrentCorrect,
    CurrentIncorrect,
    Cursor,
    Untyped,
    Overtyped,
}

fn split_current_word(word: &TestWord) -> Vec<(String, Status)> {
    let mut parts = Vec::new();
    let mut cur_string = String::new();
    let mut cur_status = Status::Untyped;

    let mut progress = word.progress.chars();
    for tc in word.text.chars() {
        let p = progress.next();
        let status = match p {
            None => Status::CurrentUntyped,
            Some(c) => match c {
                c if c == tc => Status::CurrentCorrect,
                _ => Status::CurrentIncorrect,
            },
        };

        if status == cur_status {
            cur_string.push(tc);
        } else {
            if !cur_string.is_empty() {
                parts.push((cur_string, cur_status));
                cur_string = String::new();
            }
            cur_string.push(tc);
            cur_status = status;

            // first currentuntyped is cursor
            if status == Status::CurrentUntyped {
                parts.push((cur_string, Status::Cursor));
                cur_string = String::new();
            }
        }
    }
    if !cur_string.is_empty() {
        parts.push((cur_string, cur_status));
    }
    let overtyped = progress.collect::<String>();
    if !overtyped.is_empty() {
        parts.push((overtyped, Status::Overtyped));
    }
    parts
}

fn split_typed_word(word: &TestWord) -> Vec<(String, Status)> {
    let mut parts = Vec::new();
    let mut cur_string = String::new();
    let mut cur_status = Status::Untyped;

    let mut progress = word.progress.chars();
    for tc in word.text.chars() {
        let p = progress.next();
        let status = match p {
            None => Status::Untyped,
            Some(c) => match c {
                c if c == tc => Status::Correct,
                _ => Status::Incorrect,
            },
        };

        if status == cur_status {
            cur_string.push(tc);
        } else {
            if !cur_string.is_empty() {
                parts.push((cur_string, cur_status));
                cur_string = String::new();
            }
            cur_string.push(tc);
            cur_status = status;
        }
    }
    if !cur_string.is_empty() {
        parts.push((cur_string, cur_status));
    }

    let overtyped = progress.collect::<String>();
    if !overtyped.is_empty() {
        parts.push((overtyped, Status::Overtyped));
    }
    parts
}

fn word_parts_to_spans(parts: Vec<(String, Status)>, theme: &Theme) -> Vec<Span<'_>> {
    let mut spans = Vec::new();
    for (text, status) in parts {
        let style = match status {
            Status::Correct => theme.prompt_correct,
            Status::Incorrect => theme.prompt_incorrect,
            Status::Untyped => theme.prompt_untyped,
            Status::CurrentUntyped => theme.prompt_current_untyped,
            Status::CurrentCorrect => theme.prompt_current_correct,
            Status::CurrentIncorrect => theme.prompt_current_incorrect,
            Status::Cursor => theme.prompt_current_untyped.patch(theme.prompt_cursor),
            Status::Overtyped => theme.prompt_incorrect,
        };

        spans.push(Span::styled(text, style));
    }
    spans.push(Span::styled(" ", theme.prompt_untyped));
    spans
}

impl ThemedWidget for &results::Results {
    fn render(self, area: Rect, buf: &mut Buffer, theme: &Theme) {
        buf.set_style(area, theme.default);

        // Chunks
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(1), Constraint::Length(1)])
            .split(area);
        let res_chunks = Layout::default()
            .direction(Direction::Vertical)
            .margin(1) // Graph looks tremendously better with just a little margin
            .constraints([Constraint::Ratio(1, 3), Constraint::Ratio(2, 3)])
            .split(chunks[0]);
        let info_chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Ratio(1, 2), Constraint::Ratio(1, 2)])
            .split(res_chunks[0]);

        let msg = if self.missed_words.is_empty() {
            "Press 'q' to quit or 'r' for another test"
        } else {
            "Press 'q' to quit, 'r' for another test or 'p' to practice missed words"
        };

        let exit = Span::styled(msg, theme.results_restart_prompt);
        buf.set_span(chunks[1].x, chunks[1].y, &exit, chunks[1].width);

        // Sections
        let mut overview_text = Text::styled("", theme.results_overview);
        overview_text.extend([
            Line::from(format!(
                "Adjusted WPM: {:.1}",
                self.timing.overall_cps * WPM_PER_CPS * f64::from(self.accuracy.overall)
            )),
            Line::from(format!(
                "Accuracy: {:.1}%",
                f64::from(self.accuracy.overall) * 100f64
            )),
            Line::from(format!(
                "Raw WPM: {:.1}",
                self.timing.overall_cps * WPM_PER_CPS
            )),
            Line::from(format!("Correct Keypresses: {}", self.accuracy.overall)),
            Line::from(format!("Best Streak: {} letters", self.best_streak)),
        ]);
        let overview = Paragraph::new(overview_text).block(
            Block::default()
                .title(Span::styled("Overview", theme.title))
                .borders(Borders::ALL)
                .border_type(theme.border_type)
                .border_style(theme.results_overview_border),
        );
        overview.render(info_chunks[0], buf);

        let mut worst_keys: Vec<(&KeyEvent, &Fraction)> = self
            .accuracy
            .per_key
            .iter()
            .filter(|(key, _)| matches!(key.code, KeyCode::Char(_)))
            .collect();
        worst_keys.sort_unstable_by_key(|x| x.1);

        let mut worst_text = Text::styled("", theme.results_worst_keys);
        if !self.weak_letters.is_empty() {
            let all_time = self
                .weak_letters
                .iter()
                .map(|(c, acc)| format!("{} {:.0}%", c, acc * 100.0))
                .collect::<Vec<_>>()
                .join("  ");
            worst_text.extend([Line::from(format!("all-time: {}", all_time))]);
        }
        worst_text.extend(
            worst_keys
                .iter()
                .filter_map(|(key, acc)| {
                    if let KeyCode::Char(character) = key.code {
                        let key_accuracy = f64::from(**acc) * 100.0;
                        if key_accuracy != 100.0 {
                            Some(format!("- {} at {:.1}% accuracy", character, key_accuracy))
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                })
                .take(5)
                .map(Line::from),
        );
        let worst = Paragraph::new(worst_text).block(
            Block::default()
                .title(Span::styled("Worst Keys", theme.title))
                .borders(Borders::ALL)
                .border_type(theme.border_type)
                .border_style(theme.results_worst_keys_border),
        );
        worst.render(info_chunks[1], buf);

        let wpm_sma: Vec<(f64, f64)> = self
            .timing
            .per_event
            .windows(WPM_SMA_WIDTH)
            .enumerate()
            .map(|(i, window)| {
                (
                    (i + WPM_SMA_WIDTH) as f64,
                    window.len() as f64 / window.iter().copied().sum::<f64>() * WPM_PER_CPS,
                )
            })
            .collect();

        // Render the chart if possible
        if !wpm_sma.is_empty() {
            let wpm_sma_min = wpm_sma
                .iter()
                .map(|(_, x)| x)
                .fold(f64::INFINITY, |a, &b| a.min(b));
            let wpm_sma_max = wpm_sma
                .iter()
                .map(|(_, x)| x)
                .fold(f64::NEG_INFINITY, |a, &b| a.max(b));

            let wpm_datasets = vec![Dataset::default()
                .name("WPM")
                .marker(Marker::Braille)
                .graph_type(GraphType::Line)
                .style(theme.results_chart)
                .data(&wpm_sma)];

            let y_label_min = wpm_sma_min as u16;
            let y_label_max = (wpm_sma_max as u16).max(y_label_min + 6);

            let wpm_chart = Chart::new(wpm_datasets)
                .block(Block::default().title(vec![Span::styled("Chart", theme.title)]))
                .x_axis(
                    Axis::default()
                        .title(Span::styled("Keypresses", theme.results_chart_x))
                        .bounds([0.0, self.timing.per_event.len() as f64]),
                )
                .y_axis(
                    Axis::default()
                        .title(Span::styled(
                            "WPM (10-keypress rolling average)",
                            theme.results_chart_y,
                        ))
                        .bounds([wpm_sma_min, wpm_sma_max])
                        .labels(
                            (y_label_min..y_label_max)
                                .step_by(5)
                                .map(|n| Span::raw(format!("{}", n)))
                                .collect(),
                        ),
                );
            wpm_chart.render(res_chunks[1], buf);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    mod split_words {
        use super::Status::*;
        use super::*;

        struct TestCase {
            word: &'static str,
            progress: &'static str,
            expected: Vec<(&'static str, Status)>,
        }

        fn setup(test_case: TestCase) -> (TestWord, Vec<(String, Status)>) {
            let mut word = TestWord::from(test_case.word);
            word.progress = test_case.progress.to_string();

            let expected = test_case
                .expected
                .iter()
                .map(|(s, v)| (s.to_string(), *v))
                .collect::<Vec<_>>();

            (word, expected)
        }

        #[test]
        fn typed_words_split() {
            let cases = vec![
                TestCase {
                    word: "monkeytype",
                    progress: "monkeytype",
                    expected: vec![("monkeytype", Correct)],
                },
                TestCase {
                    word: "monkeytype",
                    progress: "monkeXtype",
                    expected: vec![("monke", Correct), ("y", Incorrect), ("type", Correct)],
                },
                TestCase {
                    word: "monkeytype",
                    progress: "monkeas",
                    expected: vec![("monke", Correct), ("yt", Incorrect), ("ype", Untyped)],
                },
            ];

            for case in cases {
                let (word, expected) = setup(case);
                let got = split_typed_word(&word);
                assert_eq!(got, expected);
            }
        }

        #[test]
        fn current_word_split() {
            let cases = vec![
                TestCase {
                    word: "monkeytype",
                    progress: "monkeytype",
                    expected: vec![("monkeytype", CurrentCorrect)],
                },
                TestCase {
                    word: "monkeytype",
                    progress: "monke",
                    expected: vec![
                        ("monke", CurrentCorrect),
                        ("y", Cursor),
                        ("type", CurrentUntyped),
                    ],
                },
                TestCase {
                    word: "monkeytype",
                    progress: "monkeXt",
                    expected: vec![
                        ("monke", CurrentCorrect),
                        ("y", CurrentIncorrect),
                        ("t", CurrentCorrect),
                        ("y", Cursor),
                        ("pe", CurrentUntyped),
                    ],
                },
            ];

            for case in cases {
                let (word, expected) = setup(case);
                let got = split_current_word(&word);
                assert_eq!(got, expected);
            }
        }
    }
}
