//! 将 Unified Diff 文本（`commit_file_diff` 输出）解析为 `ely_git::Stretch`，并支持词级高亮。

use ely_git::{DiffLine, LineKind, Stretch, diff as compute_text_diff, mark_words};

/// 解析单个文件 diff 后的结构化展示数据。
#[derive(Debug, Clone, PartialEq)]
pub enum ParsedDiff {
    /// 可文本渲染的 hunk 列表与增删行统计。
    Text {
        stretches: Vec<Stretch>,
        added: usize,
        removed: usize,
    },
    /// Git 标记为二进制文件。
    Binary,
    /// 无内容变更（例如纯重命名或仅权限变化）。
    Empty,
}

/// 解析 unified diff 字符串为 `ParsedDiff`，并对每个 hunk 自动计算词级高亮 `mark_words`。
pub fn parse_unified_diff(input: &str) -> ParsedDiff {
    if input.trim().is_empty() {
        return ParsedDiff::Empty;
    }

    let mut stretches: Vec<Stretch> = Vec::new();
    let mut current_header: Option<String> = None;
    let mut current_lines: Vec<DiffLine> = Vec::new();
    let mut old_line: usize = 0;
    let mut new_line: usize = 0;
    let mut added: usize = 0;
    let mut removed: usize = 0;
    let mut is_binary = false;

    let flush_hunk = |stretches: &mut Vec<Stretch>,
                      header: &mut Option<String>,
                      lines: &mut Vec<DiffLine>| {
        if let Some(hdr) = header.take() {
            let mut hunk_lines = std::mem::take(lines);
            mark_words(&mut hunk_lines);
            stretches.push(Stretch::Hunk {
                header: hdr.into(),
                lines: hunk_lines,
            });
        }
    };

    for raw_line in input.lines() {
        let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);

        if current_header.is_none() {
            if line.starts_with("Binary files ") || line == "GIT binary patch" {
                is_binary = true;
                continue;
            }
            if let Some((o_start, n_start)) = parse_hunk_header(line) {
                current_header = Some(line.to_string());
                old_line = o_start;
                new_line = n_start;
            }
            continue;
        }

        // 在 hunk 内部遇到新的 `diff --git` 或下一个 `@@` 头时先收尾当前 hunk
        if line.starts_with("diff --git ") {
            flush_hunk(&mut stretches, &mut current_header, &mut current_lines);
            continue;
        }

        if let Some((o_start, n_start)) = parse_hunk_header(line) {
            flush_hunk(&mut stretches, &mut current_header, &mut current_lines);
            current_header = Some(line.to_string());
            old_line = o_start;
            new_line = n_start;
            continue;
        }

        if line.starts_with('\\') {
            // `\ No newline at end of file`
            continue;
        }

        if let Some(rest) = line.strip_prefix(' ') {
            current_lines.push(DiffLine {
                kind: LineKind::Same,
                old: Some(old_line),
                new: Some(new_line),
                text: rest.to_string().into(),
                words: Vec::new(),
            });
            old_line += 1;
            new_line += 1;
        } else if let Some(rest) = line.strip_prefix('+') {
            added += 1;
            current_lines.push(DiffLine {
                kind: LineKind::Added,
                old: None,
                new: Some(new_line),
                text: rest.to_string().into(),
                words: Vec::new(),
            });
            new_line += 1;
        } else if let Some(rest) = line.strip_prefix('-') {
            removed += 1;
            current_lines.push(DiffLine {
                kind: LineKind::Removed,
                old: Some(old_line),
                new: None,
                text: rest.to_string().into(),
                words: Vec::new(),
            });
            old_line += 1;
        } else if line.is_empty() {
            // 某些 git diff 输出可能对空上下文行省略前导空格
            current_lines.push(DiffLine {
                kind: LineKind::Same,
                old: Some(old_line),
                new: Some(new_line),
                text: "".into(),
                words: Vec::new(),
            });
            old_line += 1;
            new_line += 1;
        } else {
            // 碰到非 hunk 行（例如新的文件头），结束当前 hunk
            flush_hunk(&mut stretches, &mut current_header, &mut current_lines);
            if line.starts_with("Binary files ") || line == "GIT binary patch" {
                is_binary = true;
            }
        }
    }

    flush_hunk(&mut stretches, &mut current_header, &mut current_lines);

    if is_binary && stretches.is_empty() {
        return ParsedDiff::Binary;
    }
    if stretches.is_empty() {
        return ParsedDiff::Empty;
    }
    ParsedDiff::Text {
        stretches,
        added,
        removed,
    }
}

/// 从新旧文本直接构造 `Vec<Stretch>`（用于未提交暂存/工作区文件 diff）。
pub fn stretches_from_texts(old: Option<&str>, new: Option<&str>) -> ParsedDiff {
    let old_text = old.unwrap_or("");
    let new_text = new.unwrap_or("");
    if old_text == new_text {
        return ParsedDiff::Empty;
    }
    let stretches = compute_text_diff(old_text, new_text, 3);
    let (added, removed) = count_stretches(&stretches);
    if !stretches
        .iter()
        .any(|s| matches!(s, Stretch::Hunk { .. }))
    {
        return ParsedDiff::Empty;
    }
    ParsedDiff::Text {
        stretches,
        added,
        removed,
    }
}

/// 统计 `Stretch` 中的增删行数。
pub fn count_stretches(stretches: &[Stretch]) -> (usize, usize) {
    stretches
        .iter()
        .filter_map(|stretch| match stretch {
            Stretch::Hunk { lines, .. } => Some(lines),
            Stretch::Folded(_) => None,
        })
        .flatten()
        .fold((0, 0), |(added, removed), line| match line.kind {
            LineKind::Added => (added + 1, removed),
            LineKind::Removed => (added, removed + 1),
            LineKind::Same => (added, removed),
        })
}

/// 解析 `@@ -old[,len] +new[,len] @@...`，返回 `(old_start, new_start)`。
fn parse_hunk_header(line: &str) -> Option<(usize, usize)> {
    let rest = line.strip_prefix("@@ ")?;
    let (ranges, _) = rest.split_once(" @@")?;
    let (old_part, new_part) = ranges.split_once(' ')?;
    let old_start = parse_range_start(old_part, '-')?;
    let new_start = parse_range_start(new_part, '+')?;
    Some((old_start.max(1), new_start.max(1)))
}

fn parse_range_start(part: &str, sign: char) -> Option<usize> {
    let raw = part.strip_prefix(sign)?;
    let start_str = raw.split_once(',').map_or(raw, |(s, _)| s);
    start_str.parse::<usize>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_multi_hunk_unified_diff_with_word_highlights() {
        let raw = "\
diff --git a/src/lib.rs b/src/lib.rs
index 1234567..89abcde 100644
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1,4 +1,4 @@
 fn greet() {
-    println!(\"hello world\");
+    println!(\"hello rust\");
 }
@@ -10 +10,2 @@ fn tail()
 let x = 1;
+let y = 2;
";
        let parsed = parse_unified_diff(raw);
        let ParsedDiff::Text {
            stretches,
            added,
            removed,
        } = parsed
        else {
            panic!("expected text diff, got {parsed:?}");
        };

        assert_eq!(added, 2);
        assert_eq!(removed, 1);
        assert_eq!(stretches.len(), 2);

        let Stretch::Hunk { header, lines } = &stretches[0] else {
            panic!("expected hunk");
        };
        assert_eq!(header.as_ref(), "@@ -1,4 +1,4 @@");
        assert_eq!(lines.len(), 4);
        assert_eq!(lines[0].kind, LineKind::Same);
        assert_eq!(lines[0].old, Some(1));
        assert_eq!(lines[0].new, Some(1));

        assert_eq!(lines[1].kind, LineKind::Removed);
        assert_eq!(lines[1].old, Some(2));
        assert_eq!(lines[1].new, None);
        assert!(!lines[1].words.is_empty(), "expected inline word highlights");

        assert_eq!(lines[2].kind, LineKind::Added);
        assert_eq!(lines[2].old, None);
        assert_eq!(lines[2].new, Some(2));
        assert!(!lines[2].words.is_empty(), "expected inline word highlights");

        let Stretch::Hunk { lines: lines2, .. } = &stretches[1] else {
            panic!("expected second hunk");
        };
        assert_eq!(lines2.len(), 2);
        assert_eq!(lines2[0].old, Some(10));
        assert_eq!(lines2[1].new, Some(11));
    }

    #[test]
    fn parses_binary_and_empty_rename_diffs() {
        let bin = "\
diff --git a/logo.png b/logo.png
new file mode 100644
Binary files a/logo.png and b/logo.png differ
";
        assert_eq!(parse_unified_diff(bin), ParsedDiff::Binary);

        let rename_only = "\
diff --git a/old.rs b/new.rs
rename from old.rs
rename to new.rs
";
        assert_eq!(parse_unified_diff(rename_only), ParsedDiff::Empty);
        assert_eq!(parse_unified_diff(""), ParsedDiff::Empty);
    }

    #[test]
    fn builds_stretches_from_uncommitted_texts() {
        let ParsedDiff::Text {
            stretches,
            added,
            removed,
        } = stretches_from_texts(Some("a\nb\nc\n"), Some("a\nB\nc\nd\n"))
        else {
            panic!("expected text diff");
        };
        assert_eq!(added, 2);
        assert_eq!(removed, 1);
        assert!(!stretches.is_empty());
    }
}
