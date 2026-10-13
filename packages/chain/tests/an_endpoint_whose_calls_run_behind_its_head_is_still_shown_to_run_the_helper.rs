//! An endpoint whose calls run a block or two below the head its headers
//! give (a node behind a load balancer) still runs the helper: the probe
//! checks the block the helper says it ran at against the endpoint's own
//! header for that block, and does not take a number below the head as
//! overrides refused. Its observations say the block they ran at.

mod stand_in_node;

use chain::follow::{At, Path, Plan};
use stand_in_node::{Anvil, Proxy, policy};

#[tokio::test]
async fn an_endpoint_whose_calls_run_behind_its_head_is_still_shown_to_run_the_helper() {
    let anvil = Anvil::start(31_480_006, 1_700_000_000).await;
    anvil.mine(300, 1).await;
    for behind in [1u64, 2] {
        let proxy = Proxy::start(&anvil.url).await;
        proxy.script(|script| script.calls_behind = behind);
        let mut follower = stand_in_node::follower(&[&proxy.url], policy()).await;
        let observed = follower
            .observe(&Plan::default(), At::Latest)
            .await
            .unwrap();
        let known = follower.capabilities(0).unwrap();
        assert_eq!(known.overrides, Some(true), "{behind} behind");
        assert_eq!(observed.path, Path::ViewReads, "{behind} behind");
        let head = anvil.head().await;
        assert_eq!(observed.number, head - behind);
        assert_eq!(observed.parent_hash, anvil.block(head - behind).await.1);
    }
}
