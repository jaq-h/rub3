//! `rub3`, the developer- and agent-facing CLI of implementation.md §2.5.
//!
//! Two subcommands, and the plan lists two more that are deliberately absent:
//! `fetch` (§3.1) and `register` (§3.2) are the agent-side halves of content
//! addressed distribution and of the discovery registry, and neither of those
//! exists yet. A subcommand that cannot work is worse than an absent one, so
//! they are not here in any form.
//!
//! | Module | What |
//! |---|---|
//! | [`deployments`] | `contracts/deployments.json`, and the refusal when it publishes no factory |
//! | [`repo`] | finding the checkout both subcommands work in |
//! | [`tier`] | tier bundles and front doors, named as an operator names them |
//! | [`pack`] | wrapper + application + configuration, as one distributable |
//! | [`deploy`] | a licence contract, through the canonical factory by default |
//!
//! The one rule that runs through all of it: **the canonical `Rub3Factory`
//! comes out of `contracts/deployments.json` and from nowhere else.** `pack`
//! compiles it into the binary so a wrapper can tell a canonical deploy from
//! any other with no network round trip, `deploy` passes it to the deploy
//! script, and both refuse where that file says `null`, which is everywhere
//! until launch. See [`deployments`] for why that is the whole design rather
//! than a strictness setting.

pub mod deploy;
pub mod deployments;
pub mod pack;
pub mod repo;
pub mod tier;
