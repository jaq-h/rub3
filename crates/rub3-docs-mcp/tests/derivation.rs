//! Proof that nothing served is transcribed.
//!
//! Two shapes of test, because "derived" has two halves worth proving.
//!
//! The mutation tests build a throwaway checkout, ask the server a question,
//! change the file the answer came from, and ask again through the *same*
//! server. An answer that did not move is either a hardcoded string or a cache,
//! and both are the failure this crate exists to prevent.
//!
//! The whole-repository tests prove the same property against the real source
//! and the real artifacts: every Rust signature served is a byte-for-byte slice
//! of the file it claims to come from, and every rendered contract signature
//! agrees with the selector map solc itself wrote. Neither can pass on a copy.

use std::fs;
use std::path::PathBuf;

use rmcp::handler::server::wrapper::Parameters;
use rub3_docs_mcp::server::{ContractAbi, RustApi};
use rub3_docs_mcp::{docs, rustapi, solidity, DocsServer, Repo};

// ── Fixture ───────────────────────────────────────────────────────────────────

/// A throwaway checkout with one crate and one contract artifact.
///
/// It carries the two markers `Repo` looks for and nothing else it does not
/// need, so a test that mutates it is unambiguous about which file the served
/// answer came from.
struct Fixture {
    directory: tempfile::TempDir,
}

const DEMO_LIB: &str = r#"//! Demo crate.

pub mod widget;

mod gadget;

pub use gadget::spin as gadget_spin;
pub use widget::alpha;
"#;

const DEMO_WIDGET: &str = r#"//! Widgets.

pub use self::inner::polish;

/// Does the alpha thing.
pub fn alpha(count: u32) -> bool {
    count > 0
}

mod inner {
    /// Polishes a widget.
    pub fn polish(coats: u8) -> u8 {
        coats
    }
}
"#;

/// A private file module, re-exported under another name from the crate root.
///
/// The wrapper's `supervisor_run` is this shape, and the wrapper's headless
/// front door is the inline-module shape in `DEMO_WIDGET`. Neither declaration
/// is reachable through a `pub mod`, so a listing that stops at the `pub use`
/// statement never carries a parameter list for either.
const DEMO_GADGET: &str = r#"//! Gadgets.

/// Spins a gadget.
pub fn spin(turns: u8) -> u8 {
    turns
}
"#;

impl Fixture {
    fn new() -> Fixture {
        let directory = tempfile::tempdir().expect("tempdir");
        let fixture = Fixture { directory };
        fixture.write(
            "Cargo.toml",
            "[workspace]\nmembers = [\"crates/demo\"]\nresolver = \"2\"\n",
        );
        fixture.write(
            "contracts/foundry.toml",
            "[profile.default]\nsrc = \"src\"\nout = \"out\"\n",
        );
        fixture.write("contracts/canonical-bytecode.json", MANIFEST);
        fixture.write("contracts/out/Demo.sol/Demo.json", ARTIFACT);
        fixture.write(
            "crates/demo/Cargo.toml",
            "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        );
        fixture.write("crates/demo/src/lib.rs", DEMO_LIB);
        fixture.write("crates/demo/src/widget.rs", DEMO_WIDGET);
        fixture.write("crates/demo/src/gadget.rs", DEMO_GADGET);
        fixture.write("README.md", "# Demo\n\nThe demo fixture.\n");
        fixture
    }

    fn write(&self, relative: &str, contents: &str) {
        let path = self.directory.path().join(relative);
        fs::create_dir_all(path.parent().expect("a parent")).expect("mkdir");
        fs::write(path, contents).expect("write");
    }

    fn write_bytes(&self, relative: &str, contents: &[u8]) {
        let path = self.directory.path().join(relative);
        fs::create_dir_all(path.parent().expect("a parent")).expect("mkdir");
        fs::write(path, contents).expect("write");
    }

    fn remove(&self, relative: &str) {
        fs::remove_dir_all(self.directory.path().join(relative)).expect("rmdir");
    }

    fn remove_file(&self, relative: &str) {
        fs::remove_file(self.directory.path().join(relative)).expect("rm");
    }

    fn server(&self) -> DocsServer {
        let repo = Repo::resolve(
            Some(self.directory.path().to_path_buf()),
            PathBuf::from("/"),
        )
        .expect("the fixture carries both markers");
        DocsServer::new(repo)
    }
}

/// A minimal forge artifact: one function, one selector, one AST node.
const ARTIFACT: &str = r#"{
  "abi": [
    {"type": "function", "name": "purchase", "inputs": [], "outputs": [{"name": "", "type": "uint256", "internalType": "uint256"}], "stateMutability": "payable"}
  ],
  "methodIdentifiers": { "purchase()": "00112233" },
  "deployedBytecode": { "object": "0x6080", "immutableReferences": {} },
  "metadata": { "settings": { "compilationTarget": { "src/Demo.sol": "Demo" } } },
  "ast": { "nodes": [ {"nodeType": "ContractDefinition", "name": "Demo", "contractKind": "contract", "abstract": false} ] }
}
"#;

const MANIFEST: &str = r#"{
  "schema": 1,
  "contracts": {
    "Demo": {
      "source": "src/Demo.sol",
      "deployed_bytecode_sha256": "1111111111111111111111111111111111111111111111111111111111111111",
      "immutable_ranges": [{"start": 7, "length": 32}]
    }
  }
}
"#;

/// True when the reported line is where the served declaration begins.
///
/// A signature is cut before the body, so the file's line is the served line
/// plus whatever opens the block; the reported line is right when the file's
/// line starts with it. Leading indentation is dropped because a nested member
/// is sliced from its own first token.
fn starts_the_declaration(source: &str, line: usize, signature: &str) -> bool {
    let first = signature.lines().next().unwrap_or_default();
    source
        .lines()
        .nth(line - 1)
        .is_some_and(|found| found.trim_start().starts_with(first.trim_start()))
}

/// The declaration served under a re-export: signature, declaring file, line.
fn reexported(
    api: &[rustapi::CrateApi],
    module: &str,
    name: &str,
) -> Option<(String, String, usize)> {
    api.iter()
        .flat_map(|crate_api| crate_api.modules.iter())
        .filter(|candidate| candidate.name == module)
        .flat_map(|candidate| candidate.items.iter())
        .filter(|item| item.kind == "reexport")
        .flat_map(|item| item.members.iter())
        .find(|member| member.name == name)
        .map(|member| (member.signature.clone(), member.path.clone(), member.line))
}

fn only_signature(api: &[rustapi::CrateApi], module: &str, name: &str) -> Option<String> {
    api.iter()
        .flat_map(|crate_api| crate_api.modules.iter())
        .filter(|candidate| candidate.name == module)
        .flat_map(|candidate| candidate.items.iter())
        .find(|item| item.name == name)
        .map(|item| item.signature.clone())
}

/// How many `#[cfg]` attributes a crate root puts on one `mod` declaration.
///
/// Parsed rather than counted textually, because the question is about the
/// attributes attached to that declaration and a `#[cfg]` on the item beside it
/// looks identical to a line scan. `lib` names the crate root itself, which no
/// `mod` item declares and which therefore carries no gate.
fn declared_cfg_count(root: &str, module: &str) -> usize {
    if module == "lib" {
        return 0;
    }
    let parsed = syn::parse_file(root).expect("the crate root parses");
    parsed
        .items
        .iter()
        .filter_map(|item| match item {
            syn::Item::Mod(declaration) if declaration.ident == module => Some(
                declaration
                    .attrs
                    .iter()
                    .filter(|attr| attr.path().is_ident("cfg"))
                    .count(),
            ),
            _ => None,
        })
        .next()
        .unwrap_or_default()
}

// ── Mutation: the Rust surface follows the source ─────────────────────────────

#[test]
fn a_renamed_function_moves_the_served_signature() {
    let fixture = Fixture::new();
    let server = fixture.server();

    let before = server
        .rust_api(Parameters(RustApi {
            crate_name: None,
            module: None,
            name: None,
            limit: None,
        }))
        .expect("the fixture crate derives")
        .0
        .crates;
    assert_eq!(
        only_signature(&before, "widget", "alpha").as_deref(),
        Some("pub fn alpha(count: u32) -> bool"),
    );

    fixture.write(
        "crates/demo/src/widget.rs",
        "//! Widgets.\n\n/// Does the beta thing.\npub fn beta(count: u32, label: &str) -> bool {\n    count > 0 && !label.is_empty()\n}\n",
    );

    // The same server instance, deliberately: an answer that survives the edit
    // is a cache, which for a docs server is the same defect as a hardcoded
    // string.
    let after = server
        .rust_api(Parameters(RustApi {
            crate_name: None,
            module: None,
            name: None,
            limit: None,
        }))
        .expect("the edited crate derives")
        .0
        .crates;
    assert_eq!(
        only_signature(&after, "widget", "beta").as_deref(),
        Some("pub fn beta(count: u32, label: &str) -> bool"),
    );
    assert!(
        only_signature(&after, "widget", "alpha").is_none(),
        "alpha no longer exists in the source and must not be served"
    );
}

#[test]
fn a_feature_gate_added_to_the_crate_root_reaches_the_served_module() {
    let fixture = Fixture::new();
    let server = fixture.server();

    let gate = |api: &[rustapi::CrateApi]| {
        api.iter()
            .flat_map(|crate_api| crate_api.modules.iter())
            .find(|module| module.name == "widget")
            .map(|module| module.cfg.clone())
            .unwrap_or_default()
    };
    let ask = || {
        server
            .rust_api(Parameters(RustApi {
                crate_name: None,
                module: None,
                name: None,
                limit: None,
            }))
            .expect("derives")
            .0
            .crates
    };

    assert!(gate(&ask()).is_empty());

    fixture.write(
        "crates/demo/src/lib.rs",
        "//! Demo crate.\n\n#[cfg(feature = \"widgets\")]\npub mod widget;\n",
    );

    assert_eq!(
        gate(&ask()),
        vec!["#[cfg(feature = \"widgets\")]".to_string()],
        "the module's feature gate is read from the crate root at call time"
    );

    // Rust ANDs stacked gates, so serving the first alone would tell an agent
    // the module is reachable with `--features widgets` on any platform.
    fixture.write(
        "crates/demo/src/lib.rs",
        "//! Demo crate.\n\n#[cfg(feature = \"widgets\")]\n#[cfg(target_os = \"macos\")]\npub mod widget;\n",
    );

    assert_eq!(
        gate(&ask()),
        vec![
            "#[cfg(feature = \"widgets\")]".to_string(),
            "#[cfg(target_os = \"macos\")]".to_string(),
        ],
        "every gate on the declaration is served, not the first"
    );
}

// ── Mutation: the contract surface follows the artifact ───────────────────────

#[test]
fn an_edited_abi_entry_moves_the_served_signature_and_selector() {
    let fixture = Fixture::new();
    let server = fixture.server();

    let ask = || {
        server
            .contract_abi(Parameters(ContractAbi {
                contract: "Demo".to_string(),
                include_raw: None,
            }))
            .expect("Demo derives")
            .0
    };

    let before = ask();
    assert_eq!(before.functions.len(), 1);
    assert_eq!(before.functions[0].signature, "purchase()");
    assert_eq!(before.functions[0].selector.as_deref(), Some("0x00112233"));
    assert_eq!(
        before.functions[0].declaration,
        "function purchase() payable returns (uint256)"
    );

    fixture.write(
        "contracts/out/Demo.sol/Demo.json",
        &ARTIFACT
            .replace(
                r#""name": "purchase", "inputs": []"#,
                r#""name": "purchase", "inputs": [{"name": "quantity", "type": "uint256", "internalType": "uint256"}]"#,
            )
            .replace(r#""purchase()": "00112233""#, r#""purchase(uint256)": "44556677""#),
    );

    let after = ask();
    assert_eq!(after.functions[0].signature, "purchase(uint256)");
    assert_eq!(after.functions[0].selector.as_deref(), Some("0x44556677"));
    assert_eq!(
        after.functions[0].declaration,
        "function purchase(uint256 quantity) payable returns (uint256)"
    );
}

#[test]
fn a_selector_absent_from_the_artifact_is_served_as_absent() {
    // The selector is solc's, read out of `methodIdentifiers`. When the artifact
    // does not carry one this crate reports none rather than hashing the
    // signature itself, because a selector it computed would be a second
    // opinion nobody asked for.
    let fixture = Fixture::new();
    let server = fixture.server();
    fixture.write(
        "contracts/out/Demo.sol/Demo.json",
        &ARTIFACT.replace(r#""methodIdentifiers": { "purchase()": "00112233" },"#, ""),
    );
    let abi = server
        .contract_abi(Parameters(ContractAbi {
            contract: "Demo".to_string(),
            include_raw: None,
        }))
        .expect("Demo still derives")
        .0;
    assert_eq!(abi.functions[0].selector, None);
}

#[test]
fn the_served_fingerprint_is_the_published_one() {
    let fixture = Fixture::new();
    let server = fixture.server();

    let fingerprint = |server: &DocsServer| {
        server
            .list_contracts()
            .expect("Demo derives")
            .0
            .contracts
            .into_iter()
            .find(|contract| contract.name == "Demo")
            .and_then(|contract| contract.canonical)
    };

    let before = fingerprint(&server).expect("Demo is in the fixture manifest");
    assert!(before.deployed_bytecode_sha256.starts_with("1111"));
    assert_eq!(before.immutable_ranges, vec![(7, 32)]);

    fixture.write(
        "contracts/canonical-bytecode.json",
        &MANIFEST
            .replace('1', "2")
            .replace(r#""start": 7, "length": 32"#, r#""start": 9, "length": 32"#),
    );

    let after = fingerprint(&server).expect("still published");
    assert!(
        after.deployed_bytecode_sha256.starts_with("2222"),
        "the fingerprint is read from contracts/canonical-bytecode.json at call time, got {}",
        after.deployed_bytecode_sha256
    );
    assert_eq!(after.immutable_ranges, vec![(9, 32)]);
}

#[test]
fn a_contract_question_without_artifacts_names_the_build_command() {
    // The one case where refusing to answer is the correct answer:
    // contracts/out is not checked in, and a remembered ABI would be worse than
    // an error.
    let fixture = Fixture::new();
    let server = fixture.server();
    fixture.remove("contracts/out");

    let error = match server.list_contracts() {
        Err(error) => error,
        Ok(_) => panic!("no artifacts, so there are no contract facts to serve"),
    };
    let message = format!("{error:?}");
    assert!(
        message.contains("forge build"),
        "the refusal must name the command that fixes it, got: {message}"
    );
}

#[test]
fn an_unknown_module_is_refused_with_the_modules_that_exist() {
    // The other shape of "refusing is the answer": an empty result set for a
    // misspelled module reads as "that module exposes no public API", and an
    // agent believing it goes straight back to inventing the signature.
    let fixture = Fixture::new();
    let server = fixture.server();

    let error = match server.rust_api(Parameters(RustApi {
        crate_name: Some("demo".to_string()),
        module: Some("widgt".to_string()),
        name: None,
        limit: None,
    })) {
        Err(error) => error,
        Ok(served) => panic!(
            "a module that does not exist must not answer, got {} crates",
            served.0.crates.len()
        ),
    };
    let message = format!("{error:?}");
    assert!(
        message.contains("widgt") && message.contains("widget"),
        "the refusal must name the modules that do exist, got: {message}"
    );

    let served = server
        .rust_api(Parameters(RustApi {
            crate_name: Some("demo".to_string()),
            module: Some("widget".to_string()),
            name: None,
            limit: None,
        }))
        .expect("the module that does exist answers")
        .0
        .crates;
    assert_eq!(
        only_signature(&served, "widget", "alpha").as_deref(),
        Some("pub fn alpha(count: u32) -> bool"),
    );
}

/// A `pub use` out of a private module answers with the declaration, not with
/// the statement.
///
/// The statement names the item and says nothing about how to call it, so a
/// listing that stops there leaves an agent to invent the parameter list, which
/// is the one failure this crate exists to prevent. Both shapes the wrapper
/// uses are covered: a private file module (`supervisor_run`) and a private
/// module inline in the same file (the headless front door).
#[test]
fn a_reexport_out_of_a_private_module_serves_the_declaration_it_points_at() {
    let fixture = Fixture::new();
    let server = fixture.server();
    let ask = || {
        server
            .rust_api(Parameters(RustApi {
                crate_name: None,
                module: None,
                name: None,
                limit: None,
            }))
            .expect("the fixture crate derives")
            .0
            .crates
    };

    let (signature, path, line) =
        reexported(&ask(), "lib", "gadget_spin").expect("the crate root re-exports gadget::spin");
    assert_eq!(signature, "pub fn spin(turns: u8) -> u8");
    assert_eq!(
        path, "crates/demo/src/gadget.rs",
        "the answer names the file that declares it, not the one re-exporting it"
    );
    assert_eq!(line, 4);

    let (signature, path, _) =
        reexported(&ask(), "widget", "polish").expect("widget re-exports its inline inner module");
    assert_eq!(signature, "pub fn polish(coats: u8) -> u8");
    assert_eq!(path, "crates/demo/src/widget.rs");

    // The same server instance, so an answer that survives the edit is a cache
    // or a transcription rather than a slice taken at call time.
    fixture.write(
        "crates/demo/src/gadget.rs",
        "//! Gadgets.\n\n/// Spins a gadget.\npub fn spin(turns: u8, reverse: bool) -> u8 {\n    turns\n}\n",
    );
    assert_eq!(
        reexported(&ask(), "lib", "gadget_spin").map(|found| found.0),
        Some("pub fn spin(turns: u8, reverse: bool) -> u8".to_string()),
    );
}

/// A re-export of something this listing already carries is left as a statement.
///
/// Resolving one would repeat every public item of every public module under
/// the crate root's re-exports, which is noise an agent pays for in context.
#[test]
fn a_reexport_of_an_already_listed_declaration_is_not_repeated() {
    let fixture = Fixture::new();
    let server = fixture.server();
    let api = server
        .rust_api(Parameters(RustApi {
            crate_name: None,
            module: None,
            name: None,
            limit: None,
        }))
        .expect("the fixture crate derives")
        .0
        .crates;

    assert!(
        reexported(&api, "lib", "alpha").is_none(),
        "widget is a public module, so its declaration of alpha is already served"
    );
    assert_eq!(
        only_signature(&api, "widget", "alpha").as_deref(),
        Some("pub fn alpha(count: u32) -> bool"),
    );
}

/// A name that matches nothing refuses, the way a bad crate or module does.
#[test]
fn an_unmatched_name_is_refused_with_the_names_that_exist() {
    let fixture = Fixture::new();
    let server = fixture.server();

    let error = match server.rust_api(Parameters(RustApi {
        crate_name: Some("demo".to_string()),
        module: None,
        name: Some("alpah".to_string()),
        limit: None,
    })) {
        Err(error) => error,
        Ok(served) => panic!(
            "a name that matches nothing must not answer, got {} crates",
            served.0.crates.len()
        ),
    };
    let message = format!("{error:?}");
    assert!(
        message.contains("alpah") && message.contains("alpha"),
        "the refusal must name the items that do exist, got: {message}"
    );

    let served = server
        .rust_api(Parameters(RustApi {
            crate_name: Some("demo".to_string()),
            module: None,
            name: Some("alpha".to_string()),
            limit: None,
        }))
        .expect("the name that does exist answers")
        .0
        .crates;
    assert_eq!(
        only_signature(&served, "widget", "alpha").as_deref(),
        Some("pub fn alpha(count: u32) -> bool"),
    );
}

/// A missing fingerprint manifest is no fingerprints, not a broken tool.
///
/// Everything else `list_contracts` reports is derived from the artifacts, and
/// `contract_abi` needs the same set to name the contracts that do exist, so
/// failing both over a record neither derives anything from would take the
/// contract surface down for a checkout that simply has not run the gate.
#[test]
fn a_missing_fingerprint_manifest_leaves_the_contract_surface_answering() {
    let fixture = Fixture::new();
    let server = fixture.server();
    fixture.remove_file("contracts/canonical-bytecode.json");

    let contracts = server
        .list_contracts()
        .expect("the artifact-derived facts do not need the manifest")
        .0
        .contracts;
    let demo = contracts
        .iter()
        .find(|contract| contract.name == "Demo")
        .expect("Demo is still derived from its artifact");
    assert!(demo.deployable);
    assert!(
        demo.canonical.is_none(),
        "no manifest means no published fingerprint, not a wrong one"
    );

    let abi = server
        .contract_abi(Parameters(ContractAbi {
            contract: "Demo".to_string(),
            include_raw: None,
        }))
        .expect("the ABI is derived from the artifact")
        .0;
    assert_eq!(abi.functions[0].signature, "purchase()");

    // The unknown-contract refusal builds its "known" list from the same set.
    let error = match server.contract_abi(Parameters(ContractAbi {
        contract: "Demoo".to_string(),
        include_raw: None,
    })) {
        Err(error) => error,
        Ok(_) => panic!("a contract with no artifact must not answer"),
    };
    assert!(
        format!("{error:?}").contains("Demo"),
        "the refusal still names the contracts that do exist"
    );
}

/// An unfiltered call is capped, and says so.
///
/// The whole public surface of this workspace is on the order of a hundred
/// kilobytes of JSON with the doc comments in it, and `rust_api {}` is a
/// legitimate first call. A caller that cannot tell a first page from the whole
/// surface would read the page as the API, which is the same wrong belief the
/// refusals guard against.
#[test]
fn the_item_cap_cuts_the_listing_short_and_reports_it() {
    let fixture = Fixture::new();
    let server = fixture.server();
    let ask = |limit: Option<usize>| {
        server
            .rust_api(Parameters(RustApi {
                crate_name: None,
                module: None,
                name: None,
                limit,
            }))
            .expect("the fixture crate derives")
            .0
    };

    let whole = ask(None);
    assert!(
        !whole.truncated,
        "the fixture is smaller than the default cap"
    );
    let served = |api: &rub3_docs_mcp::server::WorkspaceApi| -> usize {
        api.crates
            .iter()
            .flat_map(|crate_api| crate_api.modules.iter())
            .map(|module| module.items.len())
            .sum()
    };
    assert!(served(&whole) > 1, "the fixture has items to cut");

    let capped = ask(Some(1));
    assert!(
        capped.truncated,
        "one item out of several is a truncated answer"
    );
    assert_eq!(served(&capped), 1);
    assert_eq!(
        capped.crates[0].modules[0].items[0].signature,
        whole.crates[0].modules[0].items[0].signature,
        "the cap takes a prefix of the derivation, not a different answer"
    );

    // A limit at or above the whole surface is not truncation.
    let exact = ask(Some(served(&whole)));
    assert!(!exact.truncated);
    assert_eq!(served(&exact), served(&whole));
}

/// A module that derives no items keeps its own documentation.
///
/// `//! What this crate is.` over nothing but `pub mod` declarations is an
/// ordinary crate root, and the walk describes no `mod` item, so that module
/// doc has nowhere else to surface. Dropping the module because it listed no
/// items would delete derived content, which is the opposite of what the cap
/// and the filters are cleaning up. No module in this workspace is that shape
/// today, so the test builds one.
#[test]
fn a_module_that_derived_no_items_keeps_its_doc() {
    let fixture = Fixture::new();
    let server = fixture.server();
    fixture.write(
        "crates/demo/src/lib.rs",
        "//! What the demo crate is.\n\npub mod widget;\n",
    );

    let crates = server
        .rust_api(Parameters(RustApi {
            crate_name: None,
            module: None,
            name: None,
            limit: None,
        }))
        .expect("widget still carries the crate's public items")
        .0
        .crates;
    let root = crates
        .iter()
        .flat_map(|crate_api| crate_api.modules.iter())
        .find(|module| module.name == "lib")
        .expect("the crate root is a real answer even with no items of its own");
    assert!(
        root.items.is_empty(),
        "the root declares only modules, so it derives no items"
    );
    assert_eq!(root.doc, "What the demo crate is.");
}

/// A module that exists and exposes nothing is answered, not refused.
///
/// One rule holds at every filter: a module doc is derived content this tool
/// serves, so a module that derived no public items is an answer rather than an
/// absence. Only a module that does not exist is refused, and that refusal
/// names the modules that do. Narrowing to a module the unfiltered listing just
/// showed would otherwise be an error, which reads as the module not existing
/// and is the belief every guard here was written to prevent.
#[test]
fn a_module_with_no_public_items_answers_with_its_doc() {
    let fixture = Fixture::new();
    let server = fixture.server();
    fixture.write(
        "crates/demo/src/lib.rs",
        "//! Demo crate.\n\npub mod widget;\npub mod plumbing;\n",
    );
    fixture.write(
        "crates/demo/src/plumbing.rs",
        "//! Internals only.\n\nfn helper() -> u8 {\n    1\n}\n",
    );
    let ask = |module: Option<&str>| {
        server.rust_api(Parameters(RustApi {
            crate_name: None,
            module: module.map(str::to_string),
            name: None,
            limit: None,
        }))
    };

    let unfiltered = ask(None).expect("the crate derives").0.crates;
    let listed = unfiltered
        .iter()
        .flat_map(|crate_api| crate_api.modules.iter())
        .find(|module| module.name == "plumbing")
        .expect("the unfiltered listing carries the module");
    assert_eq!(listed.doc, "Internals only.");

    // The same module, asked for directly, is the same answer.
    let narrowed = ask(Some("plumbing"))
        .expect("a module that exists answers")
        .0;
    assert_eq!(narrowed.crates.len(), 1);
    assert_eq!(narrowed.crates[0].modules.len(), 1);
    assert_eq!(narrowed.crates[0].modules[0].name, "plumbing");
    assert!(narrowed.crates[0].modules[0].items.is_empty());
    assert_eq!(narrowed.crates[0].modules[0].doc, "Internals only.");

    // A module that does not exist is still refused, and still says what does.
    let error = match ask(Some("plumbin")) {
        Err(error) => error,
        Ok(served) => panic!(
            "a module that does not exist must not answer, got {} crates",
            served.0.crates.len()
        ),
    };
    let message = format!("{error:?}");
    assert!(
        message.contains("plumbin") && message.contains("widget"),
        "the refusal must name the modules that do exist, got: {message}"
    );
}

/// A refusal never trails an empty list.
///
/// The list is the whole reason the refusal exists, so a scope with no public
/// names has to say that rather than print a colon and stop, which reads as the
/// server having lost the answer.
#[test]
fn a_refusal_with_nothing_to_list_says_so() {
    let fixture = Fixture::new();
    let server = fixture.server();
    fixture.write(
        "crates/demo/src/lib.rs",
        "//! Demo crate.\n\npub mod widget;\npub mod plumbing;\n",
    );
    fixture.write(
        "crates/demo/src/plumbing.rs",
        "//! Internals only.\n\nfn helper() -> u8 {\n    1\n}\n",
    );

    let error = match server.rust_api(Parameters(RustApi {
        crate_name: None,
        module: Some("plumbing".to_string()),
        name: Some("helper".to_string()),
        limit: None,
    })) {
        Err(error) => error,
        Ok(served) => panic!(
            "nothing public is named helper, got {} crates",
            served.0.crates.len()
        ),
    };
    let message = format!("{error:?}");
    assert!(
        message.contains("There are no public names in scope"),
        "a refusal with nothing to list must say so, got: {message}"
    );
    assert!(
        !message.contains("scope: "),
        "an empty list must not be printed at all, got: {message}"
    );
}

/// The cap drops a crate it emptied, and keeps one that had nothing to cut.
///
/// A crate whose every item the cap took is an artifact of truncation and
/// answers nothing, so a page carrying it spends context on an empty entry. A
/// crate that derived no items in the first place is a real answer, and its
/// `//!` survives the cap the way it survives the filters.
#[test]
fn the_cap_drops_a_crate_it_emptied_and_keeps_a_doc_only_one() {
    let fixture = Fixture::new();
    let server = fixture.server();
    fixture.write(
        "Cargo.toml",
        "[workspace]\nmembers = [\"crates/demo\", \"crates/second\", \"crates/notes\"]\nresolver = \"2\"\n",
    );
    fixture.write(
        "crates/second/Cargo.toml",
        "[package]\nname = \"second\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    );
    fixture.write(
        "crates/second/src/lib.rs",
        "//! Second crate.\n\n/// Does the only thing.\npub fn only(flag: bool) -> bool {\n    flag\n}\n",
    );
    fixture.write(
        "crates/notes/Cargo.toml",
        "[package]\nname = \"notes\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    );
    fixture.write("crates/notes/src/lib.rs", "//! Nothing public here.\n");
    let ask = |limit: Option<usize>| {
        server
            .rust_api(Parameters(RustApi {
                crate_name: None,
                module: None,
                name: None,
                limit,
            }))
            .expect("the workspace derives")
            .0
    };

    let whole = ask(None);
    assert!(!whole.truncated);
    for member in ["demo", "second", "notes"] {
        assert!(
            whole.crates.iter().any(|api| api.name == member),
            "{member} is a workspace member with a library"
        );
    }

    let capped = ask(Some(1));
    assert!(capped.truncated);
    let served: Vec<&str> = capped.crates.iter().map(|api| api.name.as_str()).collect();
    assert!(
        !served.contains(&"second"),
        "the cap took every item of second, so it answers nothing, got {served:?}"
    );
    let notes = capped
        .crates
        .iter()
        .find(|api| api.name == "notes")
        .expect("a crate the cap took nothing from is still an answer");
    assert_eq!(notes.modules[0].doc, "Nothing public here.");
}

// ── The real repository ───────────────────────────────────────────────────────

/// Two derivations in a row agree, and the second is still verbatim.
///
/// `workspace` drops the spans of every earlier derivation before it starts,
/// which is what stops `proc_macro2`'s thread-local source map growing for the
/// life of the server. Dropping them anywhere later would invalidate spans that
/// a derivation in progress still holds, and the damage is silent: the slices
/// would keep their shape and start coming out of the wrong file. This is the
/// assertion that fails if that call moves.
#[test]
fn a_second_derivation_agrees_with_the_first() {
    let repo = Repo::compiled_in().expect("this crate lives in a rub3 checkout");
    let first = rustapi::workspace(&repo).expect("the workspace derives");
    let second = rustapi::workspace(&repo).expect("the workspace derives again");

    assert_eq!(
        serde_json::to_value(&first).expect("serialises"),
        serde_json::to_value(&second).expect("serialises"),
        "a second derivation of an unchanged workspace must not move"
    );
    for crate_api in &second {
        for module in &crate_api.modules {
            for item in &module.items {
                let source = repo.read(&item.path).expect("a declaring file reads");
                let served = item
                    .signature
                    .strip_suffix(" = ...;")
                    .unwrap_or(&item.signature);
                assert!(
                    source.contains(served),
                    "{} serves {served:?} on the second derivation, which is not in the file",
                    item.path
                );
            }
        }
    }
}

/// The wrapper's two front doors answer with their real declarations.
///
/// `supervisor_run` is one hop out of a private file module and
/// `ensure_headless` is two: `lib.rs` re-exports what `activation.rs`
/// re-exports out of its private `headless` module. Both are the names an
/// integrator writes, and neither declaration appears anywhere else in the
/// listing, so this is the case that decides whether the tool hands over a
/// signature or a bare `pub use` line.
#[test]
fn the_wrappers_reexported_front_doors_carry_their_declarations() {
    let repo = Repo::compiled_in().expect("this crate lives in a rub3 checkout");
    let api = rustapi::workspace(&repo).expect("the workspace derives");

    for (module, name, declared_in) in [
        (
            "lib",
            "supervisor_run",
            "crates/rub3-wrapper/src/supervisor.rs",
        ),
        (
            "activation",
            "ensure_headless",
            "crates/rub3-wrapper/src/activation.rs",
        ),
        (
            "lib",
            "ensure_headless",
            "crates/rub3-wrapper/src/activation.rs",
        ),
    ] {
        let (signature, path, line) = reexported(&api, module, name).unwrap_or_else(|| {
            panic!("{module} re-exports {name} and must serve the declaration behind it")
        });
        assert_eq!(path, declared_in);
        let source = repo.read(&path).expect("the declaring file reads");
        assert!(
            source.contains(&signature),
            "{path} serves {signature:?} for {name}, which is not in the file"
        );
        assert!(
            starts_the_declaration(&source, line, &signature),
            "{path} serves {name} as starting on line {line}, which holds something else"
        );
        assert!(
            signature.starts_with("pub fn ") && signature.contains('('),
            "{name} is served as {signature:?}, which is not a callable declaration"
        );
    }
}

/// Every signature served for the workspace appears verbatim in its own file.
///
/// This is the property that makes the whole crate trustworthy: the served text
/// is `source[span]`, so it cannot drift from the source without the source
/// changing. A signature assembled by re-rendering an AST would fail here the
/// first time rustfmt broke a long parameter list across lines.
#[test]
fn every_served_rust_signature_is_a_verbatim_slice_of_its_file() {
    let repo = Repo::compiled_in().expect("this crate lives in a rub3 checkout");
    let api = rustapi::workspace(&repo).expect("the workspace derives");
    assert!(
        api.iter().any(|crate_api| crate_api.name == "rub3-wrapper"),
        "the wrapper is a workspace member and must be described"
    );

    let mut checked = 0;
    for crate_api in &api {
        for module in &crate_api.modules {
            let check = |item: &rustapi::Item| {
                // Against the file the item says it came from, which for a
                // resolved re-export is the file that declares it rather than
                // the one the `pub use` statement is in.
                let source = repo.read(&item.path).expect("a declaring file reads");
                // A value declaration too long to serve whole is cut after its
                // type; the part served is still a slice, so the assertion is
                // on that part.
                let served = item
                    .signature
                    .strip_suffix(" = ...;")
                    .unwrap_or(&item.signature);
                assert!(
                    source.contains(served),
                    "{} serves {:?} for {} {}, which is not in the file",
                    item.path,
                    served,
                    item.kind,
                    item.name
                );
                assert!(
                    starts_the_declaration(&source, item.line, served),
                    "{} serves {} {} as starting on line {}, which holds something else",
                    item.path,
                    item.kind,
                    item.name,
                    item.line
                );
                for cfg in &item.cfg {
                    assert!(
                        source.contains(cfg),
                        "{} serves cfg {cfg:?}, which is not in the file",
                        item.path
                    );
                }
            };
            for item in &module.items {
                check(item);
                for member in &item.members {
                    check(member);
                }
                checked += 1 + item.members.len();
            }
            let root_path = format!("{}/src/lib.rs", crate_api.path);
            let root = repo.read(&root_path).expect("the crate root reads");
            for cfg in &module.cfg {
                assert!(
                    root.contains(cfg),
                    "{} serves module cfg {cfg:?}, which is not in the crate root",
                    module.name
                );
            }
            // Completeness, not only verbatimness: a subset of a module's gates
            // is a listing that understates what a build needs, and it is the
            // half no containment check can see.
            let declared = declared_cfg_count(&root, &module.name);
            assert_eq!(
                module.cfg.len(),
                declared,
                "{} declares {declared} cfg attributes on `mod {}` and {} were served",
                root_path,
                module.name,
                module.cfg.len()
            );
        }
    }
    assert!(
        checked > 100,
        "only {checked} items checked, which means the walk stopped finding the workspace"
    );
}

/// A served signature is a declaration, and only a declaration.
///
/// `syn` spans cover an item's attributes, so slicing an item's whole span hands
/// back its doc comment and any `#[cfg]` above it as part of the "signature".
/// That reads as noise in a tool result and, worse, makes the signature
/// unquotable: an agent pasting it into a call site pastes a comment with it.
/// Each kind is therefore sliced from the token that begins the declaration, and
/// this is the assertion that keeps it that way as new kinds are added.
#[test]
fn no_served_signature_carries_an_attribute_or_a_doc_comment() {
    let repo = Repo::compiled_in().expect("this crate lives in a rub3 checkout");
    let api = rustapi::workspace(&repo).expect("the workspace derives");
    for crate_api in &api {
        for module in &crate_api.modules {
            let check = |item: &rustapi::Item| {
                for offender in ["///", "//!", "#["] {
                    assert!(
                        !item.signature.contains(offender),
                        "{} serves {} {} with {offender} inside its signature: {:?}",
                        module.path,
                        item.kind,
                        item.name,
                        item.signature
                    );
                }
                assert!(
                    !item.signature.is_empty(),
                    "{} serves {} {} with an empty signature",
                    module.path,
                    item.kind,
                    item.name
                );
            };
            for item in &module.items {
                check(item);
                for member in &item.members {
                    check(member);
                }
            }
        }
    }
}

/// Two independent fields of the same artifact have to agree.
///
/// `methodIdentifiers` is solc's own map from canonical signature to selector.
/// Rendering a signature means expanding tuple parameters back into their member
/// types and keeping any array suffix - `tuple[]` over two `uint256` members is
/// `(uint256,uint256)[]` - and getting that wrong changes the selector while
/// still looking plausible. This test is what catches it: every rendered
/// signature must be a key of that map, with the selector to match.
#[test]
fn every_rendered_function_signature_matches_the_artifacts_own_selector_map() {
    let repo = Repo::compiled_in().expect("this crate lives in a rub3 checkout");
    let set = match solidity::contracts(&repo) {
        Ok(set) => set,
        Err(error) => return skip_or_fail(&format!("{error}")),
    };

    let mut checked = 0;
    for contract in &set.contracts {
        let abi = solidity::abi(&repo, &contract.name, false)
            .unwrap_or_else(|error| panic!("{}: {error}", contract.name));
        let artifact: serde_json::Value =
            serde_json::from_str(&repo.read(&contract.artifact).expect("the artifact reads"))
                .expect("the artifact is JSON");
        let identifiers = artifact
            .get("methodIdentifiers")
            .and_then(serde_json::Value::as_object)
            .cloned()
            .unwrap_or_default();

        for entry in &abi.functions {
            let expected = identifiers.get(&entry.signature).unwrap_or_else(|| {
                panic!(
                    "{} rendered {:?}, which is not a key of the artifact's methodIdentifiers. \
                     Keys: {:?}",
                    contract.name,
                    entry.signature,
                    identifiers.keys().collect::<Vec<_>>()
                )
            });
            assert_eq!(
                entry.selector.as_deref(),
                Some(format!("0x{}", expected.as_str().expect("a hex selector")).as_str()),
                "{} serves the wrong selector for {}",
                contract.name,
                entry.signature
            );
            checked += 1;
        }

        // Every function solc emitted a selector for must be served. An entry
        // dropped by the renderer would otherwise pass the loop above in
        // silence.
        let served: Vec<&str> = abi
            .functions
            .iter()
            .map(|entry| entry.signature.as_str())
            .collect();
        for signature in identifiers.keys() {
            assert!(
                served.contains(&signature.as_str()),
                "{} has a selector for {signature} in its artifact and serves no such function",
                contract.name
            );
        }
    }
    assert!(
        checked > 50,
        "only {checked} signatures cross-checked against {} contracts",
        set.contracts.len()
    );
}

/// The published fingerprints and the built contracts describe the same set.
///
/// The manifest is the record the wrapper attests against, so a contract served
/// as deployable with no fingerprint means the fingerprint gate has not been run
/// since it was added. That is the gate's failure to report, not this crate's,
/// and this test says which contract it is rather than leaving the ABI surface
/// quietly incomplete.
#[test]
fn every_deployable_contract_carries_a_published_fingerprint() {
    let repo = Repo::compiled_in().expect("this crate lives in a rub3 checkout");
    let set = match solidity::contracts(&repo) {
        Ok(set) => set,
        Err(error) => return skip_or_fail(&format!("{error}")),
    };
    let manifest: serde_json::Value = serde_json::from_str(
        &repo
            .read("contracts/canonical-bytecode.json")
            .expect("the manifest reads"),
    )
    .expect("the manifest is JSON");

    for contract in &set.contracts {
        // A library compiles to runtime code but carries a self-address
        // placeholder rather than immutables, so the fingerprint gate publishes
        // none for one. There is no library under contracts/src today; the
        // exemption is here so that adding one does not read as drift.
        if !contract.deployable || contract.kind == "library" {
            continue;
        }
        assert!(
            contract.canonical.is_some(),
            "{} is deployable and has no entry in contracts/canonical-bytecode.json. Run \
             scripts/canonical-bytecode-hashes.sh update and commit the result in the same pull \
             request.",
            contract.name
        );
        assert!(
            manifest
                .pointer(&format!("/contracts/{}", contract.name))
                .is_some(),
            "{} was served a fingerprint that the manifest does not publish",
            contract.name
        );
    }
}

/// The document inventory is a walk, not a list.
#[test]
fn the_document_inventory_finds_a_document_added_after_the_server_started() {
    let fixture = Fixture::new();
    let server = fixture.server();

    let paths = |server: &DocsServer| {
        server
            .list_documents()
            .expect("the fixture has documents")
            .0
            .documents
            .into_iter()
            .map(|document| document.path)
            .collect::<Vec<_>>()
    };
    assert_eq!(paths(&server), vec!["README.md".to_string()]);

    fixture.write("docs/late.md", "# Late\n\nAdded after startup.\n");
    assert_eq!(
        paths(&server),
        vec!["README.md".to_string(), "docs/late.md".to_string()],
        "the inventory is walked at call time"
    );
}

/// One document the walk cannot read costs that document and nothing else.
///
/// The inventory is the spine of all three document tools, so a hard failure on
/// a stray latin-1 file in somebody's checkout would take `read_document` and
/// `search_documents` down with it over a file nobody asked for.
#[test]
fn a_document_that_cannot_be_read_does_not_take_the_others_down() {
    let fixture = Fixture::new();
    let server = fixture.server();
    fixture.write_bytes("broken.md", &[b'#', b' ', 0x80, 0xff, b'\n']);

    let listed: Vec<String> = server
        .list_documents()
        .expect("the readable documents are still served")
        .0
        .documents
        .into_iter()
        .map(|document| document.path)
        .collect();
    assert_eq!(
        listed,
        vec!["README.md".to_string()],
        "the unreadable document is skipped and the rest of the inventory answers"
    );

    let read = server
        .read_document(Parameters(rub3_docs_mcp::server::ReadDocument {
            path: "README.md".to_string(),
            section: None,
        }))
        .expect("reading another document still works")
        .0;
    assert!(read.text.contains("The demo fixture"));

    let hits = server
        .search_documents(Parameters(rub3_docs_mcp::server::SearchDocuments {
            query: "fixture".to_string(),
            limit: None,
        }))
        .expect("search still works")
        .0;
    assert_eq!(hits.matches.len(), 1);
}

#[test]
fn a_document_read_cannot_escape_the_checkout() {
    let fixture = Fixture::new();
    let server = fixture.server();
    let error = match server.read_document(Parameters(rub3_docs_mcp::server::ReadDocument {
        path: "../../../etc/passwd".to_string(),
        section: None,
    })) {
        Err(error) => error,
        Ok(_) => panic!("a path outside the inventory must be refused"),
    };
    let message = format!("{error:?}");
    assert!(
        message.contains("no document at"),
        "the refusal should name the inventory, got: {message}"
    );
}

/// Section extraction against a real specification, not a fixture.
#[test]
fn a_real_section_is_served_with_its_own_line_range() {
    let repo = Repo::compiled_in().expect("this crate lives in a rub3 checkout");
    let source = repo.read("README.md").expect("README reads");
    let section = docs::section(&source, "Project structure").expect("README has that heading");
    let lines: Vec<&str> = source.lines().collect();
    assert_eq!(lines[section.start_line - 1].trim(), "## Project structure");
    assert_eq!(
        section.text,
        lines[section.start_line - 1..section.end_line].join("\n"),
        "the served text is the lines it says it is"
    );
}

/// The artifact-dependent tests pass when forge has not run, and fail when CI
/// says forge has.
///
/// `contracts/out` is not checked in, so a contributor who has never run
/// `forge build` would otherwise see a failure that says nothing about their
/// change. In CI the artifacts are built first and
/// `RUB3_DOCS_MCP_REQUIRE_ARTIFACTS=1` turns the same skip into a failure,
/// because a suite that silently ran nothing is not green.
fn skip_or_fail(reason: &str) {
    if std::env::var_os("RUB3_DOCS_MCP_REQUIRE_ARTIFACTS").is_some() {
        panic!("RUB3_DOCS_MCP_REQUIRE_ARTIFACTS is set and the artifacts are unusable: {reason}");
    }
    println!("SKIP: {reason}");
}
