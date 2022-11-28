#![deny(warnings)]
use {
    anyhow::{anyhow, Context, Error, Result},
    http::response::Builder,
    http::{HeaderMap, Method, StatusCode},
    serde::{Deserialize, Serialize},
    spin_sdk::{
        http::{Request, Response},
        http_component, outbound_http, redis,
    },
    std::{env, fs::File, io::Read, str},
};

const REDIS_URL: &str = env!("REDIS_URL");

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Outbound<'a> {
    You { url: &'a str },
    Add { url: &'a str },
    Remove { url: &'a str },
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Inbound {
    Room { name: String },
}

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
        send_to(url, &Outbound::You { url })?;

        redis::sadd(REDIS_URL, &format!("room:{room}"), &[url]).map_err(redis_error)?;

        redis::set(REDIS_URL, &format!("url:{url}"), room.as_bytes()).map_err(redis_error)?;

        send_to_all(url, room, &Outbound::Add { url })?;
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

        send_to_all(url, room, &Outbound::Remove { url })?;
    }

    Ok(())
}

fn send_to(url: &str, outbound: &Outbound) -> Result<()> {
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

fn send_to_all(url: &str, room: &str, outbound: &Outbound) -> Result<()> {
    for member in redis::smembers(REDIS_URL, &format!("room:{room}")).map_err(redis_error)? {
        if member != url {
            send_to(&member, outbound)?;
        }
    }

    Ok(())
}

fn read_to_end(path: &str) -> Result<Vec<u8>> {
    let mut vec = Vec::new();

    File::open(path)?.read_to_end(&mut vec)?;

    Ok(vec)
}

fn redis_error(_error: redis::Error) -> Error {
    anyhow!("redis error")
}

fn response() -> Builder {
    http::Response::builder()
}

#[http_component]
fn handle(req: Request) -> Result<Response> {
    let send_url = || get_header_url(req.headers(), "x-ws-proxy-send");

    println!("got request: {req:?}\n");

    Ok(match (req.method(), req.uri().path()) {
        (&Method::POST, "/frame") => {
            let Inbound::Room { name } = serde_json::from_slice(
                req.body()
                    .as_deref()
                    .ok_or_else(|| anyhow!("expected non-empty body"))?,
            )?;

            add(send_url()?, &name)?;

            response().body(None)?
        }

        (&Method::POST, "/disconnect") => {
            remove(send_url()?)?;

            response().body(None)?
        }

        (&Method::GET, "/index.js") => response()
            .header("content-type", "text/javascript;charset=UTF-8")
            .body(Some(read_to_end("index.js")?.into()))?,

        (&Method::GET, "/index.css") => response()
            .header("content-type", "text/css;charset=UTF-8")
            .body(Some(read_to_end("index.css")?.into()))?,

        (&Method::GET, _) => response()
            .header("content-type", "text/html;charset=UTF-8")
            .body(Some(read_to_end("index.html")?.into()))?,

        _ => response().status(StatusCode::BAD_REQUEST).body(None)?,
    })
}
