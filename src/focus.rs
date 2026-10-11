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
    while let Some(lines) = paragraphs.next() {
        if let Some(level) = level(&lines) {
            headings.retain(|(open, _)| *open < level);
            headings.push((level, lines[0].to_owned()));
            continue;
        }
        let mut text = lines.join("\n");
        // A sentence that ends with a colon introduces the paragraph after it.
        while text.ends_with(':')
            && let Some(next) = paragraphs.next_if(|next| level(next).is_none())
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

/// The level of a heading paragraph: the number of `#`, or 1 for the page label of a PDF.
fn level(lines: &[&str]) -> Option<usize> {
    let [line] = lines else { return None };
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
mod tests {
    use super::*;

    fn page() -> String {
        let filler = |topic: &str| {
            (1..=12)
                .map(|number| format!("Sentence {number} about {topic} says nothing of note."))
                .collect::<Vec<_>>()
                .join("\n\n")
        };
        [
            "# Client guide",
            &filler("installation"),
            "## Retries",
            &filler("retries"),
            "## Timeouts",
            "The default is 30 seconds. Set another value like this:",
            "```toml\n[client]\n\nwait = 5\n```",
            &filler("limits"),
            "## Proxies",
            &filler("proxies"),
        ]
        .join("\n\n")
    }

    #[test]
    fn returns_the_matching_parts_in_page_order_under_their_headings() {
        let page = page();
        let focused = focus(&page, "What is the default timeout?", 400).unwrap();
        assert!(
            focused.starts_with(
                "[...]\n\n# Client guide\n\n## Timeouts\n\nThe default is 30 seconds. \
                 Set another value like this:\n\n```toml\n[client]\n\nwait = 5\n```\n\n"
            ),
            "{focused}"
        );
        assert!(focused.ends_with("[...]"));
        assert!(!focused.contains("## Retries") && !focused.contains("## Proxies"));
        for budget in [60, 150, 400, 900, 2_000] {
            let focused = focus(&page, "default timeout sentence proxies", budget);
            let length = focused.map_or(0, |focused| focused.chars().count());
            assert!(length <= budget, "{length} > {budget}");
        }
    }

    #[test]
    fn keeps_the_whole_page_when_it_fits_or_nothing_matches() {
        let page = page();
        assert!(focus(&page, "default timeout", page.len()).is_none());
        assert!(focus(&page, "kubernetes", 400).is_none());
        assert!(focus(&page, "", 400).is_none());
        let link = "[Sentence](https://example.org/kubernetes \"Kubernetes\") 1 ";
        let linked = page.replace("Sentence 1 ", link);
        assert!(linked.contains("kubernetes") && focus(&linked, "kubernetes", 400).is_none());
    }

    #[test]
    fn a_part_of_a_long_table_or_document_keeps_its_context() {
        let rows = (1..=200)
            .map(|number| format!("| item{number} | {} |", number * 7))
            .collect::<Vec<_>>();
        let table = format!(
            "Prices:\n\n| Name | Price |\n| --- | --- |\n{}",
            rows.join("\n")
        );
        let focused = focus(&table, "item150", 300).unwrap();
        assert!(
            focused.starts_with("[...]\n\n| Name | Price |\n| --- | --- |\n| item1"),
            "{focused}"
        );
        assert!(focused.contains("| item150 | 1050 |"));

        let lines = |page: usize| {
            (1..=40)
                .map(|line| format!("Page {page} line {line} of the report."))
                .collect::<Vec<_>>()
                .join("\n")
        };
        let pdf = format!(
            "[Page 1]\n\n{}\n\n[Page 2]\n\n{}\nThe melting point is 1538 degrees.\n{}",
            lines(1),
            lines(2),
            lines(2)
        );
        let focused = focus(&pdf, "melting point", 1_200).unwrap();
        assert!(focused.contains("[Page 2]") && !focused.contains("[Page 1]"));
        assert!(focused.contains("The melting point is 1538 degrees."));
    }
}
