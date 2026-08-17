//! The worked `rub3` SDK integration: an application that asks the wrapper who
//! launched it and prints the answer.
//!
//! It is two things at once, and both are on purpose. As a diagnostic it answers
//! "is the channel up, and what does it say" for a developer wiring a wrapped
//! application together. As the child process of `tests/sdk_e2e.rs` it is the
//! application in an end-to-end launch, which is why its output is `key=value`
//! lines rather than prose.
//!
//! ```text
//! rub3-sdk-probe            # heartbeat, then the session; panics if either fails
//! rub3-sdk-probe heartbeat  # heartbeat only
//! rub3-sdk-probe session    # session only
//! rub3-sdk-probe try        # the same two checks, reported instead of panicked
//! ```
//!
//! The panicking modes are what an application does at startup, and are how
//! `rub3::heartbeat`'s documented failure is observed: run this binary with no
//! wrapper and it exits 101 with the panic text on stderr.

fn main() {
    let mode = std::env::args().nth(1).unwrap_or_default();

    match mode.as_str() {
        "" | "all" => {
            heartbeat();
            session();
        }
        "heartbeat" => heartbeat(),
        "session" => session(),
        "try" => std::process::exit(report()),
        other => {
            eprintln!("rub3-sdk-probe: unknown mode {other:?}");
            eprintln!("rub3-sdk-probe: modes are: all, heartbeat, session, try");
            std::process::exit(2);
        }
    }
}

/// The startup assertion, exactly as an application writes it.
fn heartbeat() {
    rub3::heartbeat();
    println!("heartbeat=ok");
}

/// The session, printed one field per line.
fn session() {
    print_session(&rub3::session());
}

fn print_session(session: &rub3::SessionInfo) {
    println!("app_id={}", session.app_id);
    println!("token_id={}", session.token_id);
    // `user_id` is the key an application stores data under; `wallet` is
    // incidental and printed only because a probe exists to show what crossed.
    println!("user_id={}", session.user_id);
    println!("wallet={}", session.wallet);
    println!("identity={}", session.identity);
    println!(
        "expires_at={}",
        session
            .expires_at
            .map(|t| t.to_rfc3339())
            .unwrap_or_else(|| "none".to_string())
    );
    println!("expired={}", session.is_expired());
}

/// The non-panicking path, for an application that would rather degrade than
/// die - and for a test that wants the failure as data.
fn report() -> i32 {
    if let Err(e) = rub3::try_heartbeat() {
        println!("error_kind={}", kind(&e));
        println!("error={e}");
        return 2;
    }
    println!("heartbeat=ok");

    match rub3::try_session() {
        Ok(session) => {
            print_session(&session);
            0
        }
        Err(e) => {
            println!("error_kind={}", kind(&e));
            println!("error={e}");
            2
        }
    }
}

/// A stable token per failure, so a caller can branch on the cause without
/// matching on prose.
fn kind(e: &rub3::Error) -> &'static str {
    match e {
        rub3::Error::NotWrapped => "not_wrapped",
        rub3::Error::Unreachable(_) => "unreachable",
        rub3::Error::Transport(_) => "transport",
        rub3::Error::Protocol(_) => "protocol",
        rub3::Error::ProtocolVersion { .. } => "protocol_version",
        rub3::Error::NoSession(_) => "no_session",
        rub3::Error::Wrapper(_) => "wrapper",
    }
}
