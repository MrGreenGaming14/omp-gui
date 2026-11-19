//! The feature areas. Each directory is owned by one area; the app holds one
//! field and one `Message` variant per area.

use iced::Task;

use crate::area::Ctx;

pub mod decisions;
pub mod harness;
pub mod models;
pub mod peers;
pub mod projects;
pub mod sandbox;
pub mod sessions;
pub mod surfaces;
pub mod tools;
pub mod updater;

/// One state per area.
pub struct Areas {
    pub harness: harness::State,
    pub sessions: sessions::State,
    pub decisions: decisions::State,
    pub tools: tools::State,
    pub surfaces: surfaces::State,
    pub models: models::State,
    pub projects: projects::State,
    pub peers: peers::State,
    pub updater: updater::State,
    pub sandbox: sandbox::State,
}

impl Areas {
    pub fn new(ctx: &mut Ctx) -> (Self, Task<crate::app::Message>) {
        use crate::app::Message;
        let (harness, harness_task) = harness::State::new(ctx);
        let (sessions, sessions_task) = sessions::State::new(ctx);
        let (decisions, decisions_task) = decisions::State::new(ctx);
        let (tools, tools_task) = tools::State::new(ctx);
        let (surfaces, surfaces_task) = surfaces::State::new(ctx);
        let (models, models_task) = models::State::new(ctx);
        let (projects, projects_task) = projects::State::new(ctx);
        let (peers, peers_task) = peers::State::new(ctx);
        let (updater, updater_task) = updater::State::new(ctx);
        let (sandbox, sandbox_task) = sandbox::State::new(ctx);
        let task = Task::batch([
            harness_task.map(Message::Harness),
            sessions_task.map(Message::Sessions),
            decisions_task.map(Message::Decisions),
            tools_task.map(Message::Tools),
            surfaces_task.map(Message::Surfaces),
            models_task.map(Message::Models),
            projects_task.map(Message::Projects),
            peers_task.map(Message::Peers),
            updater_task.map(Message::Updater),
            sandbox_task.map(Message::Sandbox),
        ]);
        let areas = Self { harness, sessions, decisions, tools, surfaces, models, projects, peers, updater, sandbox };
        (areas, task)
    }
}

/// Calls `$m!` with the arguments given and then every area as
/// `field Variant` (the `Areas` field and the `AreaId` / app `Message`
/// variant), in call order.
macro_rules! with_areas {
    ($m:ident!($($args:tt)*)) => {
        $m!($($args)*; harness Harness, sessions Sessions, decisions Decisions, tools Tools, surfaces Surfaces,
            models Models, projects Projects, peers Peers, updater Updater, sandbox Sandbox)
    };
}

pub(crate) use with_areas;
