//! The parts of a page that match a question.
//!
//! The Markdown is split into blocks at blank lines. Each block is scored against the words of
//! the query with BM25; the headings above a block count as part of its text. The best blocks
//! are returned in page order under their headings, with `[...]` where blocks are left out.

use std::collections::{HashMap, HashSet};

/// A longer block is split at line ends, so that one list, table or PDF page cannot fill the
/// whole answer. A small answer has smaller blocks: at least five fit.
const BLOCK_CHARS: usize = 1_000;
const SKIPPED: &str = "[...]";

struct Block {
    /// The heading lines above the block, outermost first.
    headings: Vec<String>,
    text: String,
}

/// The blocks of `content` that best match `query`, in at most `budget` characters. `None` when
/// the content already fits, or when no block has a word of the query.
pub(crate) fn focus(content: &str, query: &str, budget: usize) -> Option<String> {
    if content.chars().count() <= budget {
        return None;
    }
    let query: HashSet<String> = words(query).collect();
    let mut blocks = blocks(content, BLOCK_CHARS.min(budget / 5));
    // How often each query word is in each block, and the length of the block in words.
    let counts: Vec<(HashMap<&str, f64>, f64)> = blocks
        .iter()
        .map(|block| {
            let mut count = HashMap::new();
            let mut length = 0.0;
            let headings = block.headings.iter().flat_map(|heading| words(heading));
            for word in headings.chain(words(&visible(&block.text))) {
                length += 1.0;
                if let Some(word) = query.get(&word) {
                    *count.entry(word.as_str()).or_insert(0.0) += 1.0;
                }
            }
            (count, length)
        })
        .collect();
    let mut within: HashMap<&str, f64> = HashMap::new();
    for word in counts.iter().flat_map(|(count, _)| count.keys()) {
        *within.entry(word).or_insert(0.0) += 1.0;
    }
    let total = blocks.len() as f64;
    let average = counts.iter().map(|(_, length)| length).sum::<f64>() / total;
    let scores: Vec<f64> = counts
        .iter()
        .map(|(count, length)| {
            let word = |(word, found): (&&str, &f64)| {
                let rarity = (1.0 + (total - within[word] + 0.5) / (within[word] + 0.5)).ln();
                rarity * found * 2.2 / (found + 1.2 * (0.25 + 0.75 * length / average))
            };
            count.iter().map(word).sum()
        })
        .collect();
    let mut order: Vec<usize> = (0..blocks.len())
        .filter(|index| scores[*index] > 0.0)
        .collect();
    order.sort_by(|first, second| scores[*second].total_cmp(&scores[*first]));
    // Each block is counted with all its headings and a marker, and the last marker is set
    // aside, so the answer cannot exceed the budget.
    let mut left = budget.saturating_sub(SKIPPED.len() + 2);
    let mut chosen = Vec::new();
    for index in order {
        let block = &blocks[index];
        let headings = block.headings.iter();
        let around = headings
            .map(|heading| heading.chars().count() + 2)
            .sum::<usize>()
            + 9;
        let cost = block.text.chars().count() + around;
        if cost <= left {
            chosen.push(index);
            left -= cost;
        } else if chosen.is_empty() && left > around {
            // The best block alone is too long: keep its start.
            blocks[index].text = block.text.chars().take(left - around).collect();
            chosen.push(index);
            break;
        }
    }
    if chosen.is_empty() {
        return None;
    }
    chosen.sort_unstable();
    let mut parts: Vec<&str> = Vec::new();
    let mut shown: &[String] = &[];
    let mut next = 0;
    for index in chosen {
        let block = &blocks[index];
        if index > next {
            parts.push(SKIPPED);
        }
        let open = shown.iter().zip(&block.headings);
        let open = open.take_while(|(shown, heading)| shown == heading).count();
        parts.extend(block.headings[open..].iter().map(String::as_str));
        parts.push(&block.text);
        shown = &block.headings;
        next = index + 1;
    }
    if next < blocks.len() {
        parts.push(SKIPPED);
    }
    Some(parts.join("\n\n"))
}

/// Lowercase words, without a plural "s" so that "timeouts" matches "timeout".
fn words(text: &str) -> impl Iterator<Item = String> + '_ {
    text.split(|letter: char| !letter.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(|word| {
            let word = word.to_lowercase();
            match word.strip_suffix('s') {
                Some(stem) if stem.len() > 2 => stem.to_owned(),
                _ => word,
            }
        })
}

/// The text without the targets of its Markdown links, which a reader does not see.
fn visible(text: &str) -> String {
    let mut visible = String::new();
    let mut rest = text;
    while let Some(at) = rest.find("](") {
        visible.push_str(&rest[..at]);
        visible.push(' ');
        rest = &rest[at..];
        rest = rest.find(')').map_or("", |end| &rest[end + 1..]);
    }
    visible + rest
}

fn blocks(content: &str, limit: usize) -> Vec<Block> {
    let mut blocks = Vec::new();
    // The open headings with their levels.
    let mut headings: Vec<(usize, String)> = Vec::new();
    let mut paragraphs = paragraphs(content).into_iter().peekable();
    while let Some(mut lines) = paragraphs.next() {
        // A heading stands alone, or directly above its text in Markdown that is served as
        // written.
        if let Some(level) = level(lines[0]) {
            headings.retain(|(open, _)| *open < level);
            headings.push((level, lines.remove(0).to_owned()));
            if lines.is_empty() {
                continue;
            }
        }
        let mut text = lines.join("\n");
        // A sentence that ends with a colon introduces the paragraph after it.
        while text.ends_with(':')
            && let Some(next) = paragraphs.next_if(|next| level(next[0]).is_none())
        {
            text = format!("{text}\n\n{}", next.join("\n"));
        }
        let path: Vec<String> = headings.iter().map(|(_, line)| line.clone()).collect();
        blocks.extend(split(&text, limit).into_iter().map(|text| Block {
            headings: path.clone(),
            text,
        }));
    }
    blocks
}

/// The lines of each paragraph. A blank line ends a paragraph, but not inside a code fence.
fn paragraphs(content: &str) -> Vec<Vec<&str>> {
    let mut paragraphs = vec![Vec::new()];
    let mut fenced = false;
    for line in content.lines() {
        let start = line.trim_start();
        let fence = start.starts_with("```") || start.starts_with("~~~");
        fenced ^= fence;
        if !(fenced || fence || !line.trim().is_empty()) {
            paragraphs.push(Vec::new());
        } else if let Some(open) = paragraphs.last_mut() {
            open.push(line);
        }
    }
    paragraphs.retain(|lines| !lines.is_empty());
    paragraphs
}

/// The level of a heading line: the number of `#`, or 1 for the page label of a PDF.
fn level(line: &str) -> Option<usize> {
    let marks = line.bytes().take_while(|byte| *byte == b'#').count();
    if (1..=6).contains(&marks) && line[marks..].starts_with(' ') {
        Some(marks)
    } else {
        (line.starts_with("[Page ") && line.ends_with(']')).then_some(1)
    }
}

/// Splits a block of more than `limit` characters at line ends. Code stays whole. Each later
/// part of a table starts with the header rows of the table, so its cells keep their meaning.
fn split(text: &str, limit: usize) -> Vec<String> {
    if text.chars().count() <= limit || text.contains("```") || text.contains("~~~") {
        return vec![text.to_owned()];
    }
    let lines: Vec<&str> = text.lines().collect();
    let header = lines.windows(2).find(|rows| {
        rows[0].starts_with('|') && rows[1].starts_with('|') && rows[1].contains("--")
    });
    let mut parts: Vec<Vec<&str>> = vec![Vec::new()];
    let mut size = 0;
    for line in &lines {
        let length = line.chars().count() + 1;
        if size > 0 && size + length > limit {
            let rows = header.filter(|_| line.starts_with('|')).unwrap_or(&[]);
            size = rows.iter().map(|row| row.chars().count() + 1).sum();
            parts.push(rows.to_vec());
        }
        size += length;
        if let Some(part) = parts.last_mut() {
            part.push(line);
        }
    }
    parts.into_iter().map(|part| part.join("\n")).collect()
}

#[cfg(test)]
#[path = "focus.test.rs"]
mod tests;
