//! A surface that honestly says it is not implemented.
//!
//! Pre-wiring all seven means the registry is never edited again; a stub also
//! makes the router, keymap and layout testable before any surface exists.

use crate::{
    route::SurfaceId,
    surface::{Ctx, Outcome, Surface},
    widgets,
};
use async_trait::async_trait;
use crossterm::event::KeyEvent;
use omaghy_model::Result;
use ratatui::{Frame, layout::Rect};

#[derive(Debug)]
pub struct Stub {
    id: SurfaceId,
    milestone: &'static str,
}

pub fn boxed(id: SurfaceId, milestone: &'static str) -> Box<dyn Surface> {
    Box::new(Stub { id, milestone })
}

#[async_trait]
impl Surface for Stub {
    fn title(&self) -> String {
        self.id.title().to_owned()
    }

    fn render(&mut self, f: &mut Frame, area: Rect, _ctx: &Ctx) {
        widgets::empty_state(
            f,
            area,
            self.id.title(),
            &format!("Not implemented yet — arrives in {}.", self.milestone),
        );
    }

    fn on_key(&mut self, _key: KeyEvent, _ctx: &Ctx) -> Outcome {
        Outcome::Ignored
    }

    async fn load(&mut self, _ctx: &Ctx) -> Result<()> {
        Ok(())
    }
}
