use std::net::{TcpListener, TcpStream};
use std::io::{Read, Write};
use std::str;

use serde::Serialize;

use threads::ThreadPool;
use database::{load_db, Event, EVENT_LIST, Hash, UserEntry, EventEntry};

mod threads;
mod macros;
mod database;

const ADDR: &str = "127.0.0.1:8080";
const ROOT_: &str = "index.html";
const HTTP: &str = "HTTP/1.1";

fn main() {
    let listener = TcpListener::bind(ADDR).unwrap();
    let pool = ThreadPool::new(4);

    unsafe{ load_db(); }

    for stream in listener.incoming() {
        pool.execute(|| handle_conn(stream.unwrap()));
    }
}

fn handle_conn(mut stream: TcpStream) {
    let mut buf = [0; 1024];
    let bytes_ = match stream.read(&mut buf) {
        Ok(b) if b == 0 => return,
        Ok(b) => b,
        Err(e) => {
            eprintln!("Failed to read from stream: {}", e);
            return;
        },
    };

    let req = str::from_utf8(&buf[..bytes_]).unwrap();
    let (head, body) = req.split_once("\r\n\r\n").unwrap_or((req, ""));

    eprintln!("\nHead: \n{head}\n\nBody: \n{body}");

    let type_ = head.lines().next().unwrap().split_whitespace().collect::<Vec<&str>>();
    // example: GET / HTTP/1.1_
    // [0] = GET
    // [1] = /
    // [2] = HTTP/1.1

    match type_[0] {
        "GET" => handle_get(type_[1], &mut stream),
        "POST" => handle_post(type_[1], body, &mut stream),
        _ => stream.write_all(status!("400 BAD REQUEST")).unwrap(),
    }

    stream.flush().unwrap();
}

fn handle_get(arg: &str, stream: &mut TcpStream) {
    // FIXME: for now ignore favicon requests
    if arg == "/favicon.ico" { return; }
    
    let Some(arg) = arg.strip_prefix("/api/") else {
        stream.write_all(file!(std::fs::read_to_string(ROOT_).unwrap())).unwrap();
        return;
    };
    
    // parse arg into event id
    if arg.len() != 6 {
        stream.write_all(status!("400 BAD REQUEST")).unwrap();
        return;
    }

    let db = unsafe { EVENT_LIST.as_ref().unwrap().lock().unwrap() };
    let Some(ref event_id) = str_to_hash(arg) else {
        stream.write_all(status!("400 BAD REQUEST")).unwrap();
        return;
    };
    let Some(event) = db.get(event_id) else {
        stream.write_all(status!("404 NOT FOUND")).unwrap();
        return;
    };

    let json_event = serde_json::to_string(event).unwrap();
    dbg!(&json_event);
    
    stream.write_all(json!(json_event)).unwrap();
}

#[derive(Serialize)]
struct Hashes {
    event_id: String,
    edit_hash: String,
}

struct Boiled {
    pass: [char; 8],
    name: String,
}

fn handle_post(arg: &str, body: &str, stream: &mut TcpStream) {
    let Some(arg) = arg.strip_prefix("/api/") else {
        stream.write_all(status!("404 NOT FOUND")).unwrap();
        return;
    };

    //
    // create a new event
    if arg == "new" {
        if body.is_empty() {
            stream.write_all(status!("400 BAD REQUEST")).unwrap();
            return;
        }

        let Ok(event) = serde_json::from_str::<Event>(body) else {
            stream.write_all(status!("400 BAD REQUEST")).unwrap();
            return;
        };

        if event.name.len() > 32 {
            stream.write_all(status!("400 BAD REQUEST")).unwrap();
            return;
        }

        let hashes = Event::add(event);

        let responce = serde_json::to_string(&Hashes {
            event_id: hash_to_str(hashes.0),
            edit_hash: hash_to_str(hashes.1),
        }).unwrap();

        stream.write_all(json!(responce)).unwrap();
        return;
    }

    if arg.ends_with("/usr") && !arg.starts_with("//usr"){

        let arg_ = match arg.ends_with("?e") {
            true => &arg[1..arg.len()-2],
            false => &arg[1..],
        };

        let Some((event_id, _)) = arg_.split_once('/') else {
            stream.write_all(status!("400 BAD REQUEST")).unwrap();
            return;
        };

        let Some(event_id) = str_to_hash(event_id) else {
            stream.write_all(status!("400 BAD REQUEST")).unwrap();
            return;
        };

        if arg.ends_with("?d") {
            let Ok(user) = serde_json::from_str::<Boiled>(body) else {
                stream.write_all(status!("400 BAD REQUEST")).unwrap();
                return;
            };

            if let Err(e) = Event::delete_user_en(event_id, &user.name, user.pass) {
                stream.write_all(status!(e)).unwrap();
                return;
            }

            return;
        }

        let Ok(user) = serde_json::from_str::<UserEntry>(body) else {
            stream.write_all(status!("400 BAD REQUEST")).unwrap();
            return;
        };

        if user.name.len() > 32 {
            stream.write_all(status!("400 BAD REQUEST")).unwrap();
            return;
        }

        if arg.ends_with("?e") {
            if let Err(e) =  Event::edit_user(event_id, user) {
                stream.write_all(status!(e)).unwrap();
                return;
            }
        }

        else if Event::add_user(event_id, user).is_err() {
            stream.write_all(status!("404 NOT FOUND")).unwrap();
            return;
        }

        stream.write_all(status!("200 OK")).unwrap();
        return;
    }

    //
    // edit an event
    let Some((event_id, edit_hash)) = arg[1..].split_once('?') else {
        stream.write_all(status!("400 BAD REQUEST")).unwrap();
        return;
    };

    let Some(event_id) = str_to_hash(event_id) else {
        stream.write_all(status!("400 BAD REQUEST")).unwrap();
        return;
    };

    let Some(edit_hash) = str_to_hash(edit_hash) else {
        stream.write_all(status!("400 BAD REQUEST")).unwrap();
        return;
    };

    if !validate_key(event_id, edit_hash, stream) { return; }

    if body.is_empty() {
        let db = unsafe { EVENT_LIST.as_ref().unwrap().lock().unwrap() };
        match db.get(&event_id) {
            Some(e) => {
                let Ok(json_event) = serde_json::to_string(e) else {
                    stream.write_all(status!("500 INTERNAL SERVER ERROR")).unwrap();
                    return;
                };
                stream.write_all(json!(json_event)).unwrap();
                return;
            },
            None => {
                stream.write_all(status!("404 NOT FOUND")).unwrap();
                return;
            }
        };
    }

    let Ok(new_event) = serde_json::from_str::<EventEntry>(body) else {
        stream.write_all(status!("400 BAD REQUEST")).unwrap();
        return;
    };
    
    if new_event.name.len() > 32  {
        stream.write_all(status!("400 BAD REQUEST")).unwrap();
        return;
    }

    if new_event.desc.as_ref().map(|d| d.len() > 256).unwrap_or(false) {
        stream.write_all(status!("400 BAD REQUEST")).unwrap();
        return;
    }

    if let Some(ref del_usr) = new_event.deleted_users {
        for user in del_usr {
            if let Err(e) = Event::delete_user(event_id, user) {
                stream.write_all(status!(e)).unwrap();
                return;
            }
        }
    }

    Event::edit(event_id, edit_hash, new_event);

    stream.write_all(status!("200 OK")).unwrap();
}

fn validate_key(event_id: Hash, edit_hash: Hash, stream: &mut TcpStream) -> bool {
    let db = unsafe { EVENT_LIST.as_ref().unwrap().lock().unwrap() };

    match db.get(&event_id) {
        Some(e) if e.edit_hash != edit_hash => {
            stream.write_all(status!("403 FORBIDDEN")).unwrap();
            false
        },
        None => {
            stream.write_all(status!("404 NOT FOUND")).unwrap();
            false
        },
        _ => true,
    }
}

fn str_to_hash(s: &str) -> Option<Hash> {
    if s.len() != 6 { return None; }

    let mut hash: Hash = ['\0'; 6];
    let mut s = s.chars();
    (0..6).for_each(|i| hash[i] = s.next().unwrap());
    Some(hash)
}

fn hash_to_str(hash: Hash) -> String {
    hash.iter().collect::<String>()
}
