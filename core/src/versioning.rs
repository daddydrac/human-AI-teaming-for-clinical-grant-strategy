use serde::Serialize;

#[derive(Clone, Debug, PartialEq, Eq)]
struct LineEdit {
    start: usize,
    end: usize,
    replacement: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct MergeConflict {
    pub base_start_line: usize,
    pub base_end_line: usize,
    pub proposed_text: String,
    pub latest_text: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct MergeResult {
    pub merged_body: String,
    pub clean: bool,
    pub conflicts: Vec<MergeConflict>,
}

pub fn contains_conflict_markers(text:&str)->bool{
    text.lines().any(|line|matches!(line,"<<<<<<< YOUR EDIT"|"||||||| BASE"|"======="|">>>>>>> LATEST"))
}

fn lines(text: &str) -> Vec<String> {
    text.split_inclusive('\n').map(str::to_owned).collect()
}

fn edits(base: &[String], target: &[String]) -> Vec<LineEdit> {
    if base == target {
        return Vec::new();
    }
    // Grant sections are normally hundreds of lines. For an exceptionally large
    // artifact, fail safely into one explicit conflict instead of allocating an
    // unbounded quadratic LCS table.
    if base.len().saturating_mul(target.len()) > 9_000_000 {
        return vec![LineEdit {
            start: 0,
            end: base.len(),
            replacement: target.to_vec(),
        }];
    }
    let columns = target.len() + 1;
    let mut lcs = vec![0u32; (base.len() + 1) * columns];
    for i in (0..base.len()).rev() {
        for j in (0..target.len()).rev() {
            lcs[i * columns + j] = if base[i] == target[j] {
                lcs[(i + 1) * columns + j + 1] + 1
            } else {
                lcs[(i + 1) * columns + j].max(lcs[i * columns + j + 1])
            };
        }
    }
    let mut output = Vec::new();
    let (mut i, mut j) = (0usize, 0usize);
    let mut current: Option<LineEdit> = None;
    let flush = |current: &mut Option<LineEdit>, output: &mut Vec<LineEdit>| {
        if let Some(edit) = current.take() {
            output.push(edit);
        }
    };
    while i < base.len() || j < target.len() {
        if i < base.len() && j < target.len() && base[i] == target[j] {
            flush(&mut current, &mut output);
            i += 1;
            j += 1;
        } else if j < target.len()
            && (i == base.len()
                || lcs[i * columns + j + 1] > lcs[(i + 1) * columns + j])
        {
            current
                .get_or_insert_with(|| LineEdit {
                    start: i,
                    end: i,
                    replacement: Vec::new(),
                })
                .replacement
                .push(target[j].clone());
            j += 1;
        } else {
            let edit = current.get_or_insert_with(|| LineEdit {
                start: i,
                end: i,
                replacement: Vec::new(),
            });
            i += 1;
            edit.end = i;
        }
    }
    flush(&mut current, &mut output);
    output
}

fn append_base(output: &mut String, base: &[String], start: usize, end: usize) {
    for line in &base[start..end] {
        output.push_str(line);
    }
}

fn append_replacement(output: &mut String, replacement: &[String]) {
    for line in replacement {
        output.push_str(line);
    }
}

fn disjoint(left: &LineEdit, right: &LineEdit) -> bool {
    if left.start == left.end && right.start == right.end && left.start == right.start {
        return false;
    }
    left.end <= right.start || right.end <= left.start
}

fn render_region(base: &[String], start: usize, end: usize, region: &[LineEdit]) -> String {
    let mut output = String::new();
    let mut cursor = start;
    for edit in region {
        append_base(&mut output, base, cursor, edit.start);
        append_replacement(&mut output, &edit.replacement);
        cursor = edit.end;
    }
    append_base(&mut output, base, cursor, end);
    output
}

pub fn three_way_merge(base_text: &str, proposed_text: &str, latest_text: &str) -> MergeResult {
    if proposed_text == latest_text {
        return MergeResult { merged_body: proposed_text.to_owned(), clean: true, conflicts: Vec::new() };
    }
    if proposed_text == base_text {
        return MergeResult { merged_body: latest_text.to_owned(), clean: true, conflicts: Vec::new() };
    }
    if latest_text == base_text {
        return MergeResult { merged_body: proposed_text.to_owned(), clean: true, conflicts: Vec::new() };
    }
    let base = lines(base_text);
    let proposed_edits = edits(&base, &lines(proposed_text));
    let latest_edits = edits(&base, &lines(latest_text));
    let (mut proposed_index, mut latest_index, mut cursor) = (0usize, 0usize, 0usize);
    let mut merged = String::new();
    let mut conflicts = Vec::new();

    while proposed_index < proposed_edits.len() || latest_index < latest_edits.len() {
        let proposed = proposed_edits.get(proposed_index);
        let latest = latest_edits.get(latest_index);
        match (proposed, latest) {
            (Some(left), Some(right)) if left == right => {
                append_base(&mut merged, &base, cursor, left.start);
                append_replacement(&mut merged, &left.replacement);
                cursor = left.end;
                proposed_index += 1;
                latest_index += 1;
            }
            (Some(left), Some(right)) if disjoint(left, right) && left.start < right.start => {
                append_base(&mut merged, &base, cursor, left.start);
                append_replacement(&mut merged, &left.replacement);
                cursor = left.end;
                proposed_index += 1;
            }
            (Some(left), Some(right)) if disjoint(left, right) => {
                append_base(&mut merged, &base, cursor, right.start);
                append_replacement(&mut merged, &right.replacement);
                cursor = right.end;
                latest_index += 1;
            }
            (Some(left), Some(right)) => {
                let start = left.start.min(right.start);
                let mut end = left.end.max(right.end);
                let proposed_start = proposed_index;
                let latest_start = latest_index;
                while proposed_index < proposed_edits.len()
                    && (proposed_edits[proposed_index].start < end
                        || (end == start && proposed_edits[proposed_index].start == start))
                {
                    end = end.max(proposed_edits[proposed_index].end);
                    proposed_index += 1;
                }
                while latest_index < latest_edits.len()
                    && (latest_edits[latest_index].start < end
                        || (end == start && latest_edits[latest_index].start == start))
                {
                    end = end.max(latest_edits[latest_index].end);
                    latest_index += 1;
                }
                let proposed_region = render_region(&base, start, end, &proposed_edits[proposed_start..proposed_index]);
                let latest_region = render_region(&base, start, end, &latest_edits[latest_start..latest_index]);
                append_base(&mut merged, &base, cursor, start);
                if proposed_region == latest_region {
                    merged.push_str(&proposed_region);
                } else {
                    merged.push_str("<<<<<<< YOUR EDIT\n");
                    merged.push_str(&proposed_region);
                    if !proposed_region.ends_with('\n') { merged.push('\n'); }
                    merged.push_str("||||||| BASE\n");
                    append_base(&mut merged, &base, start, end);
                    if end > start && !base[end - 1].ends_with('\n') { merged.push('\n'); }
                    merged.push_str("=======\n");
                    merged.push_str(&latest_region);
                    if !latest_region.ends_with('\n') { merged.push('\n'); }
                    merged.push_str(">>>>>>> LATEST\n");
                    conflicts.push(MergeConflict {
                        base_start_line: start + 1,
                        base_end_line: end,
                        proposed_text: proposed_region,
                        latest_text: latest_region,
                    });
                }
                cursor = end;
            }
            (Some(edit), None) | (None, Some(edit)) => {
                append_base(&mut merged, &base, cursor, edit.start);
                append_replacement(&mut merged, &edit.replacement);
                cursor = edit.end;
                if proposed.is_some() { proposed_index += 1; } else { latest_index += 1; }
            }
            (None, None) => break,
        }
    }
    append_base(&mut merged, &base, cursor, base.len());
    MergeResult { clean: conflicts.is_empty(), merged_body: merged, conflicts }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merges_non_overlapping_edits_without_data_loss() {
        let result = three_way_merge("one\ntwo\nthree\n", "ONE\ntwo\nthree\n", "one\ntwo\nTHREE\n");
        assert!(result.clean);
        assert_eq!(result.merged_body, "ONE\ntwo\nTHREE\n");
    }

    #[test]
    fn preserves_overlapping_edits_as_explicit_conflict() {
        let result = three_way_merge("one\ntwo\n", "one\nTWO LOCAL\n", "one\nTWO REMOTE\n");
        assert!(!result.clean);
        assert_eq!(result.conflicts.len(), 1);
        assert!(result.merged_body.contains("<<<<<<< YOUR EDIT"));
        assert!(result.merged_body.contains("TWO REMOTE"));
    }

    #[test]
    fn unchanged_side_accepts_changed_side() {
        assert_eq!(three_way_merge("base", "base", "latest").merged_body, "latest");
        assert_eq!(three_way_merge("base", "proposed", "base").merged_body, "proposed");
    }

    #[test]
    fn detects_only_complete_reconciliation_marker_lines() {
        assert!(contains_conflict_markers("before\n<<<<<<< YOUR EDIT\nafter"));
        assert!(!contains_conflict_markers("A sentence mentions <<<<<<< YOUR EDIT inline."));
    }
}
