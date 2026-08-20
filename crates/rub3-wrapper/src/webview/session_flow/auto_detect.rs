//! The §5.1a auto-detect front door, driven at the IPC seam.
//!
//! Auto-detect exists to remove one step from the manual flow: the paste. Its
//! whole correctness claim is therefore a negative one - that *nothing else*
//! changes - so what is asserted here is mostly sameness and stopping:
//!
//!   * the flow a found hash produces is call-for-call the flow a pasted hash
//!     produces, so the §1.7 and §1.8 Phase B poller and finalize path have no
//!     second branch to grow;
//!   * a watch stops when the screen that started it stops caring, whether the
//!     page switched tabs or moved on entirely;
//!   * a build that cannot reach its endpoint lands on the manual tab with
//!     something useful said, rather than on an error screen.
//!
//! The seam is [`super::Window`] - see that module's header for what it does
//! and does not reach. Everything below runs against a
//! [`StubNode`](crate::test_support::StubNode), so no Foundry toolchain and no
//! network are involved; the live-EVM arm is in [`super::onchain`].

use std::time::{Duration, Instant};

use super::Window;
use crate::test_support::StubNode;

const APP_ID: &str = "com.rub3.auto-detect-test";
const CHAIN_ID: u64 = 8453;
const SESSION_TTL_SECS: i64 = 3600;

const CONTRACT: &str = "0x000000000000000000000000000000000000dEaD";
const WALLET: &str = "0x00000000000000000000000000000000000B0B0b";
const MINT_TX: &str = "0x1111111111111111111111111111111111111111111111111111111111111111";
const TOKEN_ID: u64 = 137;

/// The head the stub reports, and the block the watch therefore starts from.
const HEAD: u64 = 0x28;
/// The block the mint landed in. After [`HEAD`], as a real one would be.
const MINT_BLOCK: u64 = 0x2a;

/// Port 1 on loopback, where nothing listens.
const UNREACHABLE_RPC: &str = "http://127.0.0.1:1";

fn address_topic(address: &str) -> String {
    format!("0x{:0>64}", address.trim_start_matches("0x").to_lowercase())
}

fn block_hash() -> String {
    "0x0000000000000000000000000000000000000000000000000000000000000042".to_string()
}

/// The ERC-721 mint log a `purchase()` leaves behind, in the two places the
/// flow reads it from: the `eth_getLogs` the watch makes, and the receipt the
/// poller then fetches.
fn mint_log() -> serde_json::Value {
    use alloy::primitives::b256;
    const TRANSFER_SIG: alloy::primitives::B256 =
        b256!("ddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef");

    serde_json::json!({
        "address": CONTRACT.to_lowercase(),
        "topics": [
            format!("0x{}", hex::encode(TRANSFER_SIG.as_slice())),
            format!("0x{:0>64}", ""),
            address_topic(WALLET),
            format!("0x{TOKEN_ID:064x}"),
        ],
        "data": "0x",
        "blockNumber": format!("0x{MINT_BLOCK:x}"),
        "blockHash": block_hash(),
        "transactionHash": MINT_TX,
        "transactionIndex": "0x0",
        "logIndex": "0x0",
        "removed": false,
    })
}

/// A node that has already seen the buyer's `purchase()` land.
///
/// Answers the four calls the whole purchase-to-cooldown path makes: the head
/// the watch starts from, the logs it matches on, the receipt the poller and
/// the token-id lookup both read, and the `cooldownReady` view the cooldown
/// screen is built from. Both front doors drive the same node, which is what
/// makes the comparison between them a comparison of the doors alone.
fn purchased_node() -> StubNode {
    StubNode::routed(|method, _params| match method {
        "eth_blockNumber" => serde_json::json!(format!("0x{HEAD:x}")),
        "eth_getLogs" => serde_json::json!([mint_log()]),
        "eth_getTransactionReceipt" => serde_json::json!({
            "type": "0x2",
            "transactionHash": MINT_TX,
            "transactionIndex": "0x0",
            "blockHash": block_hash(),
            "blockNumber": format!("0x{MINT_BLOCK:x}"),
            "from": WALLET.to_lowercase(),
            "to": CONTRACT.to_lowercase(),
            "cumulativeGasUsed": "0x1",
            "gasUsed": "0x1",
            "effectiveGasPrice": "0x1",
            "contractAddress": null,
            "logs": [mint_log()],
            "logsBloom": format!("0x{:0>512}", ""),
            "status": "0x1",
        }),
        // The only view the cooldown screen reads: `cooldownReady(tokenId)`,
        // answering "ready, nothing remaining".
        "eth_call" => serde_json::json!(format!("0x{:064x}{:064x}", 1, 0)),
        _ => serde_json::json!(null),
    })
}

fn window_on(rpc_url: &str) -> Window {
    Window::open(APP_ID, CONTRACT, CHAIN_ID, rpc_url, SESSION_TTL_SECS)
}

/// The claim §5.1a is built on: auto-detect finds the hash, and then does
/// nothing else.
///
/// Both doors are driven against the same node, and the calls they produce are
/// compared whole rather than spot-checked - a second finalize path would have
/// to differ somewhere, and comparing the sequences is what makes "somewhere"
/// enough. That includes the processing message, which is emitted by the
/// poller: if auto-detect ever grew its own, this is where it would show up.
///
/// Gated on `cooldown` because the screen the flow arrives at is the cooldown
/// one. Nothing shipped separates the two flags, but they are independently
/// selectable, and a build without `cooldown` would land on `onShowActivate`
/// instead and wait out the timeout here for no reason.
#[test]
#[cfg(feature = "cooldown")]
fn a_found_hash_and_a_pasted_hash_drive_the_same_flow() {
    let node = purchased_node();

    let manual = window_on(&node.url);
    manual.post(serde_json::json!({
        "type":          "purchase_tx_sent",
        "tx_hash":       MINT_TX,
        "owner_address": WALLET,
    }));
    let pasted = manual.calls_until("onShowCooldown");

    let auto = window_on(&node.url);
    auto.post(serde_json::json!({
        "type":          "auto_watch_start",
        "kind":          "mint",
        "owner_address": WALLET,
        "token_id":      serde_json::Value::Null,
    }));
    let found = auto.calls_until("onShowCooldown");

    assert_eq!(
        found, pasted,
        "auto-detect must reach the cooldown screen by the same route a paste does",
    );
    assert_eq!(
        found.last().expect("a flow reaches a screen").arg["tokenId"],
        TOKEN_ID,
        "the minted token id comes from the Transfer log either way",
    );
}

/// A watch stops when the page switches away from the Auto-detect tab.
///
/// Observed from outside, by counting what reaches the endpoint: a watch that
/// was told to stop and did stops asking, and one that leaked goes on asking
/// for the rest of its two-minute budget whether or not anything is listening.
/// The wait is deliberately longer than a poll interval, because anything
/// shorter cannot tell a stopped watch from a sleeping one.
#[test]
fn a_watch_stops_when_the_page_switches_tabs() {
    let node = quiet_node();
    let window = window_on(&node.url);

    start_and_settle(&window, &node);
    window.post(serde_json::json!({ "type": "auto_watch_cancel" }));

    assert_quiet(&node, "after auto_watch_cancel");
}

/// And it stops when the page moves on without saying so.
///
/// The cancel above is the polite case. This is the one that has to hold
/// anyway: the page posts something else entirely, which means the screen that
/// wanted the answer is gone. Every inbound message stops the watch for exactly
/// this reason, so no future screen transition has to remember to.
#[test]
fn a_watch_stops_when_the_page_moves_on() {
    let node = quiet_node();
    let window = window_on(&node.url);

    start_and_settle(&window, &node);
    window.post(serde_json::json!({ "type": "ready" }));

    assert_quiet(&node, "after the page posted an unrelated message");
}

/// Starting a second watch stops the first, so a page that flips between tabs
/// leaves one watch running rather than a thread per flip.
#[test]
fn starting_a_watch_stops_the_one_before_it() {
    let node = quiet_node();
    let window = window_on(&node.url);

    start_and_settle(&window, &node);
    let before = node.request_count();

    // The replacement runs, so the endpoint does keep hearing from this
    // window; what must not happen is both watches asking.
    window.post(serde_json::json!({
        "type":          "auto_watch_start",
        "kind":          "mint",
        "owner_address": WALLET,
        "token_id":      serde_json::Value::Null,
    }));
    settle(&node);
    let after_second_start = node.request_count() - before;

    std::thread::sleep(crate::rpc::WATCH_POLL_INTERVAL + Duration::from_millis(750));
    let over_one_interval = node.request_count() - before - after_second_start;
    assert!(
        over_one_interval <= 1,
        "two watches are running: {over_one_interval} requests in one poll interval",
    );
}

/// A node nobody can reach lands on the manual tab, and says why.
///
/// The distinction the copy carries is the point: "we could not reach the
/// network" and "we watched and saw nothing" call for different next steps, and
/// only one of them is a reason to check your connection.
#[test]
fn an_unreachable_node_lands_on_the_manual_tab() {
    let window = window_on(UNREACHABLE_RPC);
    window.post(serde_json::json!({
        "type":          "auto_watch_start",
        "kind":          "mint",
        "owner_address": WALLET,
        "token_id":      serde_json::Value::Null,
    }));

    let ended = window.wait_for("onAutoWatchEnded");
    assert_eq!(ended["kind"], "mint");
    assert_eq!(
        ended["reason"], "rpc",
        "an endpoint that refused the connection is not a quiet chain: {ended}",
    );
    let detail = ended["detail"]
        .as_str()
        .expect("the manual tab is given words");
    assert!(
        detail.contains("could not reach the network"),
        "the fallback must name what went wrong: {detail:?}",
    );
    assert!(
        detail.contains("paste"),
        "the fallback must point at the box it just switched to: {detail:?}",
    );
}

/// A watch cancelled while it is mid-request says nothing at all.
///
/// The silence on cancellation cannot be read off the error. A watch starts by
/// reading the head, and a cancel raised while that request is in flight
/// surfaces as whatever the request failed with, not as `WatchEnded(Cancelled)`.
/// Reporting it would write "we could not reach the network" and a manual-tab
/// switch into a screen the user has already left, on a watch that was stopped
/// on purpose.
///
/// The node here answers slowly so the cancel reliably lands inside that first
/// request, and then answers with something unusable so the request fails.
#[test]
fn a_watch_cancelled_mid_request_reports_nothing() {
    const ANSWER_AFTER: Duration = Duration::from_millis(750);
    const CANCEL_AFTER: Duration = Duration::from_millis(150);

    let node = StubNode::routed(|_method, _params| {
        std::thread::sleep(ANSWER_AFTER);
        // Not a block number, so the head read this watch begins with fails.
        serde_json::Value::Null
    });
    let window = window_on(&node.url);

    window.post(serde_json::json!({
        "type":          "auto_watch_start",
        "kind":          "mint",
        "owner_address": WALLET,
        "token_id":      serde_json::Value::Null,
    }));
    std::thread::sleep(CANCEL_AFTER);
    window.post(serde_json::json!({ "type": "auto_watch_cancel" }));

    // Long enough for the failing request to come back and be reported, if it
    // were going to be.
    std::thread::sleep(ANSWER_AFTER + Duration::from_millis(750));
    let calls = window.drain();
    assert!(
        calls.is_empty(),
        "a cancelled watch must say nothing, but it said {calls:?}",
    );
}

/// A watch that ran out of time says something different from one that could
/// not reach the endpoint, and neither tells anyone to send a second time.
///
/// The likeliest reader of the timeout sentence has already paid: they sent the
/// transaction and the chain was slower than the budget. A message that reads
/// as failure there invites a second purchase, which is a second licence and a
/// second payment.
#[test]
fn the_two_ways_of_giving_up_say_different_things() {
    use crate::webview::{auto_watch_detail, AutoWatchKind};

    let timed_out = auto_watch_detail(AutoWatchKind::Mint, true);
    let unreachable = auto_watch_detail(AutoWatchKind::Mint, false);
    assert_ne!(timed_out, unreachable);

    assert!(
        timed_out.contains("two minutes"),
        "a person is told how long in words, not in seconds: {timed_out:?}",
    );
    assert!(
        timed_out.contains("do not send it twice"),
        "the timeout copy must not invite a second purchase: {timed_out:?}",
    );

    let activation = auto_watch_detail(AutoWatchKind::Activate, true);
    assert!(
        activation.contains("activation") && timed_out.contains("purchase"),
        "each screen names the transaction it was waiting for: {activation:?} / {timed_out:?}",
    );
}

/// An activation watch waits out the cooldown before it polls for anything, and
/// then polls.
///
/// The contract reverts an `activate()` until the cooldown runs out, so a watch
/// armed the moment the cooldown screen renders is looking for a transaction the
/// chain is guaranteed not to have. On this project's default cooldown of 1800
/// blocks that is roughly an hour of polling the user's endpoint, ending in a
/// fallback to the manual tab fifty-eight minutes before the transaction it is
/// asking for could legally be sent - auto-detect could never succeed there.
///
/// Both halves are asserted because either alone would pass on a broken watch:
/// one that never polls is quiet too, and one that ignores the cooldown reaches
/// the endpoint eventually as well.
#[test]
#[cfg(feature = "cooldown")]
fn an_activation_watch_waits_out_the_cooldown_before_polling() {
    let node = cooling_node();
    let window = window_on(&node.url);

    window.post(serde_json::json!({
        "type":          "auto_watch_start",
        "kind":          "activate",
        "owner_address": WALLET,
        "token_id":      TOKEN_ID,
    }));
    // The head read and the cooldown read the watch opens with, and then the
    // hold: nothing more for longer than a poll interval.
    settle(&node);
    assert_quiet(&node, "while the cooldown it is waiting out has not ended");

    let before = node.request_count();
    let deadline = Instant::now() + cooldown_hold() * 4;
    while node.request_count() == before {
        assert!(
            Instant::now() < deadline,
            "the watch never resumed once its cooldown had passed",
        );
        std::thread::sleep(Duration::from_millis(50));
    }

    window.post(serde_json::json!({ "type": "auto_watch_cancel" }));
}

// ── Watching the watcher ──────────────────────────────────────────────────────

/// A cooldown short enough to sit through in a test, and long enough that a
/// watch ignoring it would be caught polling inside it - [`assert_quiet`] waits
/// out a poll interval plus a margin, and this has to outlast that.
#[cfg(feature = "cooldown")]
const COOLDOWN_BLOCKS_REMAINING: u64 = 3;

/// How long that cooldown is expected to take, from the wrapper's own estimate
/// rather than a second copy of it here.
#[cfg(feature = "cooldown")]
fn cooldown_hold() -> Duration {
    crate::webview::cooldown_wait(COOLDOWN_BLOCKS_REMAINING)
}

/// A node whose token is mid-cooldown: `cooldownReady` says not yet, with
/// [`COOLDOWN_BLOCKS_REMAINING`] to go.
///
/// Nothing else needs an answer, because a watch that respects the cooldown asks
/// nothing else until it has passed - which is what the test is about.
#[cfg(feature = "cooldown")]
fn cooling_node() -> StubNode {
    StubNode::routed(|method, _params| match method {
        "eth_blockNumber" => serde_json::json!(format!("0x{HEAD:x}")),
        "eth_call" => {
            serde_json::json!(format!("0x{:064x}{COOLDOWN_BLOCKS_REMAINING:064x}", 0))
        }
        "eth_getLogs" => serde_json::json!([]),
        _ => serde_json::json!(null),
    })
}

/// A node that answers every poll with "nothing yet", so a watch against it
/// runs until its budget or its cancellation - whichever the test is about.
fn quiet_node() -> StubNode {
    StubNode::routed(|method, _params| match method {
        "eth_blockNumber" => serde_json::json!(format!("0x{HEAD:x}")),
        "eth_getLogs" => serde_json::json!([]),
        _ => serde_json::json!(null),
    })
}

/// Starts a mint watch and returns once it has actually reached the endpoint.
///
/// Without this the cancellation tests would pass against a watch that had not
/// started yet, which is not the thing they are about.
fn start_and_settle(window: &Window, node: &StubNode) {
    window.post(serde_json::json!({
        "type":          "auto_watch_start",
        "kind":          "mint",
        "owner_address": WALLET,
        "token_id":      serde_json::Value::Null,
    }));
    settle(node);
}

/// Waits until the node has been asked something and the poll that asked has
/// gone back to sleep.
fn settle(node: &StubNode) {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut seen = node.request_count();
    while node.request_count() == seen {
        assert!(
            Instant::now() < deadline,
            "the watch never reached the node"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    // Let the rest of one poll's calls arrive before the count is trusted as
    // the resting value.
    loop {
        std::thread::sleep(Duration::from_millis(150));
        let now = node.request_count();
        if now == seen {
            return;
        }
        seen = now;
    }
}

/// Asserts the node hears nothing more for longer than a poll interval.
///
/// A sleeping watch and a stopped one look identical for the first three
/// seconds, so the wait has to clear the interval before the silence means
/// anything.
fn assert_quiet(node: &StubNode, when: &str) {
    let before = node.request_count();
    std::thread::sleep(crate::rpc::WATCH_POLL_INTERVAL + Duration::from_millis(750));
    assert_eq!(
        node.request_count(),
        before,
        "the watch was still polling {when}",
    );
}
