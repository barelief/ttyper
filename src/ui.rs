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

/// Number of lines the prompt needs when wrapped to `width` cells.
fn prompt_line_count(test: &Test, width: usize) -> usize {
    let mut lines = 1;
    let mut current = 0;
    for word in &test.words {
        let w = word_cell_width(word);
        if current + w > width && current > 0 {
            lines += 1;
            current = 0;
        }
        current += w;
    }
    lines
}

/// Areas for the typing screen: [input box, prompt, status bar], centred
/// vertically. The input box has zero height unless `show_input` is set.
pub fn test_layout(test: &Test, area: Rect, show_input: bool) -> [Rect; 3] {
    let prompt_width = area.width.saturating_sub(2).max(1) as usize;
    let input_height = if show_input { 3 } else { 0 };
    let prompt_height = (prompt_line_count(test, prompt_width) as u16 + 2)
        .min(area.height.saturating_sub(3 + input_height))
        .max(3);
    let total = input_height + prompt_height + 3;
    let top = area.height.saturating_sub(total) / 2;
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(top),
            Constraint::Length(input_height),
            Constraint::Length(prompt_height),
            Constraint::Length(3),
            Constraint::Min(0),
        ])
        .split(area);
    [chunks[1], chunks[2], chunks[3]]
}

/// Where the classic input box sits (for terminal cursor placement).
pub fn input_area(test: &Test, area: Rect) -> Rect {
    test_layout(test, area, true)[0]
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

        let chunks = test_layout(test, area, show_input);
        let prompt_width = area.width.saturating_sub(2).max(1) as usize;

        let target_lines: Vec<Line> = {
            let words = words_to_spans(&test.words, test.current_word, theme);

            let mut lines: Vec<Line> = Vec::new();
            let mut current_line: Vec<Span> = Vec::new();
            let mut current_width = 0;
            for word in words {
                let word_width: usize = word.iter().map(|s| s.width()).sum();

                if current_width + word_width > prompt_width && !current_line.is_empty() {
                    lines.push(Line::from(std::mem::take(&mut current_line)));
                    current_width = 0;
                }

                current_line.extend(word);
                current_width += word_width;
            }
            lines.push(Line::from(current_line));

            lines
        };

        // Input (classic layout only)
        if show_input {
            let input = SizedBlock {
                block: Block::default()
                    .title(Line::from(vec![Span::styled("Input", theme.title)]))
                    .borders(Borders::ALL)
                    .border_type(theme.border_type)
                    .border_style(theme.input_border),
                area: chunks[0],
            };
            input.draw_inner(
                &Line::from(test.words[test.current_word].progress.clone()),
                buf,
            );
            input.render(buf);
        }

        // Prompt
        let mut title = vec![Span::styled("Prompt", theme.title)];
        if !test.focus_letters.is_empty() {
            let letters: String = test
            .focus_letters
                .iter()
                .map(char::to_string)
                .collect::<Vec<_>>()
                .join(" ");
            title.push(Span::styled(
                format!(" drilling: {letters} "),
                theme.status_streak,
            ));
        }
        let target = Paragraph::new(target_lines).block(
            Block::default()
                .title(Line::from(title))
                .borders(Borders::ALL)
                .border_type(theme.border_type)
                .border_style(theme.prompt_border),
        );
        target.render(chunks[1], buf);

        // Status bar: wpm | acc | streak | progress bar
        let status_block = Block::default()
            .borders(Borders::ALL)
            .border_type(theme.border_type)
            .border_style(theme.status_border);
        let inner = status_block.inner(chunks[2]);
        status_block.render(chunks[2], buf);

        let wpm = test
            .live_wpm()
            .map(|w| format!("{w:>3.0}"))
            .unwrap_or_else(|| "  -".into());
        let acc = test
            .live_accuracy()
            .map(|a| format!("{a:>3.0}%"))
            .unwrap_or_else(|| "   -".into());
        let streak = test.streak();
        let streak_style = if streak >= 20 {
            theme.status_streak
        } else {
            theme.status_value
        };
        let mut spans = vec![
            Span::styled(" wpm ", theme.status_label),
            Span::styled(wpm, theme.status_value),
            Span::styled("  acc ", theme.status_label),
            Span::styled(acc, theme.status_value),
            Span::styled("  streak ", theme.status_label),
            Span::styled(format!("{streak:>3}"), streak_style),
            Span::styled(
                format!("  {}/{} ", test.current_word, test.words.len()),
                theme.status_label,
            ),
        ];
        let used: usize = spans.iter().map(|s| s.width()).sum();
        let bar_width = (inner.width as usize).saturating_sub(used + 1);
        if bar_width >= 4 {
            let done = bar_width * test.current_word / test.words.len();
            spans.push(Span::styled("━".repeat(done), theme.progress_done));
            spans.push(Span::styled("━".repeat(bar_width - done), theme.progress_todo));
        }
        buf.set_line(inner.x, inner.y, &Line::from(spans), inner.width);
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
