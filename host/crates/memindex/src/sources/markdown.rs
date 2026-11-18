//! A markdown file to one doc: the note's own headings cut it, and every chunk carries the
//! path it can quote back ("Title › Chapter › Section") so a hit reads with its context.
//!
//! The reading order for a date is the front-matter `date:` key, else the first `YYYY-MM-DD`
//! in the file name, else the modification time the caller supplies. The first level-one heading names the
//! doc (that heading is then not repeated in the heading path), else the front-matter `title:`,
//! else the file stem. Text under each a level-1 to level-3 heading becomes a chunk of at most
//! [`MAX_CHUNK_CHARS`] characters; neighbouring short sections merge toward
//! [`TARGET_CHUNK_CHARS`], and a section longer than the cap splits at paragraph edges, with
//! the last paragraph of the previous piece repeated at the head of the next one when it
//! fits. A file with no searchable text still gets one header-only chunk, so its title is
//! findable.

use super::{
    file_stem, find_date, finish_doc, paragraphs, wrap_capped, ChunkOut, DocOut, MAX_CHUNK_CHARS,
    TARGET_CHUNK_CHARS,
};

/// Chunk one markdown file. `path` is the file's name relative to its folder (kept as the
/// doc's key and locator), `mtime_ms` its modification time in Unix milliseconds.
pub fn chunk_markdown(path: &str, content: &str, mtime_ms: i64) -> DocOut {
    let (front, body) = split_front_matter(content);
    let fm_title = front
        .iter()
        .find(|(k, _)| k == "title")
        .map(|(_, v)| v.clone());
    let fm_date = front
        .iter()
        .find(|(k, _)| k == "date")
        .and_then(|(_, v)| find_date(v));

    let h1 = body
        .lines()
        .map(str::trim_start)
        .find_map(|l| l.strip_prefix("# ").map(str::to_owned))
        .filter(|t| !t.trim().is_empty());
    let title = h1
        .clone()
        .or_else(|| fm_title.filter(|t| !t.trim().is_empty()))
        .unwrap_or_else(|| file_stem(path));
    let at = fm_date
        .or_else(|| find_date(&file_stem(path)))
        .unwrap_or(mtime_ms);

    let mut sections: Vec<(Vec<String>, String)> = Vec::new();
    let mut headings: Vec<String> = Vec::new();
    let mut cur_body: Vec<&str> = Vec::new();
    let mut started = false;
    for line in body.lines() {
        if let Some((level, text)) = heading_of(line) {
            if started {
                sections.push((path_of(&headings, &title), cur_body.join("\n")));
            }
            headings.truncate((level - 1) as usize);
            headings.push(text);
            cur_body.clear();
            started = true;
        } else {
            if !line.trim().is_empty() {
                started = true;
            }
            cur_body.push(line);
        }
    }
    if started {
        sections.push((path_of(&headings, &title), cur_body.join("\n")));
    }

    // One candidate chunk per section, long sections pre-split with a paragraph overlap.
    let mut cand: Vec<(Vec<String>, String)> = Vec::new();
    for (hpath, raw) in sections {
        let body = raw.trim().to_string();
        if body.is_empty() {
            continue;
        }
        let head_len = header_len(&title, &hpath);
        if body.chars().count() + head_len <= MAX_CHUNK_CHARS {
            cand.push((hpath, body));
            continue;
        }
        let cap = (MAX_CHUNK_CHARS - head_len).max(1);
        let mut prev_tail = String::new();
        for piece in pack_paragraphs(&body, cap) {
            let mut text = piece;
            if !prev_tail.is_empty() {
                let room = cap - text.chars().count();
                if prev_tail.chars().count() + 2 <= room {
                    text = format!("{prev_tail}\n\n{text}");
                }
            }
            prev_tail = paragraphs(&text)
                .last()
                .map(|s| s.to_string())
                .unwrap_or_default();
            cand.push((hpath.clone(), text));
        }
    }

    // Merge short neighbours toward the target size; a merged chunk keeps the first path.
    let mut merged: Vec<(Vec<String>, String)> = Vec::new();
    for (hpath, body) in cand {
        if let Some(last) = merged.last_mut() {
            let room = MAX_CHUNK_CHARS - header_len(&title, &last.0);
            let joined = last.1.chars().count() + 2 + body.chars().count();
            if joined <= room && last.1.chars().count() < TARGET_CHUNK_CHARS {
                last.1 = format!("{}\n\n{body}", last.1);
                continue;
            }
        }
        merged.push((hpath, body));
    }

    let mut chunks: Vec<ChunkOut> = merged
        .into_iter()
        .map(|(hpath, body)| ChunkOut {
            ord: 0,
            heading: hpath.join(" › "),
            text: format!("{}\n{body}", build_header(&title, &hpath)),
            at,
            anchor: None,
        })
        .collect();
    if chunks.is_empty() {
        chunks.push(ChunkOut {
            ord: 0,
            heading: String::new(),
            text: build_header(&title, &[]),
            at,
            anchor: None,
        });
    }
    finish_doc(path.to_string(), title, path.to_string(), at, at, chunks)
}

/// `Some(level, text)` for an a level-1 to level-3 line, else `None`. Deeper headings stay in the body.
fn heading_of(line: &str) -> Option<(u8, String)> {
    let trimmed = line.trim_start();
    for (level, prefix) in [(1u8, "# "), (2, "## "), (3, "### ")] {
        if let Some(rest) = trimmed.strip_prefix(prefix) {
            let text = rest.trim().trim_start_matches('#').trim().to_string();
            if !text.is_empty() {
                return Some((level, text));
            }
            return None;
        }
    }
    None
}

/// The heading path for a section: the stack, minus its leading heading when that is the
/// doc's title (the title already stands at the head of the header line).
fn path_of(headings: &[String], title: &str) -> Vec<String> {
    let skip = usize::from(headings.first().is_some_and(|h| h == title));
    headings[skip..].to_vec()
}

fn build_header(title: &str, hpath: &[String]) -> String {
    if hpath.is_empty() {
        title.to_string()
    } else {
        format!("{title} › {}", hpath.join(" › "))
    }
}

fn header_len(title: &str, hpath: &[String]) -> usize {
    build_header(title, hpath).chars().count() + 1
}

/// Pack paragraphs into pieces of at most `cap` characters; a paragraph over the cap is
/// word-wrapped into the packing. Order kept; pieces join later with blank lines.
fn pack_paragraphs(body: &str, cap: usize) -> Vec<String> {
    let mut pieces: Vec<String> = Vec::new();
    let mut cur = String::new();
    for para in paragraphs(body) {
        if para.chars().count() > cap {
            if !cur.is_empty() {
                pieces.push(std::mem::take(&mut cur));
            }
            for w in wrap_capped(para, cap) {
                if !cur.is_empty() && cur.chars().count() + 2 + w.chars().count() <= cap {
                    cur = format!("{cur}\n\n{w}");
                } else {
                    if !cur.is_empty() {
                        pieces.push(std::mem::take(&mut cur));
                    }
                    cur = w;
                }
            }
            continue;
        }
        if !cur.is_empty() && cur.chars().count() + 2 + para.chars().count() > cap {
            pieces.push(std::mem::take(&mut cur));
        }
        cur = if cur.is_empty() {
            para.to_string()
        } else {
            format!("{cur}\n\n{para}")
        };
    }
    if !cur.is_empty() {
        pieces.push(cur);
    }
    pieces
}

/// Whether the front matter marks the note retired: a `status` (top level, or indented under
/// `metadata`) of `archived` or `superseded`. Such a note is not indexed, so a retired memory
/// cannot be found again by a folder scan.
pub fn is_retired(content: &str) -> bool {
    let mut lines = content.lines();
    if lines.next().map(str::trim) != Some("---") {
        return false;
    }
    for line in lines {
        let t = line.trim();
        if t == "---" || t == "..." {
            break;
        }
        if let Some(v) = t.strip_prefix("status:") {
            let v = v.trim().trim_matches('"').trim_matches('\'').to_lowercase();
            if v == "archived" || v == "superseded" {
                return true;
            }
        }
    }
    false
}

/// `Some(key, value)` pairs of a `---`-delimited front-matter block, and the body after it.
/// A file whose first line is not `---` has no front matter; an unterminated block swallows
/// the rest of the file (a half-written note is better indexed as metadata than as prose).
fn split_front_matter(content: &str) -> (Vec<(String, String)>, String) {
    let mut lines = content.lines();
    if lines.next().map(str::trim) != Some("---") {
        return (Vec::new(), content.to_string());
    }
    let mut front = Vec::new();
    let mut body_start = None;
    let mut used = 1usize;
    for line in lines {
        used += 1;
        if line.trim() == "---" || line.trim() == "..." {
            body_start = Some(used);
            break;
        }
        if line.starts_with(|c: char| c.is_alphanumeric()) {
            if let Some((k, v)) = line.split_once(':') {
                if !k.trim().is_empty() {
                    front.push((
                        k.trim().to_string(),
                        v.trim().trim_matches('"').trim_matches('\'').to_string(),
                    ));
                }
            }
        }
    }
    match body_start {
        Some(start) => {
            let body = content.lines().skip(start).collect::<Vec<_>>().join("\n");
            (front, body)
        }
        None => (front, String::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(y: i32, m: u32, d: u32) -> i64 {
        super::super::days_from_civil(y, m, d) * 86_400_000
    }

    #[test]
    fn one_h1_note_is_a_single_chunk() {
        let doc = chunk_markdown(
            "notes/idea.md",
            "# Idea\n\nA short note about the relay.",
            ms(2026, 1, 2),
        );
        assert_eq!(doc.title, "Idea");
        assert_eq!(doc.key, "notes/idea.md");
        assert_eq!(doc.locator, "notes/idea.md");
        assert_eq!(doc.occurred_from, ms(2026, 1, 2));
        assert_eq!(doc.occurred_to, ms(2026, 1, 2));
        assert_eq!(doc.chunks.len(), 1);
        assert_eq!(
            doc.chunks[0].heading, "",
            "that title heading is not repeated in the path"
        );
        assert_eq!(doc.chunks[0].text, "Idea\nA short note about the relay.");
        assert_eq!(doc.chunks[0].at, ms(2026, 1, 2));
        assert_eq!(doc.chunks[0].ord, 0);
    }

    #[test]
    fn date_and_title_sources() {
        // front matter wins over mtime and quotes are trimmed
        let d = chunk_markdown(
            "y/n.md",
            "---\ntitle: \"Named\"\ndate: 2024-02-01\n---\n\nbody text",
            ms(2026, 1, 2),
        );
        assert_eq!(d.title, "Named");
        assert_eq!(d.occurred_from, ms(2024, 2, 1));
        assert_eq!(d.chunks[0].text, "Named\nbody text");

        // no title, no front matter: the file stem names it and a date in the name times it
        let d = chunk_markdown("x/2025-04-07-week.md", "no heading at all", ms(2026, 1, 2));
        assert_eq!(d.title, "2025-04-07-week");
        assert_eq!(d.occurred_from, ms(2025, 4, 7));

        // nothing to date it by: mtime
        let d = chunk_markdown("x/plain.md", "just text", ms(2026, 3, 3));
        assert_eq!(d.occurred_from, ms(2026, 3, 3));
    }

    #[test]
    fn heading_paths_and_merging() {
        let d = chunk_markdown(
            "z/n.md",
            "# Top\n\nfirst.\n## Sub\n\nsecond.\n### Deep\n\nthird.",
            ms(2026, 1, 2),
        );
        assert_eq!(d.chunks.len(), 1, "short sections merge");
        assert_eq!(
            d.chunks[0].heading, "",
            "a merged chunk keeps the first section's path"
        );
        assert!(d.chunks[0].text.contains("second."));
        assert!(d.chunks[0].text.contains("third."));

        // the first heading names the doc even with a front-matter title; the path drops that first heading
        let d = chunk_markdown(
            "z/n.md",
            "---\ntitle: Named\n---\n\n# Chapter\n\n## Part\n\nbody.",
            ms(2026, 1, 2),
        );
        assert_eq!(d.title, "Chapter", "the first heading names the doc");
        assert_eq!(d.chunks[0].heading, "Part");
        assert!(d.chunks[0].text.starts_with("Chapter › Part\n"));

        // with no first heading the front-matter title rules and a level-two heading shapes the path
        let long = "i".repeat(TARGET_CHUNK_CHARS);
        let d = chunk_markdown(
            "z/n.md",
            &format!("---\ntitle: Named\n---\n\n{long}\n## Part\n\nbody."),
            ms(2026, 1, 2),
        );
        assert_eq!(d.title, "Named");
        assert_eq!(d.chunks.len(), 2, "the long intro is not merged away");
        assert_eq!(d.chunks[1].heading, "Part");
        assert!(d.chunks[1].text.starts_with("Named › Part\n"));

        // a level-four heading is body, not a section edge
        let d = chunk_markdown("z/n.md", "# T\n\none\n#### four\n\ntwo", ms(2026, 1, 2));
        assert_eq!(d.chunks.len(), 1);
        assert!(d.chunks[0].text.contains("#### four"));
    }

    #[test]
    fn long_sections_split_under_the_cap() {
        let body = "word ".repeat(500); // 2500 chars, one paragraph
        let d = chunk_markdown("z/big.md", &format!("# Big\n\n{body}"), ms(2026, 1, 2));
        assert!(d.chunks.len() >= 2);
        for c in &d.chunks {
            assert!(
                c.text.chars().count() <= MAX_CHUNK_CHARS,
                "over cap: {}",
                c.text.chars().count()
            );
            assert!(
                c.text.starts_with("Big\n"),
                "every piece carries the header"
            );
        }

        // paragraph-split pieces repeat the previous piece's last paragraph up front
        let para = "sentence worth reading. ".repeat(40); // 1080 chars
        let d = chunk_markdown(
            "z/p.md",
            &format!("# P\n\n{para}\n\n{para}\n\n{para}"),
            ms(2026, 1, 2),
        );
        assert!(d.chunks.len() >= 2, "three 1080-char paragraphs must split");
        for c in &d.chunks {
            assert!(c.text.chars().count() <= MAX_CHUNK_CHARS);
        }

        // two 1300-char sections do not merge into an over-cap chunk
        let a = "a".repeat(1300);
        let b = "b".repeat(1300);
        let d = chunk_markdown(
            "z/m.md",
            &format!("# A\n\n{a}\n\n## B\n\n{b}"),
            ms(2026, 1, 2),
        );
        assert_eq!(d.chunks.len(), 2);
        for c in &d.chunks {
            assert!(c.text.chars().count() <= MAX_CHUNK_CHARS);
        }
    }

    #[test]
    fn empty_and_metadata_only_files_keep_a_title_chunk() {
        let d = chunk_markdown("z/e.md", "", ms(2026, 1, 2));
        assert_eq!(d.chunks.len(), 1);
        assert_eq!(d.chunks[0].text, "e");
    }

    #[test]
    fn hash_moves_with_everything_a_searcher_notices() {
        let a = chunk_markdown("s/s.md", "# t\n\nx", ms(2026, 1, 2));
        let same = chunk_markdown("s/s.md", "# t\n\nx", ms(2026, 1, 2));
        assert_eq!(a.hash, same.hash);
        assert_ne!(
            a.hash,
            chunk_markdown("s/s.md", "# t\n\ny", ms(2026, 1, 2)).hash,
            "words"
        );
        // with no date inside, the file's own mtime times the doc, and it counts
        assert_ne!(
            a.hash,
            chunk_markdown("s/s.md", "# t\n\nx", ms(2026, 9, 9)).hash,
            "time"
        );
        // a title change moves it even with identical body
        assert_ne!(
            a.hash,
            chunk_markdown("s/s.md", "# u\n\nx", ms(2026, 1, 2)).hash,
            "title"
        );
    }
}
