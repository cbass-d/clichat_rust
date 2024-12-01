use std::{
    collections::HashMap,
    io::ErrorKind,
    sync::{atomic, Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::AsyncWriteExt,
    net::{
        tcp::{OwnedReadHalf, OwnedWriteHalf},
        TcpListener, TcpStream,
    },
    signal::ctrl_c,
    sync::{
        broadcast::{self, error::TryRecvError},
        mpsc::{self},
    },
    task::JoinSet,
};

use chat_session::ChatSession;
use room::{room_manager::RoomManager, Room};
use server_updates::ServerUpdate;

mod chat_session;
mod room;
mod server_updates;
use common;

struct RequestHandler {
    reader: OwnedReadHalf,
}

struct ResponseWriter {
    writer: OwnedWriteHalf,
}

type ClientConnection = (RequestHandler, ResponseWriter);
const SERVER_ADDR: &str = "127.0.0.1";

#[derive(Clone)]
enum Terminate {
    ClientClosed,
    SocketError,
    ServerClose,
}

fn split_stream(stream: TcpStream) -> ClientConnection {
    let (reader, writer) = stream.into_split();
    (RequestHandler { reader }, ResponseWriter { writer })
}

async fn startup_server() -> Result<TcpListener, std::io::Error> {
    let listener = TcpListener::bind("127.0.0.1:6667").await?;
    return Ok(listener);
}

async fn handle_client(
    stream: TcpStream,
    id: u64,
    mut shutdown_rx: broadcast::Receiver<Terminate>,
    server_updates_tx: mpsc::UnboundedSender<ServerUpdate>,
    mut manager_updates_rx: broadcast::Receiver<Arc<RoomManager>>,
    room_manager: Arc<RoomManager>,
    server_users: Arc<Mutex<HashMap<String, u64>>>,
) -> Terminate {
    let client_connection = split_stream(stream);
    let (req_handler, mut res_writer) = client_connection;
    let mut buf: Vec<u8> = Vec::with_capacity(4096);
    let mut chat_session = ChatSession::new(id, room_manager);

    loop {
        match shutdown_rx.try_recv() {
            Ok(termination) => {
                break termination;
            }
            Err(TryRecvError::Closed) => {
                break Terminate::ServerClose;
            }
            Err(_) => {}
        }

        tokio::select! {
            new_manager = manager_updates_rx.recv() => {
                match new_manager {
                    Ok(new_manager) => {
                        chat_session.update_manager(new_manager);
                    }
                    _ => {
                        println!("[-] Failed to update room manager");
                    }
                }
            },
            _ = req_handler.reader.readable() => {
                match req_handler.reader.try_read_buf(&mut buf) {
                    Ok(len) if len > 0 => {
                        let raw_message = String::from_utf8(buf[0..len].to_vec()).unwrap();
                        if let Some((cmd, arg, sender, id, message)) = common::unpack_message(&raw_message) {
                            match cmd {
                                "register" => {
                                    match arg {
                                        Some(name) => {
                                            let mut response = String::new();
                                            {
                                                let mut server_users = server_users.lock().unwrap();
                                                if server_users.contains_key(name) {
                                                    response = common::pack_message("registered", Some("taken"), "server", 0, Some("0"));
                                                } else {
                                                    server_users.insert(name.to_string(), chat_session.get_id());
                                                    response = common::pack_message("registered", Some("success"), "server", 0, Some(&chat_session.get_id().to_string()));
                                                }
                                            }
                                            let _ = res_writer.writer.write_all(response.as_bytes()).await;
                                        },
                                        None => {
                                            let message = "[-] Invalid or no argument received";
                                            let message = common::pack_message("registered", Some("failed"), "server", 0, Some(&message));
                                            let _ = res_writer.writer.write_all(message.as_bytes()).await;
                                        },
                                    }
                                }
                                "name" => {
                                    let mut response: String;
                                    if arg == None {
                                        response = "[-] Invalid request recieved".to_string();
                                        response = common::pack_message("name", None, "server", 0, Some(&response));
                                    } else {
                                        let mut server_users = server_users.lock().unwrap();
                                        let new_name = arg.unwrap();
                                        if new_name != "anon" && server_users.contains_key(new_name) {
                                            response = format!("[-] [{new_name}] as username is already in use");
                                            response = common::pack_message("changedname", Some("failed"), "server", 0, Some(&response));
                                        } else {
                                            let old_name = sender;
                                            let _ = server_users.remove(old_name);
                                            let new_name = new_name.to_string();
                                            server_users.insert(new_name.clone(), chat_session.get_id());
                                            chat_session.set_name(new_name.clone());
                                            response = format!("[+] Username changed to [{new_name}]");
                                            response = common::pack_message("changedname", Some(&new_name), "server", 0, Some(&response));
                                        }
                                    }
                                    let _ = res_writer.writer.write_all(response.as_bytes()).await;
                                }
                                "join" => {
                                    match arg {
                                        Some(arg) => {
                                            let result = chat_session.join_room(arg.to_string()).await;
                                            let message = common::pack_message("joined", Some(arg), "server", 0, Some(&result));
                                            let _ = res_writer.writer.write_all(message.as_bytes()).await;
                                        },
                                        None => {}
                                    }
                                },
                                "list" => {
                                    match arg {
                                        Some("rooms") => {
                                            let rooms: Vec<String> = chat_session.rooms.clone().into_keys().collect();
                                            let content = rooms.join(",");
                                            let message = common::pack_message("rooms", None, "server", 0, Some(&content));
                                            let _ = res_writer.writer.write_all(message.as_bytes()).await;
                                        },
                                        Some("users") => {
                                            let mut response = String::new();
                                            {
                                                let server_users = server_users.lock().unwrap();
                                                let mut users_list: Vec<String> = Vec::new();
                                                for (name, id) in server_users.clone().into_iter() {
                                                    let value = format!("{0} {1}", name, id);
                                                    users_list.push(value);
                                                }
                                                response = users_list.join(",");
                                            }
                                            let response = common::pack_message("users", None, "server", 0, Some(&response));
                                            let _ = res_writer.writer.write_all(response.as_bytes()).await;
                                        },
                                        Some("allrooms") => {
                                            let server_rooms = chat_session.get_server_rooms();
                                            let response = Vec::from_iter(server_rooms.into_iter()).into_iter()
                                                .collect::<Vec<String>>().join(",");
                                            let response = common::pack_message("rooms", None, "server", 0, Some(&response));
                                            let _ = res_writer.writer.write_all(response.as_bytes()).await;
                                        },
                                        _ => {}
                                    }
                                },
                                "create" => {
                                    match arg {
                                        Some(new_room) => {
                                            let mut response = String::new();
                                            {
                                                let server_rooms = chat_session.get_server_rooms();
                                                if server_rooms.contains(new_room) {
                                                    response = common::pack_message("created", Some("failed"), "server", 0, None);
                                                }
                                                else {
                                                    let _ = server_updates_tx.send(ServerUpdate::CreateRoom {room: new_room.to_string()});
                                                    response = common::pack_message("created", Some("succes"), "server", 0, None);
                                                }
                                            }

                                            let _ = res_writer.writer.write_all(response.as_bytes()).await;
                                        },
                                        None => {},
                                    }
                                },
                                "sendto" => {
                                    match arg {
                                        Some(room) => {
                                            if let Some(user_handle) = chat_session.rooms.get(room) {
                                                match message {
                                                    Some(message) => {
                                                        let server_users = server_users.lock().unwrap();
                                                        let id = *server_users.get(sender).unwrap();
                                                        let message = common::pack_message("roommessage", Some(room), sender, id, Some(message));
                                                        user_handle.send_message(message.to_string());
                                                    },
                                                    None => {
                                                        println!("[-] No message provided");
                                                    }
                                                }
                                            }
                                            else {println!("[-] Room not found: {room}");}
                                        },
                                        None => {
                                            println!("[-] Invalid argument provided");
                                        },
                                    }
                                },
                                _ => {
                                    println!("[-] Invalid command received");
                                },
                            }
                        }
                        else { println!("[-] Invalid raw message received"); }
                    },
                    Ok(_) => {
                        println!("[-] Stream closed by client");
                        break Terminate::ClientClosed;
                    },
                    Err(ref e) if e.kind() == ErrorKind::WouldBlock => {},
                    Err(e) => {
                        let err = e.to_string();
                        println!("[-] Failed to read from stream: {err}");
                        break Terminate::SocketError;
                    },
                }
            }
            message = chat_session.recv() => {
                if let Some(message) = message {
                    let _ = res_writer.writer.write_all(message.as_bytes()).await;
                }
            },
            terminate = shutdown_rx.recv() => {
                break terminate.unwrap();
            }
        }

        buf.clear();
    }
}

#[tokio::main]
async fn main() {
    println!("[+] Starting listener...");
    let listener = match startup_server().await {
        Ok(listener) => listener,
        Err(e) => {
            eprintln!("[-] Failed to start server: {e}");
            panic!();
        }
    };
    let mut connections_set: JoinSet<Terminate> = JoinSet::new();
    let (shutdown_tx, shutdown_rx) = broadcast::channel::<Terminate>(10);

    // Default server rooms
    let main_room = Room::new("main");
    let mut server_rooms: Vec<Arc<Mutex<Room>>> = vec![Arc::new(Mutex::new(main_room))];
    let mut room_manager = Arc::new(RoomManager::new(server_rooms.clone()));

    // Channel for performing server wide actions
    let (server_updates_tx, mut server_updates_rx) = mpsc::unbounded_channel::<ServerUpdate>();

    // Broadcast channel for handling updates of RoomManager
    let (manager_updates_tx, manager_updates_rx) = broadcast::channel::<Arc<RoomManager>>(10);

    // Users
    let server_users: Arc<Mutex<HashMap<String, u64>>> = Arc::new(Mutex::new(HashMap::new()));

    // Incremental user id
    // Id '0' will be reserved for respsones orginating from server
    let mut user_id: atomic::AtomicU64 = atomic::AtomicU64::new(1);

    println!("[+] Server started...\n[+] Listening for connections...");

    let mut ticker = tokio::time::interval(Duration::from_millis(250));
    loop {
        tokio::select! {
            _ = ticker.tick() => {},
            Ok(_) = ctrl_c() => {
                println!("[-] Shutting down server");
                let _ = shutdown_tx.send(Terminate::ServerClose);
                break;
            },
            Ok((stream, _)) = listener.accept() => {
                let id = user_id.get_mut().clone();
                connections_set.spawn(handle_client(
                    stream,
                    id.into(),
                    shutdown_rx.resubscribe(),
                    server_updates_tx.clone(),
                    manager_updates_rx.resubscribe(),
                    Arc::clone(&room_manager),
                    Arc::clone(&server_users))
                );
                user_id = (*user_id.get_mut() + 1).into();
            },
            server_update = server_updates_rx.recv() => {
                match server_update {
                    Some(ServerUpdate::CreateRoom { room }) => {
                        let new_room = Room::new(&room);
                        server_rooms.push(Arc::new(Mutex::new(new_room)));
                        let new_manager = Arc::new(RoomManager::new(server_rooms.clone()));
                        room_manager = new_manager;
                        let _ = manager_updates_tx.send(Arc::clone(&room_manager));
                    },
                    _ => {},
                }

            },
        }
    }
    connections_set.join_all().await;
}
