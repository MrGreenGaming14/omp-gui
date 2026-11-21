//! The feature areas.

use std::any::Any;

use iced::Task;
use loa_ui_session::composer;

use crate::app::Message;
use crate::area::{Area, AreaDyn, AreaMsg, Ctx, Typed};

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

/// Makes an area and its first task.
type Boot = fn(&mut Ctx) -> (Box<dyn AreaDyn>, Task<Message>);

fn boot<A: Area>(ctx: &mut Ctx) -> (Box<dyn AreaDyn>, Task<Message>) {
    let (area, task) = A::new(ctx);
    (Box::new(Typed(area)), task.map(Message::area))
}

/// Every area, in the order the app calls them.
pub const AREAS: &[Boot] = &[
    boot::<harness::State>,
    boot::<sessions::State>,
    boot::<decisions::State>,
    boot::<tools::State>,
    boot::<surfaces::State>,
    boot::<models::State>,
    boot::<projects::State>,
    boot::<peers::State>,
    boot::<updater::State>,
    boot::<sandbox::State>,
];

/// Every area's state, in [`AREAS`] order.
pub struct Registry(Vec<Box<dyn AreaDyn>>);

impl Registry {
    pub fn new(ctx: &mut Ctx) -> (Self, Task<Message>) {
        let (areas, tasks): (Vec<_>, Vec<_>) = AREAS.iter().map(|boot| boot(ctx)).unzip();
        (Self(areas), Task::batch(tasks))
    }

    pub fn iter(&self) -> impl Iterator<Item = &dyn AreaDyn> {
        self.0.iter().map(Box::as_ref)
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut Box<dyn AreaDyn>> {
        self.0.iter_mut()
    }

    /// The area with id `id`.
    pub fn find(&mut self, id: &str) -> Option<&mut Box<dyn AreaDyn>> {
        self.0.iter_mut().find(|area| area.id() == id)
    }

    /// The area `A`. Every area is in [`AREAS`], so it is always found.
    pub fn get<A: Area>(&self) -> &A {
        let area = self.0.iter().find(|area| area.id() == A::ID).expect("every area is registered");
        &(area.as_ref() as &dyn Any).downcast_ref::<Typed<A>>().expect("the area registered under its own id").0
    }

    pub fn get_mut<A: Area>(&mut self) -> &mut A {
        let area = self.0.iter_mut().find(|area| area.id() == A::ID).expect("every area is registered");
        &mut (area.as_mut() as &mut dyn Any).downcast_mut::<Typed<A>>().expect("the area registered under its own id").0
    }

    /// Hands `message` to area `id`.
    pub fn update(&mut self, id: &str, message: AreaMsg, ctx: &mut Ctx) -> Task<Message> {
        match self.find(id) {
            Some(area) => area.update(message, ctx),
            None => {
                ctx.notify(format!("No area {id} for {message:?}"));
                Task::none()
            }
        }
    }

    /// Every area's `/` commands, in order.
    pub fn slash_rows(&self) -> Vec<composer::Command> {
        self.iter().flat_map(|area| area.slash_rows()).collect()
    }
}
