//! The Markdown documents, parsed.
//!
//! Three facts are derived here and none of them is written down: which
//! documents exist, what structure each one has, and what a named section of
//! one contains. The inventory is a directory walk, not a list, so a document a
//! sibling branch adds is served without editing this file.
//!
//! Headings and code fences come from [`pulldown_cmark`] rather than from a line
//! scanner, because the distinction that matters most here is exactly the one a
//! line scanner gets wrong: a `# comment` line inside a shell block is not a
//! heading. Measured on this repository, a naive scan reports 46 top-level
//! headings in `contracts/contracts.md`, which has one.

use std::collections::BTreeSet;

use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use serde::Serialize;

use crate::repo::{Repo, RepoError};

/// Directories the inventory walk never enters.
///
/// `contracts/lib` holds the OpenZeppelin and forge-std submodules, whose own
/// documentation is not rub3's; `target` and `contracts/out` hold build output;
/// `.git` holds no documents at all. Everything else under the root is fair
/// game, including a document added after this file was written.
const SKIPPED_DIRECTORIES: [&str; 6] = [
    ".git",
    "target",
    "contracts/lib",
    "contracts/out",
    "contracts/cache",
    "contracts/broadcast",
];

/// One document in the inventory.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct Document {
    /// Repository-relative path, and the identifier every tool takes.
    pub path: String,
    /// The document's `#` heading.
    pub title: String,
    /// The prose between the title and the first section: what this document
    /// owns. Every document in this repository states that in its first
    /// paragraph, which is why it can be served rather than summarised.
    pub purpose: String,
    /// Total lines, so a caller can tell a stub from a specification.
    pub lines: usize,
    /// Every heading, in document order.
    pub headings: Vec<Heading>,
}

/// One heading.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct Heading {
    /// 1 for `#`, 6 for `######`.
    pub level: u8,
    /// The heading text, with inline markup left as written.
    pub text: String,
    /// The GitHub-style anchor, so a caller can build a link that resolves.
    pub anchor: String,
    /// 1-based line of the heading itself.
    pub line: usize,
}

/// A fenced code block, and the language tag it carries.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct CodeFence {
    /// 1-based line of the opening fence.
    pub line: usize,
    /// The fence's info string, empty when it carries no language.
    pub language: String,
}

/// A section of a document: a heading and everything under it.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct Section {
    /// The heading that opens the section.
    pub heading: Heading,
    /// 1-based inclusive line range.
    pub start_line: usize,
    /// 1-based inclusive line range.
    pub end_line: usize,
    /// The section text, verbatim.
    pub text: String,
}

/// One search hit.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct Match {
    /// Repository-relative path of the document the hit is in.
    pub path: String,
    /// 1-based line of the hit.
    pub line: usize,
    /// The innermost heading at or above the hit, if any.
    pub section: Option<String>,
    /// The matching line, trimmed of surrounding whitespace.
    pub text: String,
}

/// Discovers every Markdown document under the repository root.
///
/// Symlinks are skipped. `CLAUDE.md` in this repository is a symlink to
/// `AGENTS.md`, and an inventory listing both would answer two paths with one
/// document's content and invite a caller to diff them for a difference that
/// cannot exist.
pub fn inventory(repo: &Repo) -> Result<Vec<Document>, RepoError> {
    let mut paths = Vec::new();
    collect(repo.root(), "", &mut paths)?;
    paths.sort();
    paths
        .into_iter()
        .map(|path| {
            let text = repo.read(&path)?;
            Ok(parse(&path, &text))
        })
        .collect()
}

fn collect(
    directory: &std::path::Path,
    prefix: &str,
    found: &mut Vec<String>,
) -> Result<(), RepoError> {
    let entries = std::fs::read_dir(directory).map_err(|source| RepoError::Unreadable {
        path: prefix.to_string(),
        source,
    })?;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let relative = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        // `file_type` does not follow symlinks, which is how a symlinked
        // document is skipped rather than served twice.
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(_) => continue,
        };
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            if SKIPPED_DIRECTORIES.contains(&relative.as_str()) {
                continue;
            }
            collect(&entry.path(), &relative, found)?;
        } else if file_type.is_file() && relative.ends_with(".md") {
            found.push(relative);
        }
    }
    Ok(())
}

/// Parses one document's structure.
pub fn parse(path: &str, text: &str) -> Document {
    let headings = headings(text);
    let title = headings
        .iter()
        .find(|heading| heading.level == 1)
        .map(|heading| heading.text.clone())
        .unwrap_or_default();
    Document {
        path: path.to_string(),
        title,
        purpose: purpose(text, &headings),
        lines: text.lines().count(),
        headings,
    }
}

/// Every heading in a document, code fences excluded.
pub fn headings(text: &str) -> Vec<Heading> {
    let lines = LineIndex::new(text);
    let mut headings = Vec::new();
    let mut anchors: BTreeSet<String> = BTreeSet::new();
    let mut open: Option<(u8, usize, String)> = None;
    for (event, range) in Parser::new_ext(text, Options::ENABLE_STRIKETHROUGH).into_offset_iter() {
        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                open = Some((
                    heading_level(level),
                    lines.line_of(range.start),
                    String::new(),
                ));
            }
            Event::Text(fragment) | Event::Code(fragment) => {
                if let Some((_, _, collected)) = open.as_mut() {
                    collected.push_str(&fragment);
                }
            }
            Event::End(TagEnd::Heading(_)) => {
                if let Some((level, line, collected)) = open.take() {
                    let anchor = unique_anchor(&anchor_of(&collected), &mut anchors);
                    headings.push(Heading {
                        level,
                        text: collected,
                        anchor,
                        line,
                    });
                }
            }
            _ => {}
        }
    }
    headings
}

/// Every fenced code block, with the language it declares.
///
/// This is what makes "no code fence without a language" checkable rather than
/// aspirational: an untagged fence arrives here as an empty `language`.
pub fn code_fences(text: &str) -> Vec<CodeFence> {
    let lines = LineIndex::new(text);
    let mut fences = Vec::new();
    for (event, range) in Parser::new_ext(text, Options::ENABLE_STRIKETHROUGH).into_offset_iter() {
        if let Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(info))) = event {
            fences.push(CodeFence {
                line: lines.line_of(range.start),
                // The info string may carry more than a language
                // (```rust,ignore); the first word is the language.
                language: info.split(',').next().unwrap_or("").trim().to_string(),
            });
        }
    }
    fences
}

/// Extracts a section by heading text or by anchor.
///
/// A section runs from its heading to the next heading of the same or a higher
/// level, so asking for `## Testing` returns its `###` subsections too. That is
/// the unit a reader means by "that section", and slicing to the next heading of
/// any level would return a paragraph and hide the rest.
pub fn section(text: &str, selector: &str) -> Option<Section> {
    let headings = headings(text);
    let wanted = headings.iter().position(|heading| {
        heading.text.eq_ignore_ascii_case(selector) || heading.anchor == anchor_of(selector)
    })?;
    let heading = headings[wanted].clone();
    let end_line = headings[wanted + 1..]
        .iter()
        .find(|later| later.level <= heading.level)
        .map(|later| later.line - 1)
        .unwrap_or_else(|| text.lines().count());
    let body: Vec<&str> = text
        .lines()
        .skip(heading.line - 1)
        .take(end_line.saturating_sub(heading.line - 1))
        .collect();
    Some(Section {
        start_line: heading.line,
        end_line,
        text: body.join("\n"),
        heading,
    })
}

/// What a search found, and whether it stopped short of the corpus.
///
/// The flag is a fact the search establishes rather than one a caller infers
/// from the count: a search that found exactly `limit` hits and a search that
/// found more return the same number, and an agent told "truncated" re-queries
/// at a larger limit for evidence that is not there, or concludes the corpus
/// holds more than it does.
#[derive(Debug, Clone)]
pub struct Hits {
    /// The hits, capped at the limit.
    pub matches: Vec<Match>,
    /// True when at least one further hit exists past the limit.
    pub truncated: bool,
}

/// Case-insensitive substring search across an inventory.
///
/// Substring rather than regex on purpose: the caller is an agent looking for
/// where a term is specified, the corpus is a few thousand lines, and a regex
/// surface would add an error mode ("your pattern did not compile") to a tool
/// whose whole job is to answer.
pub fn search(repo: &Repo, documents: &[Document], needle: &str, limit: usize) -> Hits {
    let needle = needle.to_lowercase();
    let mut matches = Vec::new();
    let mut truncated = false;
    'documents: for document in documents {
        let Ok(text) = repo.read(&document.path) else {
            continue;
        };
        for (index, line) in text.lines().enumerate() {
            if !line.to_lowercase().contains(&needle) {
                continue;
            }
            if matches.len() == limit {
                // The one hit past the limit is looked for and never returned.
                // It is the only thing that distinguishes a full page from the
                // whole corpus.
                truncated = true;
                break 'documents;
            }
            let line_number = index + 1;
            matches.push(Match {
                path: document.path.clone(),
                line: line_number,
                section: document
                    .headings
                    .iter()
                    .rev()
                    .find(|heading| heading.line <= line_number)
                    .map(|heading| heading.text.clone()),
                text: line.trim().to_string(),
            });
        }
    }
    Hits { matches, truncated }
}

/// The prose between the title and the first section heading.
fn purpose(text: &str, headings: &[Heading]) -> String {
    let Some(title) = headings.iter().find(|heading| heading.level == 1) else {
        return String::new();
    };
    let next = headings
        .iter()
        .find(|heading| heading.line > title.line)
        .map(|heading| heading.line - 1)
        .unwrap_or_else(|| text.lines().count());
    text.lines()
        .skip(title.line)
        .take(next.saturating_sub(title.line))
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

fn heading_level(level: HeadingLevel) -> u8 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

/// The GitHub anchor for a heading: lowercased, punctuation dropped, spaces to
/// hyphens.
pub fn anchor_of(heading: &str) -> String {
    let mut anchor = String::with_capacity(heading.len());
    for character in heading.chars() {
        if character.is_alphanumeric() || character == '-' || character == '_' {
            anchor.extend(character.to_lowercase());
        } else if character == ' ' {
            anchor.push('-');
        }
    }
    anchor
}

fn unique_anchor(base: &str, seen: &mut BTreeSet<String>) -> String {
    if seen.insert(base.to_string()) {
        return base.to_string();
    }
    for suffix in 1.. {
        let candidate = format!("{base}-{suffix}");
        if seen.insert(candidate.clone()) {
            return candidate;
        }
    }
    unreachable!("the suffix search is unbounded")
}

/// Byte offset to 1-based line number.
pub(crate) struct LineIndex {
    starts: Vec<usize>,
}

impl LineIndex {
    pub(crate) fn new(text: &str) -> LineIndex {
        let mut starts = vec![0];
        starts.extend(
            text.char_indices()
                .filter(|(_, character)| *character == '\n')
                .map(|(index, _)| index + 1),
        );
        LineIndex { starts }
    }

    pub(crate) fn line_of(&self, offset: usize) -> usize {
        match self.starts.binary_search(&offset) {
            Ok(index) => index + 1,
            Err(index) => index,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
# Title

Purpose prose.

## First

```bash
# not a heading
echo hi
```

### Nested

body

## Second

tail
";

    #[test]
    fn a_hash_inside_a_code_fence_is_not_a_heading() {
        let parsed = parse("sample.md", SAMPLE);
        let texts: Vec<&str> = parsed
            .headings
            .iter()
            .map(|heading| heading.text.as_str())
            .collect();
        assert_eq!(texts, ["Title", "First", "Nested", "Second"]);
    }

    #[test]
    fn the_purpose_is_the_prose_under_the_title() {
        assert_eq!(parse("sample.md", SAMPLE).purpose, "Purpose prose.");
    }

    #[test]
    fn a_section_carries_its_subsections_and_stops_at_its_peer() {
        let section = section(SAMPLE, "First").expect("First is a heading");
        assert!(section.text.contains("### Nested"), "{}", section.text);
        assert!(!section.text.contains("## Second"), "{}", section.text);
    }

    #[test]
    fn a_section_resolves_by_anchor_too() {
        let by_anchor = section(SAMPLE, "nested").expect("nested is an anchor");
        assert_eq!(by_anchor.heading.text, "Nested");
    }

    #[test]
    fn an_untagged_fence_reports_an_empty_language() {
        let fences = code_fences("```\nplain\n```\n\n```bash\necho\n```\n");
        assert_eq!(fences.len(), 2);
        assert_eq!(fences[0].language, "");
        assert_eq!(fences[0].line, 1);
        assert_eq!(fences[1].language, "bash");
    }

    #[test]
    fn a_full_page_of_hits_is_only_truncated_when_another_one_exists() {
        // A scratch checkout rather than this repository, because the assertion
        // is about an exact hit count and the real corpus moves under it.
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("Cargo.toml"), "").expect("marker");
        std::fs::create_dir(dir.path().join("contracts")).expect("contracts");
        std::fs::write(dir.path().join("contracts/foundry.toml"), "").expect("marker");
        std::fs::write(
            dir.path().join("notes.md"),
            "# Notes\n\nneedle one\nneedle two\nneedle three\n",
        )
        .expect("document");
        let repo = Repo::resolve(
            Some(dir.path().to_path_buf()),
            std::path::PathBuf::from("/"),
        )
        .expect("the scratch directory carries both markers");
        let documents = inventory(&repo).expect("the inventory reads");

        let whole = search(&repo, &documents, "needle", 3);
        assert_eq!(whole.matches.len(), 3);
        assert!(
            !whole.truncated,
            "three hits under a limit of three is the whole corpus, not a page of it"
        );

        let capped = search(&repo, &documents, "needle", 2);
        assert_eq!(capped.matches.len(), 2);
        assert!(capped.truncated, "a third hit sits past the limit");
    }

    #[test]
    fn heading_lines_are_one_based() {
        let parsed = parse("sample.md", SAMPLE);
        assert_eq!(parsed.headings[0].line, 1);
        assert_eq!(parsed.headings[1].line, 5);
    }
}
