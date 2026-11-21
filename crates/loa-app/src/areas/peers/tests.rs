use crate::testing::TwoNodes;

#[test]
fn two_nodes_open_their_endpoints_and_copy_keys_from_the_palette() {
    let mut nodes = TwoNodes::new();
    for test in [&mut nodes.owner, &mut nodes.guest] {
        test.until("the node runs", |app| app.areas.get::<super::State>().node.is_some());
        test.command("peers.copy_key");
        assert_eq!(test.notice(), "Key copied");
        test.pump_feeds();
    }
    assert_ne!(nodes.owner.app.ctx.services.node.key, nodes.guest.app.ctx.services.node.key);
}
