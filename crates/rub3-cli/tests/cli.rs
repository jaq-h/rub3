//! The `rub3` binary, driven the way an operator drives it.
//!
//! The refusals are what these tests are mostly about. Nothing is deployed to
//! any public network, so the canonical path through `pack` and `deploy` cannot
//! be exercised against a real address today - it is exercised against a
//! fixture checkout whose manifest publishes one, which is the only honest way
//! to prove that path works before there is something to point at.

use std::path::Path;
use std::process::{Command, Output};

/// The manifest the repository actually ships: every entry null.
const COMMITTED_MANIFEST: &str = include_str!("../../../contracts/deployments.json");
/// A manifest that publishes a factory on one chain. Fabricated; see its note.
const POPULATED_MANIFEST: &str = include_str!("fixtures/deployments-populated.json");

/// Builds a directory that looks enough like a rub3 checkout to be resolved as
/// one, carrying whichever manifest a test wants to reason about.
fn checkout(manifest: &str) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::write(root.join("Cargo.toml"), "[workspace]\n").unwrap();
    std::fs::create_dir_all(root.join("crates/rub3-wrapper")).unwrap();
    std::fs::write(root.join("crates/rub3-wrapper/Cargo.toml"), "").unwrap();
    std::fs::create_dir_all(root.join("contracts")).unwrap();
    std::fs::write(root.join("contracts/deployments.json"), manifest).unwrap();
    tmp
}

fn rub3(root: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_rub3"))
        .arg("--repo-root")
        .arg(root)
        .args(args)
        .output()
        .expect("the rub3 binary runs")
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).to_string()
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).to_string()
}

/// A pack that would otherwise succeed: everything valid but the factory.
fn pack_args() -> Vec<&'static str> {
    vec![
        "pack",
        "--binary",
        env!("CARGO_BIN_EXE_rub3"),
        "--app-id",
        "com.example.myapp",
        "--contract",
        "0x1234567890abcdef1234567890abcdef12345678",
        "--chain",
        "base",
        "--tier",
        "cooldown",
        "--session-ttl",
        "7",
        "--output",
        "/tmp/rub3-cli-test-output-that-is-never-written",
        "--dry-run",
    ]
}

fn deploy_args() -> Vec<&'static str> {
    vec![
        "deploy",
        "--type",
        "access",
        "--name",
        "My App License",
        "--symbol",
        "MAL",
        "--identity",
        "access",
        "--price-eth",
        "0.05",
        "--chain",
        "base",
        "--dry-run",
    ]
}

#[test]
fn pack_refuses_a_chain_with_no_canonical_factory() {
    let repo = checkout(COMMITTED_MANIFEST);
    let output = rub3(repo.path(), &pack_args());

    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
    let message = stderr(&output);
    assert!(message.contains("base"), "{message}");
    assert!(message.contains("8453"), "{message}");
    assert!(message.contains("contracts/deployments.json"), "{message}");
    // The refusal must not hand anybody something to paste.
    assert!(
        !message.contains("0x0000000000000000000000000000000000000000"),
        "the refusal offers a zero address: {message}"
    );
    assert!(
        stdout(&output).is_empty(),
        "a refused pack printed a plan: {}",
        stdout(&output)
    );
}

#[test]
fn deploy_refuses_a_chain_with_no_canonical_factory() {
    let repo = checkout(COMMITTED_MANIFEST);
    let output = rub3(repo.path(), &deploy_args());

    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
    let message = stderr(&output);
    assert!(message.contains("base"), "{message}");
    assert!(message.contains("8453"), "{message}");
    assert!(
        message.contains("--direct"),
        "the way out is named: {message}"
    );
    assert!(
        !message.contains("0x0000000000000000000000000000000000000000"),
        "the refusal offers a zero address: {message}"
    );
}

#[test]
fn a_published_factory_is_what_gets_baked_in() {
    let repo = checkout(POPULATED_MANIFEST);
    let output = rub3(repo.path(), &pack_args());

    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let plan = stdout(&output);
    assert!(
        plan.contains("0xf4c70a7000000000000000000000000000000001"),
        "the fixture's factory is not in the plan: {plan}"
    );
    assert!(
        plan.contains("canonical, from contracts/deployments.json"),
        "{plan}"
    );
    assert!(
        plan.contains("RUB3_PACK") || plan.contains("cargo build"),
        "{plan}"
    );
    assert!(plan.contains("--no-default-features"), "{plan}");
    assert!(plan.contains("--features tier-3"), "{plan}");
}

#[test]
fn a_published_factory_is_what_a_deploy_goes_through() {
    let repo = checkout(POPULATED_MANIFEST);
    let output = rub3(repo.path(), &deploy_args());

    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let plan = stdout(&output);
    assert!(
        plan.contains("FACTORY=0xf4c70a7000000000000000000000000000000001"),
        "{plan}"
    );
    assert!(plan.contains("PRICE=50000000000000000"), "{plan}");
    assert!(plan.contains("forge script script/Deploy.s.sol"), "{plan}");
    assert!(
        plan.contains("--rpc-url base"),
        "the chain name is foundry.toml's own alias: {plan}"
    );
    assert!(
        plan.contains("forge simulates only"),
        "a deploy must not broadcast unless asked: {plan}"
    );
}

#[test]
fn the_second_chain_in_the_same_manifest_is_still_refused() {
    // The two records have independent lifecycles: one chain publishing says
    // nothing about the other, and nothing may be carried across.
    let repo = checkout(POPULATED_MANIFEST);
    let mut args = pack_args();
    let chain = args.iter().position(|a| *a == "base").unwrap();
    args[chain] = "base_sepolia";
    let output = rub3(repo.path(), &args);

    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
    assert!(stderr(&output).contains("84532"), "{}", stderr(&output));
}

#[test]
fn a_zero_address_in_the_manifest_is_refused_rather_than_used() {
    let poisoned = COMMITTED_MANIFEST.replace(
        "\"factory\": null",
        "\"factory\": \"0x0000000000000000000000000000000000000000\"",
    );
    let repo = checkout(&poisoned);
    let output = rub3(repo.path(), &pack_args());

    assert_ne!(output.status.code(), Some(0), "{}", stdout(&output));
    assert!(
        stderr(&output).contains("zero address"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn an_explicit_factory_is_the_only_other_way_to_get_one() {
    let repo = checkout(COMMITTED_MANIFEST);
    let mut args = pack_args();
    args.push("--factory");
    args.push("0xf4c70a7000000000000000000000000000000002");
    let output = rub3(repo.path(), &args);

    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let plan = stdout(&output);
    assert!(
        plan.contains("not a canonical deploy"),
        "a binary packed this way must say what it is: {plan}"
    );
}

#[test]
fn an_explicit_factory_may_not_be_the_zero_address_either() {
    let repo = checkout(COMMITTED_MANIFEST);
    let mut args = pack_args();
    args.push("--factory");
    args.push("0x0000000000000000000000000000000000000000");
    let output = rub3(repo.path(), &args);

    assert_eq!(output.status.code(), Some(1), "{}", stdout(&output));
    assert!(
        stderr(&output).contains("zero address"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn an_unknown_chain_name_is_refused_rather_than_guessed_at() {
    let repo = checkout(COMMITTED_MANIFEST);
    let mut args = pack_args();
    let chain = args.iter().position(|a| *a == "base").unwrap();
    args[chain] = "arbitrum";
    let output = rub3(repo.path(), &args);

    assert_eq!(output.status.code(), Some(1), "{}", stdout(&output));
    let message = stderr(&output);
    assert!(message.contains("arbitrum"), "{message}");
    assert!(message.contains("base (8453)"), "{message}");
}

#[test]
fn a_chain_id_the_manifest_does_not_answer_for_still_needs_a_factory() {
    // A local anvil is addressable; what it has no answer for is which factory
    // is canonical on it.
    let repo = checkout(COMMITTED_MANIFEST);
    let mut args = pack_args();
    let chain = args.iter().position(|a| *a == "base").unwrap();
    args[chain] = "31337";
    let output = rub3(repo.path(), &args);

    assert_ne!(output.status.code(), Some(0), "{}", stdout(&output));
    assert!(stderr(&output).contains("31337"), "{}", stderr(&output));
}

#[test]
fn a_stablecoin_price_needs_the_token_it_settles_in() {
    let repo = checkout(POPULATED_MANIFEST);
    let mut args = deploy_args();
    args.push("--price-usdc");
    args.push("20");
    let output = rub3(repo.path(), &args);

    assert_eq!(output.status.code(), Some(1), "{}", stdout(&output));
    let message = stderr(&output);
    assert!(message.contains("--price-token"), "{message}");
    assert!(
        message.contains("never be guessed"),
        "the reason is the point: {message}"
    );
}

#[test]
fn a_stablecoin_price_becomes_the_tokens_own_units() {
    let repo = checkout(POPULATED_MANIFEST);
    let mut args = deploy_args();
    args.extend([
        "--price-usdc",
        "20",
        "--price-token",
        "0xf4c70a7000000000000000000000000000000003",
    ]);
    let output = rub3(repo.path(), &args);

    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let plan = stdout(&output);
    assert!(plan.contains("PRICE_AMOUNT=20000000"), "{plan}");
    assert!(
        plan.contains("PRICE_TOKEN=0xf4c70a7000000000000000000000000000000003"),
        "{plan}"
    );
}

#[test]
fn a_direct_deploy_passes_no_factory_at_all() {
    let repo = checkout(COMMITTED_MANIFEST);
    let mut args = deploy_args();
    args.push("--direct");
    let output = rub3(repo.path(), &args);

    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let plan = stdout(&output);
    assert!(
        !plan.contains("FACTORY="),
        "a direct deploy names no factory: {plan}"
    );
    assert!(
        plan.contains("removed from the child's environment") && plan.contains("FACTORY"),
        "an inherited FACTORY would silently change the deploy: {plan}"
    );
    assert!(plan.contains("no factory records it"), "{plan}");
}

#[test]
fn an_account_model_deploy_needs_its_tba_implementation() {
    let repo = checkout(POPULATED_MANIFEST);
    let mut args = deploy_args();
    let identity = args.iter().position(|a| *a == "access").unwrap();
    // The second `access` is --identity's value; the first is --type's.
    let identity = args
        .iter()
        .enumerate()
        .filter(|(_, a)| **a == "access")
        .map(|(i, _)| i)
        .nth(1)
        .unwrap_or(identity);
    args[identity] = "account";
    let output = rub3(repo.path(), &args);

    assert_eq!(output.status.code(), Some(1), "{}", stdout(&output));
    assert!(
        stderr(&output).contains("--tba-implementation"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn a_subscription_needs_its_period() {
    let repo = checkout(POPULATED_MANIFEST);
    let mut args = deploy_args();
    let kind = args.iter().position(|a| *a == "access").unwrap();
    args[kind] = "subscription";
    let output = rub3(repo.path(), &args);

    assert_eq!(output.status.code(), Some(1), "{}", stdout(&output));
    assert!(stderr(&output).contains("--period"), "{}", stderr(&output));
}

#[test]
fn there_is_no_fetch_and_no_register_subcommand() {
    // implementation.md §2.5 lists four subcommands. `fetch` (§3.1) and
    // `register` (§3.2) have nothing to talk to yet, and a subcommand that
    // cannot work is worse than an absent one.
    let repo = checkout(COMMITTED_MANIFEST);
    for absent in ["fetch", "register"] {
        let output = rub3(repo.path(), &[absent]);
        assert_ne!(output.status.code(), Some(0), "`rub3 {absent}` ran");
    }
    let help = rub3(repo.path(), &["--help"]);
    let text = stdout(&help);
    assert!(!text.contains("fetch"), "{text}");
    assert!(!text.contains("register"), "{text}");
}

#[test]
fn a_directory_that_is_not_a_checkout_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let output = rub3(tmp.path(), &pack_args());
    assert_eq!(output.status.code(), Some(1), "{}", stdout(&output));
    assert!(
        stderr(&output).contains("is not a rub3 checkout"),
        "{}",
        stderr(&output)
    );
}
