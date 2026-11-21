use crate::testing::TestApp;

#[test]
fn projects_open_and_pin_from_the_palette() {
    let mut test = TestApp::new();
    test.session("hello");
    let project = test.app.ctx.services.project.clone();
    let titles = test.palette_titles();
    let open = format!("Open project: {project}");
    let pin = format!("Pin project: {project}");
    let at = |title: &str| titles.iter().position(|each| each == title).unwrap_or_else(|| panic!("{title} listed: {titles:?}"));
    assert_eq!(at(&pin), at(&open) + 1, "each project's Open is followed by its Pin");
    test.command(&format!("projects.open:{project}"));
    assert_eq!(test.app.ctx.screen(), Some("projects"));
    assert_eq!(test.area::<super::State>().shown(), Some(project.as_str()));
    test.command(&format!("projects.pin:{project}"));
    assert!(test.app.ctx.shell.sidebar().pinned_projects().contains(&project));
    test.command("projects.bogus");
    assert_eq!(test.notice(), "Unknown command projects.bogus");
}
