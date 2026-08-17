//! Agent-legibility as a checked property of the repository's documents.
//!
//! The documents were already written to be read by a machine. What was missing
//! is the check: nothing failed when a code fence lost its language, when a
//! cross-reference pointed at a heading that had been renamed, or when a new
//! document arrived with no statement of what it owns. Each assertion below is
//! one of those, and each one is stated as the property rather than as a list of
//! known-good files, so a document a sibling branch adds is held to it too.
//!
//! This suite lives in the docs MCP server's crate because the server is the
//! consumer: `list_documents` reports the heading outline an agent navigates by,
//! and `read_document` slices a section out of it. A document that fails here is
//! one the server would describe badly.

use std::collections::BTreeSet;

use rub3_docs_mcp::{docs, Repo};

/// Where `llms.txt` points for the clean Markdown of a repository file.
///
/// Raw URLs rather than the `blob` viewer, because the llms.txt specification
/// asks for a clean Markdown representation and that is what raw serves.
/// Absolute rather than relative, because `llms.txt` is routinely read on its
/// own, detached from the tree it describes.
const RAW_PREFIX: &str = "https://raw.githubusercontent.com/jaq-h/rub3/main/";

fn repo() -> Repo {
    Repo::compiled_in().expect("this crate lives in a rub3 checkout")
}

/// The documents this gate holds to the standard: the git-tracked ones.
///
/// The server walks the working tree on purpose, because a document written a
/// moment ago has to be served. A gate that did the same would hold an
/// untracked scratch file to the repository's documentation standard and then
/// tell its author to link their own working notes from `llms.txt`, which is a
/// red suite that means nothing. Committing the file is what puts it in scope.
///
/// When git cannot answer (a source tarball, a sandbox with no git binary) the
/// whole inventory is held instead: that is the stricter of the two, so the
/// gate never quietly checks less than it should.
fn inventory(repo: &Repo) -> Vec<docs::Document> {
    let all = docs::inventory(repo).expect("the documents read");
    let Some(tracked) = tracked_documents(repo) else {
        return all;
    };
    all.into_iter()
        .filter(|document| tracked.contains(&document.path))
        .collect()
}

fn tracked_documents(repo: &Repo) -> Option<BTreeSet<String>> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(repo.root())
        .args(["ls-files", "-z", "--", "*.md"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let listing = String::from_utf8(output.stdout).ok()?;
    let tracked: BTreeSet<String> = listing
        .split('\0')
        .filter(|path| !path.is_empty())
        .map(str::to_string)
        .collect();
    // An empty listing is git answering "nothing is tracked here", which in a
    // real checkout means the question was asked in the wrong place.
    (!tracked.is_empty()).then_some(tracked)
}

// ── The documents ─────────────────────────────────────────────────────────────

/// A fence with no language is a block a renderer cannot highlight and a
/// consumer cannot classify.
///
/// `text` is the right tag for the ASCII diagrams and directory trees this
/// repository uses; the point of the check is that the choice is made, not that
/// it is a programming language.
#[test]
fn every_code_fence_declares_a_language() {
    let repo = repo();
    let mut untagged = Vec::new();
    for document in inventory(&repo) {
        let text = repo.read(&document.path).expect("a document reads");
        for fence in docs::code_fences(&text) {
            if fence.language.is_empty() {
                untagged.push(format!("{}:{}", document.path, fence.line));
            }
        }
    }
    assert!(
        untagged.is_empty(),
        "these code fences declare no language. Tag prose, diagrams and directory trees `text`, \
         and shell blocks `bash`:\n  {}",
        untagged.join("\n  ")
    );
}

/// A cross-reference that does not resolve is worse than no cross-reference: it
/// sends a reader, or an agent, somewhere that does not exist.
#[test]
fn every_relative_link_resolves_to_a_file_and_a_heading() {
    let repo = repo();
    let documents = inventory(&repo);
    let mut broken = Vec::new();

    for document in &documents {
        let text = repo.read(&document.path).expect("a document reads");
        let directory = document
            .path
            .rsplit_once('/')
            .map(|(directory, _)| directory.to_string())
            .unwrap_or_default();

        for (line, target) in links(&text) {
            if target.starts_with("http://")
                || target.starts_with("https://")
                || target.starts_with("mailto:")
            {
                continue;
            }
            let (path, anchor) = match target.split_once('#') {
                Some((path, anchor)) => (path, Some(anchor)),
                None => (target.as_str(), None),
            };
            let resolved = if path.is_empty() {
                document.path.clone()
            } else {
                normalise(&directory, path)
            };
            if !repo.is_file(&resolved) {
                broken.push(format!(
                    "{}:{line}: {target} -> no file at {resolved}",
                    document.path
                ));
                continue;
            }
            let Some(anchor) = anchor else { continue };
            let target_text = repo.read(&resolved).expect("a linked document reads");
            let anchors: BTreeSet<String> = docs::headings(&target_text)
                .into_iter()
                .map(|heading| heading.anchor)
                .collect();
            if !anchors.contains(&anchor.to_lowercase()) {
                broken.push(format!(
                    "{}:{line}: {target} -> {resolved} has no such heading",
                    document.path
                ));
            }
        }
    }
    assert!(
        broken.is_empty(),
        "these cross-references do not resolve:\n  {}",
        broken.join("\n  ")
    );
}

/// Headings are the index an agent navigates by, so they have to describe the
/// structure rather than the styling.
///
/// One `#` per document, and no level skipped: a `###` directly under a `#`
/// claims a `##` that does not exist, which silently breaks any outline built
/// from the levels - including the one `list_documents` serves.
#[test]
fn every_document_has_one_title_and_skips_no_heading_level() {
    let mut problems = Vec::new();
    for document in inventory(&repo()) {
        let titles: Vec<&docs::Heading> = document
            .headings
            .iter()
            .filter(|heading| heading.level == 1)
            .collect();
        if titles.len() != 1 {
            problems.push(format!(
                "{}: {} top-level headings, expected exactly one",
                document.path,
                titles.len()
            ));
        }
        let mut previous = 0;
        for heading in &document.headings {
            if previous != 0 && heading.level > previous + 1 {
                problems.push(format!(
                    "{}:{}: h{} directly under h{} ({})",
                    document.path, heading.line, heading.level, previous, heading.text
                ));
            }
            previous = heading.level;
        }
    }
    assert!(
        problems.is_empty(),
        "these documents have a heading structure that does not describe them:\n  {}",
        problems.join("\n  ")
    );
}

/// Every document says what it owns, in its own first paragraph.
///
/// This repository's rule is that each document has a single subject and states
/// it up front, which is what makes a section read out of one document
/// trustworthy on its own. A new document without that line is the failure mode
/// this catches: an agent cannot tell whether it is authoritative.
#[test]
fn every_document_states_its_purpose_under_its_title() {
    let mut silent = Vec::new();
    for document in inventory(&repo()) {
        if document.purpose.trim().len() < 80 {
            silent.push(format!("{}: {:?}", document.path, document.purpose.trim()));
        }
    }
    assert!(
        silent.is_empty(),
        "these documents do not state what they own between the title and the first section:\n  {}",
        silent.join("\n  ")
    );
}

/// The repository writes plain dashes. This is the regression guard for that.
///
/// `llms.txt` is swept alongside the Markdown inventory rather than left to it:
/// the walk collects `*.md` only, and the one prose file an integrating agent
/// reads first is the last one that should sit outside the rule.
#[test]
fn no_document_uses_an_em_dash() {
    let repo = repo();
    let mut offenders = Vec::new();
    let paths = inventory(&repo)
        .into_iter()
        .map(|document| document.path)
        .chain(std::iter::once("llms.txt".to_string()));
    for path in paths {
        let text = repo.read(&path).expect("a document reads");
        for (number, line) in text.lines().enumerate() {
            if line.contains('\u{2014}') {
                offenders.push(format!("{}:{}", path, number + 1));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "em dashes at:\n  {}\nUse a plain dash.",
        offenders.join("\n  ")
    );
}

// ── llms.txt ──────────────────────────────────────────────────────────────────

/// `llms.txt` follows the format the specification actually defines.
///
/// Checked against llmstxt.org v2 (last modified 2026-08-10): an optional BOM,
/// then the H1 that is the only required section, then an optional blockquote
/// summary, then optional prose containing no headings, then any number of
/// H2-delimited lists whose items are Markdown links with optional `: notes`.
/// v2 dropped the special meaning of the `Optional` section, which is why this
/// asserts nothing about which sections exist beyond their shape.
#[test]
fn llms_txt_follows_the_specification() {
    let repo = repo();
    let text = repo.read("llms.txt").expect("llms.txt exists at the root");
    let lines: Vec<&str> = text.lines().collect();

    assert!(
        !text.starts_with('\u{feff}'),
        "a byte-order mark is permitted, but this file should not need one"
    );

    let first = lines
        .iter()
        .find(|line| !line.trim().is_empty())
        .expect("llms.txt is not empty");
    assert!(
        first.starts_with("# ") && !first.starts_with("##"),
        "the first content line must be the H1 title, got {first:?}"
    );

    let after_title = lines
        .iter()
        .skip_while(|line| !line.starts_with("# "))
        .skip(1)
        .find(|line| !line.trim().is_empty())
        .expect("something follows the title");
    assert!(
        after_title.starts_with("> "),
        "the summary blockquote comes directly after the title, got {after_title:?}"
    );

    // Everything between the summary and the first H2 is the details section,
    // which the specification allows as long as it holds no headings.
    let first_section = lines
        .iter()
        .position(|line| line.starts_with("## "))
        .expect("llms.txt carries at least one H2 section");
    let title = lines
        .iter()
        .position(|line| line.starts_with("# "))
        .expect("the title was found above");
    for (offset, line) in lines[..first_section].iter().enumerate() {
        assert!(
            offset == title || !line.starts_with('#'),
            "llms.txt:{}: only the title may be a heading before the first section, got {line:?}",
            offset + 1
        );
    }

    // Every list item under an H2 is a link, because that is what makes the
    // file machine-readable rather than merely Markdown.
    let mut in_section = false;
    let mut items = 0;
    for (offset, line) in lines.iter().enumerate() {
        if line.starts_with("## ") {
            in_section = true;
            continue;
        }
        if !in_section || !line.starts_with("- ") {
            continue;
        }
        items += 1;
        assert!(
            line.starts_with("- ["),
            "llms.txt:{}: a list item under a section must be a link, got {line:?}",
            offset + 1
        );
        assert_eq!(
            links(line).len(),
            1,
            "llms.txt:{}: one link per item, got {line:?}",
            offset + 1
        );
    }
    assert!(items >= 6, "llms.txt lists only {items} links");
}

/// Every link in `llms.txt` points at a file that exists.
///
/// The links are absolute, so they cannot be resolved by fetching them here
/// without a network. They are checked structurally instead: each one is the raw
/// URL for a path in this repository, and that path exists. A renamed or deleted
/// file therefore turns this red in the same commit that moved it, which is the
/// whole reason to check rather than to trust.
#[test]
fn every_llms_txt_link_names_a_file_in_this_repository() {
    let repo = repo();
    let text = repo.read("llms.txt").expect("llms.txt exists");
    let mut wrong = Vec::new();
    for (line, target) in links(&text) {
        let Some(path) = target.strip_prefix(RAW_PREFIX) else {
            wrong.push(format!(
                "llms.txt:{line}: {target} is not a {RAW_PREFIX} URL"
            ));
            continue;
        };
        if !repo.is_file(path) {
            wrong.push(format!("llms.txt:{line}: {target} -> no file at {path}"));
        }
    }
    assert!(
        wrong.is_empty(),
        "these llms.txt links do not name a file in this repository:\n  {}",
        wrong.join("\n  ")
    );
}

/// `llms.txt` covers every document in the repository.
///
/// The point of the file is that an agent which reads it has been pointed at
/// everything authoritative. A document that exists and is not linked is
/// invisible to exactly the reader this file was written for, so a new document
/// turns this red until it is either listed or deliberately excluded here with a
/// reason.
#[test]
fn llms_txt_links_every_document_in_the_repository() {
    let repo = repo();
    let text = repo.read("llms.txt").expect("llms.txt exists");
    let linked: BTreeSet<String> = links(&text)
        .into_iter()
        .filter_map(|(_, target)| target.strip_prefix(RAW_PREFIX).map(str::to_string))
        .collect();

    let mut missing = Vec::new();
    for document in inventory(&repo) {
        if linked.contains(&document.path) {
            continue;
        }
        missing.push(document.path);
    }
    assert!(
        missing.is_empty(),
        "these documents exist and llms.txt does not point at them:\n  {}\n\nAdd each one to a \
         section of llms.txt with a line saying what it owns. CLAUDE.md is a symlink to AGENTS.md \
         and is not in the inventory, so it needs no entry of its own.",
        missing.join("\n  ")
    );
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Every `[text](target)` in a document, with its 1-based line.
///
/// Deliberately naive about nesting - a link label containing brackets is not a
/// shape this repository writes - and deliberately blind to fenced code, since a
/// link inside an example block is illustration rather than a reference.
fn links(text: &str) -> Vec<(usize, String)> {
    let mut found = Vec::new();
    let mut fenced = false;
    for (number, line) in text.lines().enumerate() {
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
            continue;
        }
        if fenced {
            continue;
        }
        let bytes = line.as_bytes();
        let mut index = 0;
        while let Some(open) = line[index..].find("](") {
            let start = index + open + 2;
            let Some(close) = line[start..].find(')') else {
                break;
            };
            let target = &line[start..start + close];
            // `](` inside prose without a preceding `[` is not a link.
            if line[..index + open].contains('[') && !bytes.is_empty() {
                found.push((number + 1, target.to_string()));
            }
            index = start + close;
        }
    }
    found
}

/// Resolves a relative link against the linking document's directory.
fn normalise(directory: &str, target: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    if !directory.is_empty() {
        parts.extend(directory.split('/'));
    }
    for component in target.split('/') {
        match component {
            "." | "" => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    parts.join("/")
}
