#[derive(Debug, Clone)]
pub struct TextChunk {
    pub ordinal: usize,
    pub text: String,
    pub start_word: usize,
    pub end_word: usize,
}

pub fn chunk_text(text: &str, target_words: usize, overlap_words: usize) -> Vec<TextChunk> {
    let target = target_words.max(64);
    let overlap = overlap_words.min(target / 2);
    let words: Vec<&str> = text.split_whitespace().collect();
    if words.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut start = 0usize;
    let mut ordinal = 0usize;
    while start < words.len() {
        let mut end = (start + target).min(words.len());
        if end < words.len() {
            // Prefer a sentence-ish boundary near the target without scanning too far.
            let floor = start + (target * 3 / 4);
            let mut candidate = end;
            for i in (floor.min(end)..end).rev() {
                let w = words[i];
                if w.ends_with('.') || w.ends_with('?') || w.ends_with('!') || w.ends_with(';') {
                    candidate = i + 1;
                    break;
                }
            }
            end = candidate.max(start + 1);
        }
        out.push(TextChunk {
            ordinal,
            text: words[start..end].join(" "),
            start_word: start,
            end_word: end,
        });
        ordinal += 1;
        if end == words.len() {
            break;
        }
        start = end.saturating_sub(overlap);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn overlaps_without_losing_tail() {
        let text = (0..1000)
            .map(|i| format!("w{i}"))
            .collect::<Vec<_>>()
            .join(" ");
        let chunks = chunk_text(&text, 100, 20);
        assert!(chunks.len() > 1);
        assert_eq!(chunks.first().unwrap().start_word, 0);
        assert_eq!(chunks.last().unwrap().end_word, 1000);
        for pair in chunks.windows(2) {
            assert!(pair[1].start_word < pair[0].end_word);
        }
    }
}
