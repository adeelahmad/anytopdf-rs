//! A scripted IMAP server on loopback, shared by the watcher and CLI tests.
//! It speaks just enough IMAP4rev1 for LOGIN, SELECT, UID SEARCH/FETCH/STORE.
#![allow(dead_code)]
use std::{
    collections::BTreeMap,
    io::{BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    sync::{Arc, Mutex},
    thread,
};

pub const USER: &str = "user";
pub const PASSWORD: &str = "pass word";

#[derive(Default)]
pub struct Server {
    pub messages: BTreeMap<u32, Vec<u8>>,
    pub commands: Vec<String>,
}

pub type Shared = Arc<Mutex<Server>>;

pub trait Stream: Read + Write + Send {}
impl<T: Read + Write + Send> Stream for T {}

/// Wraps an accepted socket in TLS (supplied by tests that need it).
pub type Upgrade = Arc<dyn Fn(TcpStream) -> Box<dyn Stream> + Send + Sync>;

pub enum Security {
    Plain,
    Implicit(Upgrade),
    StartTls(Upgrade),
}

pub fn serve(shared: Shared) -> u16 {
    serve_with(shared, Security::Plain)
}

pub fn serve_with(shared: Shared, security: Security) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let security = Arc::new(security);
    thread::spawn(move || {
        for stream in listener.incoming() {
            let shared = shared.clone();
            let security = security.clone();
            thread::spawn(move || connection(stream.unwrap(), &shared, &security));
        }
    });
    port
}

fn connection(mut tcp: TcpStream, shared: &Shared, security: &Security) {
    match security {
        Security::Plain => session(Box::new(tcp), shared, true),
        Security::Implicit(upgrade) => session(upgrade(tcp), shared, true),
        Security::StartTls(upgrade) => {
            tcp.write_all(b"* OK [CAPABILITY IMAP4rev1 STARTTLS] fake ready\r\n")
                .unwrap();
            let mut line = Vec::new();
            let mut byte = [0u8; 1];
            while !line.ends_with(b"\n") {
                if tcp.read(&mut byte).unwrap_or(0) == 0 {
                    return;
                }
                line.push(byte[0]);
            }
            let line = String::from_utf8(line).unwrap();
            let (tag, command) = line.trim_end().split_once(' ').unwrap();
            shared.lock().unwrap().commands.push(command.to_string());
            assert_eq!(command, "STARTTLS");
            tcp.write_all(format!("{tag} OK begin TLS\r\n").as_bytes())
                .unwrap();
            session(upgrade(tcp), shared, false);
        }
    }
}

fn session(stream: Box<dyn Stream>, shared: &Shared, greet: bool) {
    let mut reader = BufReader::new(stream);
    if greet {
        reader
            .get_mut()
            .write_all(b"* OK [CAPABILITY IMAP4rev1] fake ready\r\n")
            .unwrap();
        reader.get_mut().flush().unwrap();
    }
    let mut line = String::new();
    while {
        line.clear();
        reader.read_line(&mut line).unwrap_or(0) > 0
    } {
        let (tag, command) = line.trim_end().split_once(' ').unwrap();
        let mut server = shared.lock().unwrap();
        server.commands.push(command.to_string());
        let upper = command.to_ascii_uppercase();
        let mut reply = Vec::new();
        if upper.starts_with("STARTTLS") {
            reply.extend(format!("{tag} BAD not here\r\n").bytes());
        } else if upper.starts_with("LOGIN") {
            if command == format!("LOGIN \"{USER}\" \"{PASSWORD}\"") {
                reply.extend(format!("{tag} OK logged in\r\n").bytes());
            } else {
                reply.extend(format!("{tag} NO bad credentials\r\n").bytes());
            }
        } else if upper.starts_with("CAPABILITY") {
            reply.extend(format!("* CAPABILITY IMAP4rev1 UIDPLUS\r\n{tag} OK\r\n").bytes());
        } else if upper.starts_with("SELECT") {
            let next = server.messages.keys().max().map_or(1, |m| m + 1);
            reply.extend(
                format!(
                    "* {} EXISTS\r\n* OK [UIDVALIDITY 42] ok\r\n* OK [UIDNEXT {next}] ok\r\n{tag} OK [READ-WRITE] selected\r\n",
                    server.messages.len()
                )
                .bytes(),
            );
        } else if let Some(rest) = upper.strip_prefix("UID SEARCH UID ") {
            let start: u32 = rest.split(':').next().unwrap().parse().unwrap();
            // Real servers match the highest UID for `n:*` even when it is below n.
            let mut hits: Vec<u32> = server
                .messages
                .keys()
                .copied()
                .filter(|u| *u >= start)
                .collect();
            if hits.is_empty()
                && let Some(max) = server.messages.keys().max()
            {
                hits.push(*max);
            }
            let list: Vec<String> = hits.iter().map(u32::to_string).collect();
            reply.extend(format!("* SEARCH {}\r\n{tag} OK\r\n", list.join(" ")).bytes());
        } else if let Some(rest) = upper.strip_prefix("UID FETCH ") {
            let (uid, item) = rest.split_once(' ').unwrap();
            let uid: u32 = uid.parse().unwrap();
            if let Some(body) = server.messages.get(&uid) {
                if item == "RFC822.SIZE" {
                    reply.extend(
                        format!("* 1 FETCH (UID {uid} RFC822.SIZE {})\r\n", body.len()).bytes(),
                    );
                } else {
                    reply.extend(
                        format!("* 1 FETCH (UID {uid} BODY[] {{{}}}\r\n", body.len()).bytes(),
                    );
                    reply.extend(body);
                    reply.extend(b")\r\n");
                }
            }
            reply.extend(format!("{tag} OK\r\n").bytes());
        } else if upper.starts_with("UID STORE") {
            reply.extend(format!("{tag} OK\r\n").bytes());
        } else if upper.starts_with("LOGOUT") {
            reply.extend(format!("* BYE\r\n{tag} OK\r\n").bytes());
            let _ = reader.get_mut().write_all(&reply);
            let _ = reader.get_mut().flush();
            return;
        } else {
            reply.extend(format!("{tag} BAD unknown\r\n").bytes());
        }
        let out = reader.get_mut();
        if out.write_all(&reply).and_then(|()| out.flush()).is_err() {
            return;
        }
    }
}
