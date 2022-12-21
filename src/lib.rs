#![deny(warnings)]
use {
    anyhow::{anyhow, Context, Error, Result},
    http::{response::Builder, HeaderMap, Method, StatusCode},
    spin_sdk::{
        http::{Request, Response},
        http_component, outbound_http, redis,
    },
    spin_webrtc_protocol::{ClientMessage, ServerMessage},
    std::{env, fs, str},
};

const REDIS_URL: &str = env!("REDIS_URL");

fn get_header_url<'a>(headers: &'a HeaderMap, name: &str) -> Result<&'a str> {
    headers
        .get(name)
        .with_context(|| anyhow!(r#"missing required header: "{name}""#))?
        .to_str()
        .with_context(|| anyhow!(r#"unable to parse "{name}" header as UTF-8"#))
}

fn add(url: &str, room: &str) -> Result<()> {
    println!("add {url} to {room}\n");

    // TODO: check if specified URL is already in a room and either remove it or error out

    if !room.is_empty() {
        send_to(url, &ClientMessage::You { url })?;

        redis::sadd(REDIS_URL, &format!("room:{room}"), &[url]).map_err(redis_error)?;

        redis::set(REDIS_URL, &format!("url:{url}"), room.as_bytes()).map_err(redis_error)?;

        send_to_all(url, room, &ClientMessage::Add { url })?;
    }

    Ok(())
}

fn remove(url: &str) -> Result<()> {
    let room = redis::get(REDIS_URL, &format!("url:{url}")).map_err(redis_error)?;
    let room = str::from_utf8(&room)?;

    if !room.is_empty() {
        println!("remove {url} from {room}\n");

        redis::del(REDIS_URL, &[&format!("url:{url}")]).map_err(redis_error)?;

        redis::srem(REDIS_URL, &format!("room:{room}"), &[url]).map_err(redis_error)?;

        send_to_all(url, room, &ClientMessage::Remove { url })?;
    }

    Ok(())
}

fn send_to(url: &str, outbound: &ClientMessage) -> Result<()> {
    println!("send to {url}: {outbound:?}\n");

    let response = outbound_http::send_request(
        http::Request::builder()
            .method("POST")
            .uri(url)
            .header("content-type", "text/plain;charset=UTF-8")
            .body(Some(serde_json::to_string(outbound)?.into()))?,
    )?;

    if response.status() == StatusCode::NOT_FOUND {
        remove(url)?;
    }

    Ok(())
}

fn send_to_all(url: &str, room: &str, outbound: &ClientMessage) -> Result<()> {
    for member in redis::smembers(REDIS_URL, &format!("room:{room}")).map_err(redis_error)? {
        if member != url {
            send_to(&member, outbound)?;
        }
    }

    Ok(())
}

fn redis_error(_error: redis::Error) -> Error {
    anyhow!("redis error")
}

fn response() -> Builder {
    http::Response::builder()
}

fn content_type(path: &str) -> &'static str {
    // todo: use a library for this

    let default = "application/octet-stream";

    if let Some(index) = path.rfind('.') {
        match &path[(index + 1)..] {
            "html" => "text/html;charset=UTF-8",
            "css" => "text/css;charset=UTF-8",
            "js" => "text/javascript;charset=UTF-8",
            "wasm" => "application/wasm",
            _ => default,
        }
    } else {
        default
    }
}

#[http_component]
fn handle(req: Request) -> Result<Response> {
    let send_url = || get_header_url(req.headers(), "ws-bridge-send");

    println!("got request: {req:?}\n");

    Ok(match (req.method(), req.uri().path()) {
        (&Method::POST, "/frame") => {
            let message = serde_json::from_slice(
                req.body()
                    .as_deref()
                    .ok_or_else(|| anyhow!("expected non-empty body"))?,
            )?;

            match message {
                ServerMessage::Room { name } => add(send_url()?, name)?,
                ServerMessage::Ping => (),
            }

            response().body(None)?
        }

        (&Method::POST, "/disconnect") => {
            remove(send_url()?)?;

            response().body(None)?
        }

        (&Method::GET, path) => {
            if let Ok(body) = fs::read(path) {
                response()
                    .header("content-type", content_type(path))
                    .body(Some(body.into()))
            } else {
                response()
                    .header("content-type", content_type("index.html"))
                    .body(Some(fs::read("index.html")?.into()))
            }?
        }

        _ => response().status(StatusCode::BAD_REQUEST).body(None)?,
    })
}
