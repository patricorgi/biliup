use similar::{Algorithm, DiffOp, capture_diff_slices_deadline};
use std::time::{Duration, Instant};

pub const MAX_LINE: usize = 16;

#[derive(Clone, Debug)]
pub struct Cue {
    pub start: i64,
    pub end: i64,
    pub text: String,
}

fn parse_time(text: &str) -> Option<i64> {
    let mut parts = text.trim().split([':', ',', '.']);
    let [h, m, s, ms] = [parts.next()?, parts.next()?, parts.next()?, parts.next()?];
    if parts.next().is_some() || h.len() > 5 || m.len() != 2 || s.len() != 2 || ms.len() != 3 {
        return None;
    }
    Some(
        h.parse::<i64>().ok()? * 3_600_000
            + m.parse::<i64>().ok()? * 60_000
            + s.parse::<i64>().ok()? * 1000
            + ms.parse::<i64>().ok()?,
    )
}

pub fn parse(srt: &str) -> Vec<Cue> {
    let normalized = srt.replace("\r\n", "\n");
    normalized
        .split("\n\n")
        .filter_map(|block| {
            let mut lines = block.lines();
            let (start, end) = lines.find_map(|line| line.split_once(" --> "))?;
            let (start, end) = (parse_time(start)?, parse_time(end)?);
            let text = lines.collect::<Vec<_>>().join("\n");
            (start >= 0 && end > start && !text.trim().is_empty()).then_some(Cue {
                start,
                end,
                text,
            })
        })
        .collect()
}

fn clock(ms: i64) -> String {
    format!(
        "{:02}:{:02}:{:02},{:03}",
        ms / 3_600_000,
        ms / 60_000 % 60,
        ms / 1000 % 60,
        ms % 1000
    )
}

pub fn render(cues: &[Cue]) -> String {
    let mut out = String::new();
    use std::fmt::Write;
    for (i, cue) in cues.iter().enumerate() {
        let _ = writeln!(
            out,
            "{}\n{} --> {}\n{}\n",
            i + 1,
            clock(cue.start),
            clock(cue.end),
            cue.text
        );
    }
    out
}

fn clean(text: &str) -> String {
    let mut input = text.chars().peekable();
    let mut previous = None;
    let mut result = String::new();
    while let Some(ch) = input.next() {
        let is_decimal = ch == '.'
            && previous.is_some_and(|before: char| before.is_ascii_digit())
            && input.peek().is_some_and(char::is_ascii_digit);
        match ch {
            ',' | '，' => result.push(' '),
            '。' | '.' | '．' if !is_decimal => {}
            _ => result.push(ch),
        }
        previous = Some(ch);
    }
    result.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub fn lines(text: &str) -> Vec<String> {
    lines_with_limit(text, MAX_LINE)
}

fn lines_with_limit(text: &str, limit: usize) -> Vec<String> {
    let mut out = Vec::new();
    for line in text.lines() {
        let chars: Vec<_> = clean(line).chars().collect();
        let mut remaining = chars.as_slice();
        while !remaining.is_empty() {
            let mut count = remaining.len().min(limit);
            if remaining.len() > limit {
                let candidates = &remaining[limit / 2..limit];
                if let Some(index) = candidates
                    .iter()
                    .rposition(|ch| matches!(ch, ' ' | '？' | '?' | '！' | '!'))
                {
                    count = limit / 2 + index + 1;
                }
            }
            let line = remaining[..count].iter().collect::<String>();
            let line = line.trim();
            if line.chars().any(char::is_alphanumeric) {
                out.push(line.to_owned());
            }
            remaining = &remaining[count..];
        }
    }
    out
}

pub fn automatic(reference: &[Cue]) -> Vec<String> {
    automatic_with_limit(reference, MAX_LINE)
}

pub(super) fn automatic_with_limit(reference: &[Cue], limit: usize) -> Vec<String> {
    let mut blocks: Vec<Cue> = Vec::new();
    for cue in reference {
        if let Some(last) = blocks.last_mut() {
            let left = last.text.trim_end();
            let right = cue.text.trim_start();
            let space = left.ends_with([',', '，'])
                || (left.ends_with(|ch: char| ch.is_ascii_alphanumeric())
                    && right.starts_with(|ch: char| ch.is_ascii_alphanumeric()));
            let candidate = format!("{left}{}{right}", if space { " " } else { "" });
            if cue.start - last.end <= 300
                && cue.end - last.start <= 3_500
                && clean(&candidate).chars().count() <= limit
                && !left.ends_with(['。', '.', '！', '？', '!', '?'])
            {
                last.end = cue.end;
                last.text = candidate;
                continue;
            }
        }
        blocks.push(cue.clone());
    }
    blocks.iter().flat_map(|cue| lines_with_limit(&cue.text, limit)).collect()
}

fn key(ch: char) -> Option<char> {
    ch.is_alphanumeric()
        .then(|| ch.to_lowercase().next().unwrap_or(ch))
}

pub fn align(reference: &[Cue], lines: &[String], duration: i64) -> Vec<Cue> {
    align_cues(reference, lines, duration, true)
}

/// Look again only where the edited alignment would otherwise fill a long
/// recognition gap. Ordinary corrections need no extra inference.
pub fn refinement_windows(reference: &[Cue], lines: &[String], duration: i64) -> Vec<(i64, i64)> {
    let estimated = align_cues(reference, lines, duration, false);
    let mut suspicious = estimated
        .iter()
        .filter(|cue| cue.end - cue.start >= 3_800)
        .peekable();
    let mut windows = Vec::new();
    let mut previous_end = 0;
    for (start, end) in reference
        .iter()
        .map(|cue| (cue.start, cue.end))
        .chain(std::iter::once((duration, duration)))
    {
        while suspicious
            .peek()
            .is_some_and(|cue| cue.end - previous_end < 1_800)
        {
            suspicious.next();
        }
        if start - previous_end >= 1_800
            && suspicious
                .peek()
                .is_some_and(|cue| start.min(cue.end) - previous_end.max(cue.start) >= 1_800)
        {
            let mut left = previous_end;
            while left < start && windows.len() < 12 {
                let right = (left + 12_000).min(start);
                windows.push((left, right));
                left = right;
            }
        }
        previous_end = previous_end.max(end);
    }
    windows
}

fn align_cues(reference: &[Cue], lines: &[String], duration: i64, tighten: bool) -> Vec<Cue> {
    let mut source = Vec::new();
    let mut times = Vec::new();
    for cue in reference {
        let letters: Vec<_> = cue.text.chars().filter_map(key).collect();
        let len = letters.len() as i64;
        let width = cue.end - cue.start;
        for (index, ch) in letters.into_iter().enumerate() {
            let i = index as i64;
            source.push(ch);
            times.push((
                cue.start + width * i / len,
                cue.start + width * (i + 1) / len,
            ));
        }
    }
    if times.is_empty() {
        return Vec::new();
    }
    let mut target = Vec::new();
    let mut ranges = Vec::new();
    for line in lines {
        let start = target.len();
        target.extend(line.chars().filter_map(key));
        ranges.push(start..target.len());
    }
    let mut matched = vec![None; target.len()];
    for op in capture_diff_slices_deadline(
        Algorithm::Myers,
        &source,
        &target,
        Some(Instant::now() + Duration::from_millis(400)),
    ) {
        if let DiffOp::Equal {
            old_index,
            new_index,
            len,
        } = op
        {
            for offset in 0..len {
                matched[new_index + offset] = Some(times[old_index + offset]);
            }
        }
    }
    let anchors = tighten.then(|| matched.clone());
    let mut index = 0;
    let mut last = times[0].0;
    while index < matched.len() {
        if let Some((_, end)) = matched[index] {
            last = end;
            index += 1;
            continue;
        }
        let start = index;
        while index < matched.len() && matched[index].is_none() {
            index += 1;
        }
        let next = matched
            .get(index)
            .and_then(|time| *time)
            .map_or(times[times.len() - 1].1, |(start, _)| start)
            .max(last);
        let len = (index - start) as i64;
        for (offset, item) in matched[start..index].iter_mut().enumerate() {
            let offset = offset as i64;
            *item = Some((
                last + (next - last) * offset / len,
                last + (next - last) * (offset + 1) / len,
            ));
        }
        last = next;
    }
    let mut end = 0;
    let mut result = Vec::with_capacity(lines.len());
    let min_span = (duration / lines.len().max(1) as i64).clamp(1, 100);
    for (index, (line, range)) in lines.iter().zip(ranges).enumerate() {
        if range.is_empty() || end >= duration {
            continue;
        }
        let (mut start, _) = matched[range.start].expect("aligned characters");
        let (_, mut last) = matched[range.end - 1].expect("aligned characters");
        if let Some(anchors) = &anchors {
            if let Some((left, right)) = speech_span(&anchors[range.clone()]) {
                start = left;
                last = right;
            } else {
                // An entirely rewritten line has no reliable word timestamps.
                // Keep it brief near the next anchor rather than showing it
                // throughout the intervening pause.
                start = start.max(last - (range.len() as i64 * 180).clamp(550, 2_800));
            }
        }
        let maximum = (duration - min_span * (lines.len() - 1 - index) as i64)
            .max(end + 1)
            .min(duration);
        let start = start.max(end).min((maximum - min_span).max(end));
        end = last.max(start + min_span).min(maximum);
        result.push(Cue {
            start,
            end,
            text: line.clone(),
        });
    }
    result
}

/// Pick the strongest cluster of equal characters in the same stretch of
/// speech. Isolated matches in a previous sentence must not pull a cue back.
fn speech_span(anchors: &[Option<(i64, i64)>]) -> Option<(i64, i64)> {
    let mut clusters: Vec<(usize, usize, i64, i64, usize)> = Vec::new();
    for (index, timing) in anchors.iter().enumerate() {
        let Some((start, end)) = timing else { continue };
        if let Some(last) = clusters.last_mut()
            && *start - last.3 <= 850
        {
            last.1 = index;
            last.3 = last.3.max(*end);
            last.4 += 1;
            continue;
        }
        clusters.push((index, index, *start, *end, 1));
    }
    let (first, last, start, end, _) = clusters.into_iter().max_by_key(|cluster| cluster.4)?;
    let leading = (first as i64 * 120).min(360);
    let trailing = ((anchors.len() - last - 1) as i64 * 120).min(360);
    Some((start - leading, end + trailing))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corrected_prefix_does_not_fill_the_previous_pause() {
        let reference = [
            Cue {
                start: 95_000,
                end: 96_540,
                text: "这是上一句啊".into(),
            },
            Cue {
                start: 103_230,
                end: 105_250,
                text: "我在我的球又白放".into(),
            },
        ];
        let lines = lines(
            "这是上一句啊
哇塞我的球又白放了",
        );
        let result = align(&reference, &lines, 106_000);
        assert!(result[1].start >= 102_870);
        assert_eq!(result[1].text, "哇塞我的球又白放了");
    }

    #[test]
    fn recovered_phrase_beats_an_isolated_match_before_a_pause() {
        let reference = [
            Cue {
                start: 38_520,
                end: 38_690,
                text: "哦呦".into(),
            },
            Cue {
                start: 47_430,
                end: 49_060,
                text: "他这个怎么会飞啊".into(),
            },
            Cue {
                start: 49_220,
                end: 49_760,
                text: "哎呦喂".into(),
            },
        ];
        let lines = lines(
            "哦呦他这个怎么会飞啊
哎呦喂",
        );
        let result = align(&reference, &lines, 51_000);
        assert!(result[0].start >= 47_000 && result[0].end < 49_220);
    }
}
