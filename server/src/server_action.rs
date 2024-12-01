use tokio::sync::mpsc::{self};

pub enum ServerAction {
    AddSession {
        id: u64,
        session_channel: mpsc::UnboundedSender<String>,
    },
    DropSession {
        name: String,
        id: u64,
    },
    CreateRoom {
        room: String,
    },
    PrivMsg {
        user_id: u64,
        message: String,
    },
}
