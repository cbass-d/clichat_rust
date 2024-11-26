use std::time::Duration;
use tokio::{
    io::*,
    net::{
        tcp::{OwnedReadHalf, OwnedWriteHalf},
        TcpStream,
    },
    sync::broadcast::{Receiver, Sender},
};

use common;
use state_handler::{
    action::Action,
    state::{ConnectionStatus, State},
    StateHandler,
};
use tui::{
    app_router::AppRouter,
    components::component::{Component, ComponentRender},
    Event, Tui,
};

mod state_handler;
mod tui;

struct ResponseStream {
    reader: OwnedReadHalf,
}

struct RequestHandler {
    writer: OwnedWriteHalf,
}

#[derive(Clone)]
enum Terminate {
    StateExit,
    Error,
}

type ConnectionHandler = (ResponseStream, RequestHandler);

fn split_stream(stream: TcpStream) -> ConnectionHandler {
    let (reader, writer) = stream.into_split();

    (ResponseStream { reader }, RequestHandler { writer })
}

async fn establish_connection(server: &str) -> Result<TcpStream> {
    let stream = TcpStream::connect(server).await?;
    return Ok(stream);
}

fn terminate_connection(state: &mut State) -> (State, Option<ConnectionHandler>) {
    state.set_connection_status(ConnectionStatus::Unitiliazed);

    (state.clone(), None)
}

async fn run(shutdown_tx: Sender<Terminate>, shutdown_rx: &mut Receiver<Terminate>) -> Result<()> {
    let (state_handler, mut state_rx) = StateHandler::new();
    let (tui, mut action_rx, mut tui_events) = Tui::new();
    let mut shutdown_rx_state = shutdown_rx.resubscribe();
    let mut shutdown_rx_tui = shutdown_rx.resubscribe();

    // State Handler
    let state_task = tokio::spawn(async move {
        // Create and send initial state to TUI
        let mut state = State::default();
        state_handler.state_tx.send(state.clone()).unwrap();

        let mut ticker = tokio::time::interval(Duration::from_millis(250));
        let mut connection_handle: Option<ConnectionHandler> = None;
        let mut origin = String::new();
        let mut buf = Vec::with_capacity(4096);
        let mut update: bool = false;

        loop {
            if state.exit {
                let _ = shutdown_tx.send(Terminate::StateExit);
            }

            if let Some((res_stream, req_handler)) = connection_handle.as_mut() {
                tokio::select! {
                    _tick = ticker.tick() => {},
                    _ = res_stream.reader.readable() => {
                        match res_stream.reader.try_read_buf(&mut buf) {
                            Ok(len) if len > 0 => {
                                let message = String::from_utf8(buf[0..len].to_vec()).unwrap();
                                state.push_notification(message);
                                update = true;
                            },
                            Ok(_) => {
                                state.push_notification("[-] Connection closed by server".to_string());
                                (state, connection_handle) = terminate_connection(&mut state);
                                update = true;
                            },
                            Err(ref e) if e.kind() == ErrorKind::WouldBlock => {},
                            Err(e) => {
                                let err = e.to_string();
                                state.push_notification("[-] Failed to read from stream: ".to_string() + &err);
                                state.push_notification("[-] Closing connection to server".to_string());
                                (state, connection_handle) = terminate_connection(&mut state);
                                update = true;
                            }
                        }
                    }
                    action = action_rx.recv() => {
                        match action.unwrap() {
                            Action::SetName { name } => {
                                state.set_name(name.clone());
                                state.push_notification(format!("[+] Name set to [{name}]"));
                                update = true;
                            },
                            Action::SendTo { arg, message } => {
                                let message = common::pack_message(&origin, "sendto", Some(&arg), &state.get_name(), Some(&message));
                                let len = req_handler.writer.try_write(message.as_bytes()).unwrap();
                                let len = len.to_string();
                                state.push_notification("[+] Bytes written to server:" .to_string() + &len);
                                update = true;
                            },
                            Action::Join { room } => {
                                let message = common::pack_message(&origin, "join", Some(&room), &state.get_name(), None);
                                let len = req_handler.writer.try_write(message.as_bytes()).unwrap();
                                let len = len.to_string();
                                state.push_notification("[+] Bytes written to server:" .to_string() + &len);
                                update = true;
                            },
                            Action::List { opt } => {
                                let message = common::pack_message(&origin, "list", Some(&opt), &state.get_name(), None);
                                let len = req_handler.writer.try_write(message.as_bytes()).unwrap();
                                let len = len.to_string();
                                state.push_notification("[+] Bytes written to server:" .to_string() + &len);
                                update = true;
                            }
                            Action::Disconnect => {
                                state.push_notification("[-] Closing connection to server".to_string());
                                (state, connection_handle) = terminate_connection(&mut state);
                                update = true;
                            },
                            Action::Quit => {
                                state.exit();
                                update = true;
                            },
                            Action::Invalid => {
                                state.push_notification("[-] Invalid command".to_string());
                                update = true;
                            },
                            _ => {}
                        }
                    },
                    _ = shutdown_rx_state.recv() => {
                            break;
                    }
                }
            } else {
                tokio::select! {
                    _tick = ticker.tick() => {},
                    action = action_rx.recv() => {
                        match action.unwrap() {
                            Action::SetName { name } => {
                                state.set_name(name.clone());
                                state.push_notification(format!("[+] Name set to [{name}]"));
                                update = true;
                            }
                            Action::SendTo {..} => {
                                state.push_notification("[-] Not connected to a server".to_string());
                                update = true;
                            }
                            Action::Join {..} => {
                                state.push_notification("[-] Not connected to a server".to_string());
                                update = true;
                            }
                            Action::List {..} => {
                                state.push_notification("[-] Not connected to a server".to_string());
                                update = true;
                            }
                            Action::Connect { addr } => {
                                match establish_connection(&addr).await {
                                    Ok(s) => {
                                        state.set_server(addr);
                                        state.set_connection_status(ConnectionStatus::Established);
                                        origin = s.local_addr().unwrap().to_string();
                                        let _ = connection_handle.insert(split_stream(s));
                                        state.push_notification("[+] Successfully connected".to_string());
                                    },
                                    Err(e) => {
                                        let err = e.to_string();
                                        state.push_notification("[-] Failed to connect: ".to_string() + &err);
                                    },
                                }
                                update = true;
                            },
                            Action::Disconnect => {
                                state.push_notification("[-] Not connected to a server".to_string());
                                update = true;
                            }
                            Action::Quit => {
                                state.exit();
                                update = true;
                            },
                            Action::Invalid => {
                                state.push_notification("[-] Invalid command".to_string());
                                update = true;
                            },
                            _ => {}
                        }
                    },
                    _ = shutdown_rx_state.recv() => {
                            break;
                    }
                }
            }

            if update {
                match state_handler.state_tx.send(state.clone()) {
                    Ok(_) => {}
                    Err(_) => {
                        let _ = shutdown_tx.send(Terminate::Error);
                    }
                }
                update = false;
            }
            buf.clear();
        }
    });

    // TUI Handler
    let tui_task = tokio::spawn(async move {
        let mut terminal = Tui::setup_terminal();
        // Get initial State
        let state = state_rx.recv().await.unwrap();
        let mut app_router = AppRouter::new(&state, tui.action_tx);
        let _ = terminal.draw(|f| app_router.render(f, ()));

        loop {
            tokio::select! {
                event = tui_events.next() => {
                    match event.unwrap() {
                        Event::Key(key) => {
                            app_router.handle_key_event(key);
                        },
                        Event::Tick => {},
                        Event::Error => {},
                    }
                }
                state = state_rx.recv() => {
                    match state {
                        Some(state) => {
                            let _ = terminal.clear();
                            app_router = app_router.update(&state);
                        }
                        None => {},
                    }
                }
                _ = shutdown_rx_tui.recv() => {
                    break;
                }
            }

            let _ = terminal.draw(|f| app_router.render(f, ()));
        }

        Tui::teardown_terminal(&mut terminal);
    });

    let (_, _) = tokio::join!(tui_task, state_task);

    Ok(())
}

fn shutdown() {
    println!("shutting down client");
}

#[tokio::main]
async fn main() -> Result<()> {
    let (shutdown_tx, mut shutdown_rx) = tokio::sync::broadcast::channel::<Terminate>(2);
    let mut shutdown_rx_main = shutdown_rx.resubscribe();
    tokio::spawn(async move {
        let _ = run(shutdown_tx, &mut shutdown_rx).await;
    })
    .await?;

    tokio::select! {
        _ = shutdown_rx_main.recv() => {
            shutdown();
        }
    }

    Ok(())
}
