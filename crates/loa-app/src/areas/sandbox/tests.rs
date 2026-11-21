use crate::app::Message;
use crate::testing::TestApp;

#[test]
fn sandbox_settings_open_in_the_panel_and_its_tools_reach_sessions() {
    let mut test = TestApp::new();
    test.command("sandbox.settings");
    assert!(test.app.ctx.shell.tabs().iter().any(|tab| tab.id == super::TAB));
    assert!(matches!(test.area::<super::State>().view, super::View::Settings));
    for (key, value) in [("ssh.hosts", "a, b c"), ("docker.image", " "), ("vm.disk", "/disk.img")] {
        test.send(Message::area(super::Message::Edit(key, value.into())));
    }
    let config = test.area::<super::State>().draft.config();
    assert_eq!((config.ssh_hosts, config.vm_disk), (vec!["a".to_owned(), "b".into(), "c".into()], Some("/disk.img".into())), "the form writes the config");
    assert_eq!(config.docker_image, loa_sandbox::SandboxConfig::default().docker_image, "an empty image is the default one");
    test.session("hello");
    test.pump_feeds();
    assert!(test.area::<super::State>().runs().entries().is_empty(), "no run yet");
}
