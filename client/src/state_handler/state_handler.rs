#[allow(unused_imports)]
use ratatui::{
    backend::{Backend, CrosstermBackend},
    crossterm::event::{self, Event, KeyCode, KeyEventKind},
    Terminal,
};

use crate::state_handler::state::State;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

pub struct StateHandler {
    pub state_tx: UnboundedSender<State>,
}

impl StateHandler {
    pub fn new() -> (Self, UnboundedReceiver<State>) {
        let (state_tx, state_rx) = mpsc::unbounded_channel::<State>();

        (Self { state_tx }, state_rx)
    }
}
