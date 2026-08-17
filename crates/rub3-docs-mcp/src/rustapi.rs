//! The workspace's public Rust API, derived from its own source.
//!
//! Every signature served from here is a **byte range of the source file**, not
//! a re-rendering of one. `syn` locates the item; the text handed back is
//! `source[span]`. That is the strongest form the "derived, never transcribed"
//! rule can take: a served signature that did not appear verbatim in the file
//! would be a bug in the slicing, and a test asserts exactly that property over
//! the whole workspace.
//!
//! The crates come from the workspace's `members` list rather than from a name
//! written here, so a crate a sibling branch adds is served without an edit.

use std::fmt;

use serde::Serialize;

use crate::docs::LineIndex;
use crate::repo::{Repo, RepoError};

/// Why a Rust API fact could not be derived.
#[derive(Debug)]
pub enum RustApiError {
    /// The workspace manifest is missing or does not list members.
    NoWorkspace { detail: String },
    /// A source file did not parse as Rust.
    Unparseable { path: String, detail: String },
    /// The repository could not be read.
    Repo(RepoError),
}

impl fmt::Display for RustApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RustApiError::NoWorkspace { detail } => {
                write!(
                    f,
                    "cannot read the workspace members from Cargo.toml: {detail}"
                )
            }
            RustApiError::Unparseable { path, detail } => {
                write!(f, "{path} did not parse as Rust: {detail}")
            }
            RustApiError::Repo(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for RustApiError {}

impl From<RepoError> for RustApiError {
    fn from(e: RepoError) -> Self {
        RustApiError::Repo(e)
    }
}

/// One workspace crate's public API.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct CrateApi {
    /// The crate's package name.
    pub name: String,
    /// Repository-relative path to the crate directory.
    pub path: String,
    /// The crate root and every public module of it.
    pub modules: Vec<Module>,
}

/// One module: the crate root, or a `pub mod` of it.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct Module {
    /// Module name, or `lib` for the crate root.
    pub name: String,
    /// Repository-relative path of the file it is declared in.
    pub path: String,
    /// The `#[cfg(...)]` attributes the crate root puts on this module,
    /// verbatim, ANDed together the way Rust reads them.
    ///
    /// This is the fact that catches people out most: under `tier-0` the
    /// `session`, `identity` and `session_store` modules do not exist at all, so
    /// an API listing with no feature gate beside it is misleading. All of them
    /// are served rather than the first, because a subset understates what a
    /// build needs and reads as though the rest were satisfied.
    pub cfg: Vec<String>,
    /// The module's own `//!` documentation.
    pub doc: String,
    /// Public items, in source order.
    pub items: Vec<Item>,
}

/// One public item.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct Item {
    /// `fn`, `const`, `static`, `struct`, `enum`, `trait`, `type`, `impl` or
    /// `reexport`.
    pub kind: String,
    /// The item's name, or the impl's target.
    pub name: String,
    /// The item's declaration, sliced verbatim out of the source file.
    pub signature: String,
    /// The `#[cfg(...)]` attributes on the item itself, verbatim.
    pub cfg: Vec<String>,
    /// The item's `///` documentation.
    pub doc: String,
    /// Repository-relative file the declaration was sliced from.
    ///
    /// Usually the module's own file, and not always: a `reexport` resolved
    /// back to its declaration reports the file that declares it, which is the
    /// file an integrator has to read.
    pub path: String,
    /// 1-based line the declaration starts on.
    pub line: usize,
    /// Public fields, variants, or methods, and for a `reexport` the
    /// declarations it points at that no other module in this listing carries.
    pub members: Vec<Item>,
}

/// Derives the public API of every workspace member that has a library.
pub fn workspace(repo: &Repo) -> Result<Vec<CrateApi>, RustApiError> {
    let mut derived = Vec::new();
    for member in members(repo)? {
        let root = format!("{member}/src/lib.rs");
        if !repo.is_file(&root) {
            // A binary-only member has no library surface to describe.
            continue;
        }
        let manifest = repo.read(&format!("{member}/Cargo.toml"))?;
        let name = toml::from_str::<toml::Value>(&manifest)
            .ok()
            .and_then(|value| {
                value
                    .get("package")?
                    .get("name")?
                    .as_str()
                    .map(str::to_string)
            })
            .unwrap_or_else(|| member.clone());
        derived.push(CrateApi {
            name,
            path: member.clone(),
            modules: modules(repo, &member, &root)?,
        });
    }
    Ok(derived)
}

/// The workspace `members` list.
fn members(repo: &Repo) -> Result<Vec<String>, RustApiError> {
    let manifest = repo.read("Cargo.toml")?;
    let value: toml::Value = toml::from_str(&manifest).map_err(|e| RustApiError::NoWorkspace {
        detail: format!("Cargo.toml is not valid TOML: {e}"),
    })?;
    let members = value
        .get("workspace")
        .and_then(|workspace| workspace.get("members"))
        .and_then(|members| members.as_array())
        .ok_or_else(|| RustApiError::NoWorkspace {
            detail: "no [workspace] members array".to_string(),
        })?;
    Ok(members
        .iter()
        .filter_map(|member| member.as_str())
        // A glob member would need the walk this crate deliberately does not
        // do; the workspace lists its members by path, and a glob appearing
        // later is better ignored than half-resolved.
        .filter(|member| !member.contains('*'))
        .map(str::to_string)
        .collect())
}

/// One source file of a crate, and what slicing it needs.
///
/// It is carried rather than passed as loose arguments because a declaration is
/// not always in the file the caller started from: a `pub use` is resolved back
/// to the file that declares it, and the answer has to say which file that was.
struct SourceFile {
    /// The crate directory, so a module path can be resolved from the root.
    crate_dir: String,
    /// Repository-relative path of this file.
    path: String,
    /// The file, verbatim. Every signature served is a slice of this.
    text: String,
    lines: LineIndex,
}

impl SourceFile {
    fn read(
        repo: &Repo,
        crate_dir: &str,
        path: &str,
    ) -> Result<(SourceFile, syn::File), RustApiError> {
        let text = repo.read(path)?;
        let parsed = parse(path, &text)?;
        let lines = LineIndex::new(&text);
        Ok((
            SourceFile {
                crate_dir: crate_dir.to_string(),
                path: path.to_string(),
                text,
                lines,
            },
            parsed,
        ))
    }
}

/// The crate root and every `pub mod` it declares.
fn modules(repo: &Repo, crate_dir: &str, root_path: &str) -> Result<Vec<Module>, RustApiError> {
    let (root, file) = SourceFile::read(repo, crate_dir, root_path)?;

    let mut modules = vec![Module {
        name: "lib".to_string(),
        path: root_path.to_string(),
        cfg: Vec::new(),
        doc: inner_doc(&file.attrs),
        items: items(repo, &root, &file.items),
    }];

    for item in &file.items {
        let syn::Item::Mod(module) = item else {
            continue;
        };
        if !is_public(&module.vis) || is_test_only(&cfgs(&root.text, &module.attrs)) {
            continue;
        }
        let name = module.ident.to_string();
        let candidates = [
            format!("{crate_dir}/src/{name}.rs"),
            format!("{crate_dir}/src/{name}/mod.rs"),
        ];
        let Some(path) = candidates.into_iter().find(|path| repo.is_file(path)) else {
            // An inline `pub mod name { .. }` is not described: `describe` has
            // no `Item::Mod` arm, so neither the module nor its items reach the
            // listing. No crate in this workspace declares one, and a recursion
            // no test could exercise would be the worse of the two answers.
            continue;
        };
        let (module_source, module_file) = SourceFile::read(repo, crate_dir, &path)?;
        modules.push(Module {
            name,
            path,
            cfg: cfgs(&root.text, &module.attrs),
            doc: inner_doc(&module_file.attrs),
            items: items(repo, &module_source, &module_file.items),
        });
    }
    drop_reexports_of_listed_items(&mut modules);
    Ok(modules)
}

/// Keeps a resolved re-export only when the listing carries it nowhere else.
///
/// `pub use activation::ensure` points into a module this listing already
/// describes in full, so repeating the declaration under the re-export would
/// double every public item of every public module. `pub use
/// self::headless::ensure_headless` points into a private module, which nothing
/// else in the listing reaches, and that is the declaration an integrator
/// otherwise has to invent.
///
/// The test is where the declaration is, file and line, rather than what its
/// module's visibility is: it is the same question the caller is asking, and it
/// stays right as the module walk changes.
fn drop_reexports_of_listed_items(modules: &mut [Module]) {
    let listed: std::collections::BTreeSet<(String, usize)> = modules
        .iter()
        .flat_map(|module| module.items.iter())
        .filter(|item| item.kind != "reexport")
        .map(|item| (item.path.clone(), item.line))
        .collect();
    for module in modules.iter_mut() {
        for item in module.items.iter_mut() {
            if item.kind == "reexport" {
                item.members
                    .retain(|member| !listed.contains(&(member.path.clone(), member.line)));
            }
        }
    }
}

fn parse(path: &str, source: &str) -> Result<syn::File, RustApiError> {
    syn::parse_file(source).map_err(|e| RustApiError::Unparseable {
        path: path.to_string(),
        detail: e.to_string(),
    })
}

/// Every public item of one parsed file.
fn items(repo: &Repo, ctx: &SourceFile, parsed: &[syn::Item]) -> Vec<Item> {
    let mut derived = Vec::new();
    for item in parsed {
        let Some(described) = describe(repo, ctx, item) else {
            continue;
        };
        if is_test_only(&described.cfg) {
            continue;
        }
        derived.push(described);
    }
    derived
}

fn describe(repo: &Repo, ctx: &SourceFile, item: &syn::Item) -> Option<Item> {
    use syn::spanned::Spanned;

    let source = ctx.text.as_str();
    let lines = &ctx.lines;

    match item {
        syn::Item::Fn(function) if is_public(&function.vis) => Some(Item {
            kind: "fn".to_string(),
            path: ctx.path.clone(),
            name: function.sig.ident.to_string(),
            signature: slice(
                source,
                start_of(&function.vis, function.sig.span()),
                end(function.sig.span()),
            ),
            cfg: cfgs(source, &function.attrs),
            doc: outer_doc(&function.attrs),
            line: lines.line_of(start_of(&function.vis, function.sig.span())),
            members: Vec::new(),
        }),
        syn::Item::Const(constant) if is_public(&constant.vis) => Some(Item {
            kind: "const".to_string(),
            path: ctx.path.clone(),
            name: constant.ident.to_string(),
            signature: value_signature(
                source,
                start_of(&constant.vis, constant.span()),
                end(constant.span()),
                end(constant.ty.span()),
            ),
            cfg: cfgs(source, &constant.attrs),
            doc: outer_doc(&constant.attrs),
            line: lines.line_of(start_of(&constant.vis, constant.span())),
            members: Vec::new(),
        }),
        syn::Item::Static(item_static) if is_public(&item_static.vis) => Some(Item {
            kind: "static".to_string(),
            path: ctx.path.clone(),
            name: item_static.ident.to_string(),
            signature: value_signature(
                source,
                start_of(&item_static.vis, item_static.span()),
                end(item_static.span()),
                end(item_static.ty.span()),
            ),
            cfg: cfgs(source, &item_static.attrs),
            doc: outer_doc(&item_static.attrs),
            line: lines.line_of(start_of(&item_static.vis, item_static.span())),
            members: Vec::new(),
        }),
        syn::Item::Struct(item_struct) if is_public(&item_struct.vis) => {
            let body = match &item_struct.fields {
                syn::Fields::Named(named) => Some(named.brace_token.span.open()),
                syn::Fields::Unnamed(unnamed) => Some(unnamed.paren_token.span.open()),
                syn::Fields::Unit => None,
            };
            let start = start_of(&item_struct.vis, item_struct.span());
            let stop = body
                .map(|body| body.byte_range().start)
                .unwrap_or_else(|| end(item_struct.span()));
            Some(Item {
                kind: "struct".to_string(),
                path: ctx.path.clone(),
                name: item_struct.ident.to_string(),
                signature: slice(source, start, stop),
                cfg: cfgs(source, &item_struct.attrs),
                doc: outer_doc(&item_struct.attrs),
                line: lines.line_of(start),
                members: item_struct
                    .fields
                    .iter()
                    .filter(|field| is_public(&field.vis))
                    .map(|field| Item {
                        kind: "field".to_string(),
                        path: ctx.path.clone(),
                        name: field
                            .ident
                            .as_ref()
                            .map(ToString::to_string)
                            .unwrap_or_default(),
                        signature: slice(
                            source,
                            start_of(&field.vis, field.span()),
                            end(field.span()),
                        ),
                        cfg: cfgs(source, &field.attrs),
                        doc: outer_doc(&field.attrs),
                        line: lines.line_of(start_of(&field.vis, field.span())),
                        members: Vec::new(),
                    })
                    .collect(),
            })
        }
        syn::Item::Enum(item_enum) if is_public(&item_enum.vis) => {
            let start = start_of(&item_enum.vis, item_enum.span());
            Some(Item {
                kind: "enum".to_string(),
                path: ctx.path.clone(),
                name: item_enum.ident.to_string(),
                signature: slice(
                    source,
                    start,
                    item_enum.brace_token.span.open().byte_range().start,
                ),
                cfg: cfgs(source, &item_enum.attrs),
                doc: outer_doc(&item_enum.attrs),
                line: lines.line_of(start),
                members: item_enum
                    .variants
                    .iter()
                    .map(|variant| {
                        // From the variant's name, not from its span: a span
                        // covers the item's attributes, so a documented variant
                        // would otherwise be served with its own doc comment
                        // inside the signature.
                        let start = variant.ident.span().byte_range().start;
                        Item {
                            kind: "variant".to_string(),
                            path: ctx.path.clone(),
                            name: variant.ident.to_string(),
                            signature: slice(source, start, end(variant.span())),
                            cfg: cfgs(source, &variant.attrs),
                            doc: outer_doc(&variant.attrs),
                            line: lines.line_of(start),
                            members: Vec::new(),
                        }
                    })
                    .collect(),
            })
        }
        syn::Item::Trait(item_trait) if is_public(&item_trait.vis) => {
            let start = start_of(&item_trait.vis, item_trait.span());
            Some(Item {
                kind: "trait".to_string(),
                path: ctx.path.clone(),
                name: item_trait.ident.to_string(),
                signature: slice(
                    source,
                    start,
                    item_trait.brace_token.span.open().byte_range().start,
                ),
                cfg: cfgs(source, &item_trait.attrs),
                doc: outer_doc(&item_trait.attrs),
                line: lines.line_of(start),
                members: item_trait
                    .items
                    .iter()
                    .filter_map(|member| match member {
                        syn::TraitItem::Fn(function) => Some(Item {
                            kind: "fn".to_string(),
                            path: ctx.path.clone(),
                            name: function.sig.ident.to_string(),
                            signature: slice(
                                source,
                                function.sig.span().byte_range().start,
                                end(function.sig.span()),
                            ),
                            cfg: cfgs(source, &function.attrs),
                            doc: outer_doc(&function.attrs),
                            line: lines.line_of(function.sig.span().byte_range().start),
                            members: Vec::new(),
                        }),
                        _ => None,
                    })
                    .collect(),
            })
        }
        syn::Item::Type(item_type) if is_public(&item_type.vis) => {
            let start = start_of(&item_type.vis, item_type.span());
            Some(Item {
                kind: "type".to_string(),
                path: ctx.path.clone(),
                name: item_type.ident.to_string(),
                signature: slice(source, start, end(item_type.span())),
                cfg: cfgs(source, &item_type.attrs),
                doc: outer_doc(&item_type.attrs),
                line: lines.line_of(start),
                members: Vec::new(),
            })
        }
        syn::Item::Use(item_use) if is_public(&item_use.vis) => {
            let start = start_of(&item_use.vis, item_use.span());
            Some(Item {
                kind: "reexport".to_string(),
                path: ctx.path.clone(),
                name: use_name(&item_use.tree),
                signature: slice(source, start, end(item_use.span())),
                cfg: cfgs(source, &item_use.attrs),
                doc: outer_doc(&item_use.attrs),
                line: lines.line_of(start),
                members: reexported(repo, ctx, &item_use.tree),
            })
        }
        syn::Item::Impl(item_impl) => {
            // An impl block is reported for the public methods it adds. A block
            // that adds none - a trait impl, or an all-private helper block -
            // would be a header with nothing under it.
            let methods: Vec<Item> = item_impl
                .items
                .iter()
                .filter_map(|member| match member {
                    syn::ImplItem::Fn(function) if is_public(&function.vis) => Some(Item {
                        kind: "fn".to_string(),
                        path: ctx.path.clone(),
                        name: function.sig.ident.to_string(),
                        signature: slice(
                            source,
                            start_of(&function.vis, function.sig.span()),
                            end(function.sig.span()),
                        ),
                        cfg: cfgs(source, &function.attrs),
                        doc: outer_doc(&function.attrs),
                        line: lines.line_of(start_of(&function.vis, function.sig.span())),
                        members: Vec::new(),
                    }),
                    _ => None,
                })
                .filter(|method| !is_test_only(&method.cfg))
                .collect();
            if methods.is_empty() {
                return None;
            }
            // From the `impl` keyword rather than the block's span, for the same
            // reason as an enum variant: the span would carry the block's
            // attributes into the header.
            let start = item_impl.impl_token.span().byte_range().start;
            Some(Item {
                kind: "impl".to_string(),
                path: ctx.path.clone(),
                name: slice(
                    source,
                    start,
                    item_impl.brace_token.span.open().byte_range().start,
                )
                .trim_start_matches("impl")
                .trim()
                .to_string(),
                signature: slice(
                    source,
                    start,
                    item_impl.brace_token.span.open().byte_range().start,
                ),
                cfg: cfgs(source, &item_impl.attrs),
                doc: outer_doc(&item_impl.attrs),
                line: lines.line_of(start),
                members: methods,
            })
        }
        _ => None,
    }
}

/// A `const` or `static` declaration, with its value when that value is the
/// point.
///
/// `pub const EXIT_COOLDOWN_ACTIVE: i32 = 13;` is useless without the 13, so a
/// one-line declaration is served whole. `EXIT_CODE_HELP` is a whole help table
/// as a string literal, and serving it as a signature would bury the surface it
/// belongs to; a declaration spanning more than one line is therefore cut after
/// its type. Both halves of that rule are verbatim slices.
fn value_signature(source: &str, start: usize, item_end: usize, type_end: usize) -> String {
    let whole = slice(source, start, item_end);
    if !whole.contains('\n') {
        return whole;
    }
    format!("{} = ...;", slice(source, start, type_end))
}

/// What a `pub use` re-exports, read off the use tree.
///
/// The signature already carries the statement verbatim; this is the name a
/// caller filters on, so it is the leaves rather than the path prefix.
fn use_name(tree: &syn::UseTree) -> String {
    match tree {
        syn::UseTree::Path(path) => use_name(&path.tree),
        syn::UseTree::Name(name) => name.ident.to_string(),
        syn::UseTree::Rename(rename) => rename.rename.to_string(),
        syn::UseTree::Glob(_) => "*".to_string(),
        syn::UseTree::Group(group) => group
            .items
            .iter()
            .map(use_name)
            .collect::<Vec<_>>()
            .join(", "),
    }
}

/// The declarations a `pub use` points at, sliced from the files that declare
/// them.
///
/// The statement itself is already served verbatim, and a statement is not a
/// signature: `pub use self::headless::{ensure_headless, ..}` tells an
/// integrator that the name exists and nothing about how to call it. The
/// wrapper's two front doors are exactly this shape, so without the resolution
/// the parameter lists an agent most needs are the ones no answer carries.
///
/// The resolution stays inside the same crate and inside the same rule as the
/// rest of this module: the target is located with `syn` and the text handed
/// back is `source[span]` of the file that declares it. A leaf that does not
/// resolve - an external crate, a glob, a `super::` path, a chain of further
/// re-exports - contributes nothing, because a re-export whose target cannot be
/// found is a fact this crate does not have.
fn reexported(repo: &Repo, ctx: &SourceFile, tree: &syn::UseTree) -> Vec<Item> {
    let mut leaves = Vec::new();
    use_leaves(tree, &mut Vec::new(), &mut leaves);
    leaves
        .into_iter()
        .filter_map(|(segments, public_name)| {
            let (target, item) = resolve(repo, ctx, &segments, 0)?;
            let mut described = describe(repo, &target, &item)?;
            // The name a caller can write, which a rename makes different from
            // the declared one. The signature stays the declaration's own.
            described.name = public_name;
            Some(described)
        })
        .filter(|item| !is_test_only(&item.cfg))
        .collect()
}

/// Every name a use tree brings into scope, as (path segments, public name).
fn use_leaves(
    tree: &syn::UseTree,
    prefix: &mut Vec<String>,
    found: &mut Vec<(Vec<String>, String)>,
) {
    match tree {
        syn::UseTree::Path(path) => {
            prefix.push(path.ident.to_string());
            use_leaves(&path.tree, prefix, found);
            prefix.pop();
        }
        syn::UseTree::Name(name) => {
            let mut segments = prefix.clone();
            segments.push(name.ident.to_string());
            found.push((segments, name.ident.to_string()));
        }
        syn::UseTree::Rename(rename) => {
            let mut segments = prefix.clone();
            segments.push(rename.ident.to_string());
            found.push((segments, rename.rename.to_string()));
        }
        // A glob re-exports whatever its target holds at the time, so the names
        // are a property of that module rather than of this statement. Nothing
        // is resolved from one; the statement is still served verbatim.
        syn::UseTree::Glob(_) => {}
        syn::UseTree::Group(group) => {
            for item in &group.items {
                use_leaves(item, prefix, found);
            }
        }
    }
}

/// Walks a use path to the item it names, within this crate.
///
/// `use` paths resolve from the crate root, so a bare first segment is either a
/// top-level module of this crate or an external crate; the walk finds the
/// module or gives up. `super::` is not followed: a module file's parent is a
/// fact about the module tree this walk does not build, and guessing it would
/// be the one thing this crate refuses to do.
fn resolve(
    repo: &Repo,
    ctx: &SourceFile,
    segments: &[String],
    depth: usize,
) -> Option<(SourceFile, syn::Item)> {
    if depth > MAX_REEXPORT_DEPTH {
        return None;
    }
    let (first, rest) = segments.split_first()?;
    let (start_path, rest) = match first.as_str() {
        "self" => (ctx.path.clone(), rest),
        "crate" => (format!("{}/src/lib.rs", ctx.crate_dir), rest),
        "super" => return None,
        _ => (format!("{}/src/lib.rs", ctx.crate_dir), segments),
    };
    if rest.is_empty() {
        return None;
    }
    let (start, parsed) = SourceFile::read(repo, &ctx.crate_dir, &start_path).ok()?;
    resolve_in(repo, start, &parsed.items, rest, depth)
}

/// How many `pub use` hops the walk follows before giving up.
///
/// The wrapper's headless front door is two: `lib.rs` re-exports what
/// `activation.rs` re-exports out of its private `headless` module. The bound
/// is what makes a re-export that points back at itself terminate, since a
/// crate that does not compile is still a crate this server can be pointed at.
const MAX_REEXPORT_DEPTH: usize = 8;

fn resolve_in(
    repo: &Repo,
    ctx: SourceFile,
    parsed: &[syn::Item],
    segments: &[String],
    depth: usize,
) -> Option<(SourceFile, syn::Item)> {
    let (name, rest) = segments.split_first()?;
    if rest.is_empty() {
        if let Some(found) = parsed
            .iter()
            .find(|item| declared_name(item).as_deref() == Some(name.as_str()))
        {
            return Some((ctx, found.clone()));
        }
        // Not declared here, so this module may itself be re-exporting it from
        // somewhere further in. Following the hop is what lets the name an
        // integrator actually writes - `rub3_wrapper::ensure_headless`, two
        // re-exports away from its declaration - answer with a signature.
        return follow(repo, &ctx, parsed, name, depth);
    }
    let module = parsed.iter().find_map(|item| match item {
        syn::Item::Mod(module) if module.ident == name.as_str() => Some(module),
        _ => None,
    })?;
    match &module.content {
        Some((_, inner)) => resolve_in(repo, ctx, inner, rest, depth),
        None => {
            let path = module_files(&ctx.path, name)
                .into_iter()
                .find(|candidate| repo.is_file(candidate))?;
            let (child, child_file) = SourceFile::read(repo, &ctx.crate_dir, &path).ok()?;
            resolve_in(repo, child, &child_file.items, rest, depth)
        }
    }
}

/// The next hop for a name this module re-exports rather than declares.
fn follow(
    repo: &Repo,
    ctx: &SourceFile,
    parsed: &[syn::Item],
    name: &str,
    depth: usize,
) -> Option<(SourceFile, syn::Item)> {
    for item in parsed {
        let syn::Item::Use(item_use) = item else {
            continue;
        };
        if !is_public(&item_use.vis) {
            continue;
        }
        let mut leaves = Vec::new();
        use_leaves(&item_use.tree, &mut Vec::new(), &mut leaves);
        let hop = leaves
            .into_iter()
            .find(|(_, public_name)| public_name == name)
            .map(|(segments, _)| segments);
        if let Some(segments) = hop {
            return resolve(repo, ctx, &segments, depth + 1);
        }
    }
    None
}

/// Where `mod name;` declared in one file puts that module's own file.
fn module_files(parent: &str, name: &str) -> [String; 2] {
    let (directory, file) = parent.rsplit_once('/').unwrap_or(("", parent));
    let base = match file {
        "lib.rs" | "main.rs" | "mod.rs" => directory.to_string(),
        other => format!("{directory}/{}", other.trim_end_matches(".rs")),
    };
    [format!("{base}/{name}.rs"), format!("{base}/{name}/mod.rs")]
}

/// The name an item declares, for the kinds a `use` path can end at.
fn declared_name(item: &syn::Item) -> Option<String> {
    match item {
        syn::Item::Fn(function) => Some(function.sig.ident.to_string()),
        syn::Item::Const(constant) => Some(constant.ident.to_string()),
        syn::Item::Static(item_static) => Some(item_static.ident.to_string()),
        syn::Item::Struct(item_struct) => Some(item_struct.ident.to_string()),
        syn::Item::Enum(item_enum) => Some(item_enum.ident.to_string()),
        syn::Item::Trait(item_trait) => Some(item_trait.ident.to_string()),
        syn::Item::Type(item_type) => Some(item_type.ident.to_string()),
        _ => None,
    }
}

fn slice(source: &str, start: usize, end: usize) -> String {
    source
        .get(start..end)
        .unwrap_or_default()
        .trim_end()
        .to_string()
}

/// Where a declaration starts: at `pub`, when there is one.
///
/// `syn` puts visibility outside the signature, so slicing from the signature
/// alone would serve `fn ensure(..)` for a `pub fn ensure(..)` and quietly
/// misstate the API.
fn start_of(vis: &syn::Visibility, fallback: proc_macro2::Span) -> usize {
    use syn::spanned::Spanned;
    match vis {
        syn::Visibility::Inherited => fallback.byte_range().start,
        other => other.span().byte_range().start,
    }
}

fn end(span: proc_macro2::Span) -> usize {
    span.byte_range().end
}

fn is_public(vis: &syn::Visibility) -> bool {
    matches!(vis, syn::Visibility::Public(_))
}

/// The `#[cfg(...)]` attributes on an item, sliced verbatim.
fn cfgs(source: &str, attrs: &[syn::Attribute]) -> Vec<String> {
    use syn::spanned::Spanned;
    attrs
        .iter()
        .filter(|attr| attr.path().is_ident("cfg"))
        .map(|attr| {
            let span = attr.span().byte_range();
            slice(source, span.start, span.end)
        })
        .collect()
}

/// True when an item exists only under `cfg(test)`.
///
/// Test scaffolding is not API: serving it would invite an integrator to call
/// something that does not exist in their build. Today's scaffolding never
/// reaches this predicate, because visibility excludes it first and the walk
/// never enters a private module, so what this guards is the `#[cfg(test)] pub`
/// item a later change adds.
///
/// The predicate is parsed rather than searched for a substring, because the
/// substring reading is wrong in both directions and both directions are silent:
/// `cfg(not(test))` gates an item to exactly the builds an integrator ships and
/// would have been dropped from the surface, while `cfg(any(test, feature =
/// "x"))` is scaffolding that would have been served. A missing item is the
/// failure this crate exists to exclude, so a cfg whose tokens do not parse is
/// served rather than dropped.
fn is_test_only(cfgs: &[String]) -> bool {
    cfgs.iter().any(|cfg| gates_on_test(cfg))
}

/// True when one verbatim `#[cfg(...)]` requires `test`.
fn gates_on_test(cfg: &str) -> bool {
    use syn::parse::Parser;

    let Ok(attrs) = syn::Attribute::parse_outer.parse_str(cfg) else {
        return false;
    };
    attrs
        .iter()
        .filter(|attr| attr.path().is_ident("cfg"))
        .filter_map(|attr| attr.parse_args::<syn::Meta>().ok())
        .any(|meta| predicate_names_test(&meta))
}

/// True when `test` appears as a bare ident the predicate turns on.
///
/// `all(..)` and `any(..)` are walked into: under `all` the item is compiled
/// only in a test build, and under `any` it is scaffolding that happens to have
/// a second way in. `not(..)` deliberately is not, since everything inside it
/// means the opposite of what the walk is looking for.
fn predicate_names_test(meta: &syn::Meta) -> bool {
    match meta {
        syn::Meta::Path(path) => path.is_ident("test"),
        syn::Meta::List(list) if list.path.is_ident("all") || list.path.is_ident("any") => {
            let parser = syn::punctuated::Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated;
            list.parse_args_with(parser)
                .map(|nested| nested.iter().any(predicate_names_test))
                .unwrap_or(false)
        }
        _ => false,
    }
}

fn outer_doc(attrs: &[syn::Attribute]) -> String {
    doc_lines(attrs, syn::AttrStyle::Outer)
}

fn inner_doc(attrs: &[syn::Attribute]) -> String {
    doc_lines(attrs, syn::AttrStyle::Inner(syn::token::Not::default()))
}

fn doc_lines(attrs: &[syn::Attribute], style: syn::AttrStyle) -> String {
    let wanted_inner = matches!(style, syn::AttrStyle::Inner(_));
    attrs
        .iter()
        .filter(|attr| matches!(attr.style, syn::AttrStyle::Inner(_)) == wanted_inner)
        .filter(|attr| attr.path().is_ident("doc"))
        .filter_map(|attr| match &attr.meta {
            syn::Meta::NameValue(name_value) => match &name_value.value {
                syn::Expr::Lit(literal) => match &literal.lit {
                    syn::Lit::Str(text) => Some(text.value().trim().to_string()),
                    _ => None,
                },
                _ => None,
            },
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_predicate_that_requires_test_is_test_only() {
        for cfg in [
            "#[cfg(test)]",
            "#[cfg(any(test, feature = \"headless\"))]",
            "#[cfg(all(test, unix))]",
        ] {
            assert!(
                is_test_only(&[cfg.to_string()]),
                "{cfg} gates an item to a test build"
            );
        }
    }

    #[test]
    fn a_predicate_that_does_not_require_test_is_served() {
        // `not(test)` is the one the substring reading got backwards: it names
        // `test` while describing exactly the builds an integrator ships.
        for cfg in [
            "#[cfg(not(test))]",
            "#[cfg(feature = \"headless\")]",
            "#[cfg(all(unix, feature = \"webview\"))]",
            "#[cfg(any(feature = \"session\", feature = \"cooldown\"))]",
        ] {
            assert!(
                !is_test_only(&[cfg.to_string()]),
                "{cfg} belongs in the served surface"
            );
        }
    }
}
