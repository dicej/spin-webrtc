#![deny(warnings)]

use {
    base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine},
    fluvio_wasm_timer::Delay,
    futures::{stream, FutureExt, SinkExt, StreamExt, TryStreamExt},
    js_sys::{Array, Reflect},
    leptos::{
        self, create_component, leptos_dom,
        web_sys::{
            self, Element, HtmlTextAreaElement, HtmlVideoElement, KeyboardEvent, MediaStream,
            MediaStreamConstraints, MediaStreamTrack, RtcConfiguration, RtcIceCandidateInit,
            RtcIceServer, RtcPeerConnection, RtcPeerConnectionIceEvent, RtcSdpType,
            RtcSessionDescriptionInit, RtcTrackEvent,
        },
        For, ForProps, IntoAttribute, IntoChild, NodeRef, Prop, ReadSignal, RwSignal, Scope,
        WriteSignal,
    },
    once_cell::unsync::OnceCell,
    reqwasm::{
        http::Request,
        websocket::{futures::WebSocket, Message, WebSocketError},
    },
    spin_webrtc_protocol::{ClientMessage, PeerMessage, ServerMessage},
    std::{
        cell::RefCell, collections::HashMap, fmt::Debug, io, ops::Deref, rc::Rc, time::Duration,
    },
    thiserror::Error,
    wasm_bindgen::{closure::Closure, JsCast, JsValue},
    wasm_bindgen_futures::JsFuture,
};

const PING_INTERVAL: Duration = Duration::from_secs(30);

#[derive(Error, Debug)]
pub enum MyError {
    #[error("JS error")]
    Js(JsValue),

    #[error("JS error")]
    GlooJs(#[from] gloo_utils::errors::JsError),

    #[error("JSON error")]
    Json(#[from] serde_json::Error),

    #[error("I/O error")]
    Io(#[from] io::Error),

    #[error("WebSocket error")]
    WebSocket(WebSocketError),

    #[error("HTTP error")]
    Http(#[from] reqwasm::Error),

    #[error("redundant ClientMessage::You")]
    RedundantYou,

    #[error("missed ClientMessage::You")]
    MissingYou,

    #[error("unexpected message")]
    UnexpectedMessage(Message),

    #[error("not a string")]
    NotAString,
}

impl From<JsValue> for MyError {
    fn from(e: JsValue) -> Self {
        Self::Js(e)
    }
}

impl From<WebSocketError> for MyError {
    fn from(e: WebSocketError) -> Self {
        Self::WebSocket(e)
    }
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
enum ChatSource {
    Me,
    SomeoneElse,
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct ChatMessage {
    source: ChatSource,
    message: String,
}

#[derive(Clone)]
struct ChatLog {
    element: NodeRef,
    next_id: u64,
    log: Vec<(u64, ChatMessage)>,
}

impl ChatLog {
    fn add(&mut self, message: ChatMessage) {
        self.log.push((self.next_id, message));
        self.next_id += 1;

        wasm_bindgen_futures::spawn_local({
            let element = self.element;

            async move {
                if let Some(element) = element.get() {
                    element.set_scroll_top(element.scroll_height());
                }
            }
        });
    }
}

struct Connection {
    id: u64,
    connection: RtcPeerConnection,
    stream: Option<RwSignal<MediaStream>>,
}

fn main() {
    console_error_panic_hook::set_once();

    _ = console_log::init_with_level(log::Level::Debug);

    let location = web_sys::window().unwrap().location();

    // If a room has not yet been specified, generate a random ID and use that as the room name.
    if !location.pathname().unwrap().starts_with("/room") {
        let mut bytes = [0u8; 16];
        getrandom::getrandom(&mut bytes).unwrap();

        let room = URL_SAFE_NO_PAD.encode(bytes);

        location.set_pathname(&format!("/room/{room}")).unwrap()
    }

    leptos::mount_to_body(videos);
}

async fn send_to_peer(
    me: &OnceCell<Box<str>>,
    url: &str,
    message: PeerMessage<'_>,
) -> Result<(), MyError> {
    Request::post(url)
        .body(serde_json::to_string(&ClientMessage::Peer {
            url: me.get().ok_or(MyError::MissingYou)?,
            message,
        })?)
        .send()
        .await
        .map_err(MyError::from)
        .map(drop)
}

fn videos(cx: Scope) -> Element {
    let (local_video, set_local_video) = leptos::create_signal(cx, None);

    let (remote_videos, set_remote_videos) = leptos::create_signal(cx, Vec::new());

    let chat_log_ref = NodeRef::new(cx);

    let (chat_log, set_chat_log) = leptos::create_signal(
        cx,
        ChatLog {
            element: chat_log_ref,
            next_id: 0,
            log: Vec::new(),
        },
    );

    let me = Rc::new(OnceCell::<Box<str>>::new());

    let connections = Rc::new(RefCell::new(HashMap::<Rc<str>, Connection>::new()));

    wasm_bindgen_futures::spawn_local({
        let me = me.clone();
        let connections = connections.clone();

        async move {
            if let Err(e) = connect(
                cx,
                me,
                connections,
                set_local_video,
                set_remote_videos,
                set_chat_log,
            )
            .await
            {
                log::error!("fatal error: {e:?}");
            }
        }
    });

    let on_key = make_key_listener(connections, me, set_chat_log);

    leptos::view! { cx,
        <div id="parent" class="columns is-multiline" class="column is-four-fifths-desktop is-full-touch">
            <div id="videos" class="column is-four-fifths-desktop is-full-touch">
                <div class="tile is-ancestor is-multiline">
                    {local_video_element(cx, local_video)}
                    <For each=move || remote_videos.get() key=|(id, _)| *id>
                        {remote_video_element}
                    </For>
                </div>
                <div class="card">
                    <div class="card-content">
                        <p class="title">
                            <span class="spin-logo">
                                "Spin"
                            </span>
                            " WebRTC"
                        </p>
                        <p class="subtitle">
                            "WebAssembly-powered browser-to-browser WebRTC calls - using "
                            <a href="https://developer.fermyon.com/spin/index" target="_blank">
                                "Spin"
                            </a>
                            ", "
                            <a href="https://github.com/fermyon/websocket-bridge" target="_blank">
                                "websocket-bridge"
                            </a>
                            " and "
                            <a href="https://redis.io/" target="_blank">
                                "Redis"
                            </a>
                            " to establish indirect connections between peers"
                        </p>
                    </div>
                    <footer class="card-footer">
                        <p class="card-footer-item">
                            <span>
                                "View on "
                                <a href="https://github.com/dicej/spin-webrtc" target="_blank">
                                    "GitHub"
                                </a>
                            </span>
                        </p>
                        <p class="card-footer-item">
                            <span>
                                "Learn about "
                                <a href="https://developer.fermyon.com/spin/index" target="_blank">
                                    "Spin"
                                </a>
                            </span>
                        </p>
                    </footer>
                </div>
            </div>
            <div id="chat" id="chat" class="column is-one-fifth-desktop is-full-touch">
                <div id="chatLog" _ref=chat_log_ref>
                    <h2>"Chat"</h2>
                    <For each=move || chat_log.get().log key=|(id, _)| *id>
                        {chat_log_element}
                    </For>
                </div>
                <textarea id="chatArea" name="chatArea" on:keyup=on_key/>
            </div>
        </div>
    }
}

fn local_video_element(cx: Scope, local_video: ReadSignal<Option<MediaStream>>) -> Element {
    let element = leptos::view! { cx, <video id="localVideo" class="tile is-child is-6" playsinline autoplay muted/> }
        .dyn_into::<HtmlVideoElement>()
        .unwrap();

    leptos::create_effect(cx, {
        let element = element.clone();

        move |_| {
            element.set_src_object(local_video.get().as_ref());
            element.set_muted(true);
        }
    });

    element.into()
}

fn remote_video_element(cx: Scope, (_, video): &(u64, ReadSignal<MediaStream>)) -> Element {
    let element = leptos::view! { _, <video class="tile is-child is-6" playsinline autoplay/> }
        .dyn_into::<HtmlVideoElement>()
        .unwrap();

    leptos::create_effect(cx, {
        let element = element.clone();
        let video = *video;

        move |_| {
            element.set_src_object(Some(&video.get()));
        }
    });

    element.into()
}

fn chat_log_element(cx: Scope, (_, message): &(u64, ChatMessage)) -> Element {
    let (who, class) = match message.source {
        ChatSource::Me => ("me: ", "sent"),
        ChatSource::SomeoneElse => ("them: ", "received"),
    };

    leptos::view! { cx, <div class=class><b>{who}</b>{message.message.clone()}</div> }
}

fn make_key_listener(
    connections: Rc<RefCell<HashMap<Rc<str>, Connection>>>,
    me: Rc<OnceCell<Box<str>>>,
    chat_log: WriteSignal<ChatLog>,
) -> impl Fn(KeyboardEvent) {
    move |event: KeyboardEvent| {
        if event.key().deref() == "Enter" {
            if let Some(target) = event
                .target()
                .and_then(|target| target.dyn_into::<HtmlTextAreaElement>().ok())
            {
                let message = target.value();
                target.set_value("");

                if !message.trim().is_empty() {
                    for url in connections.borrow().keys() {
                        wasm_bindgen_futures::spawn_local({
                            let me = me.clone();
                            let url = url.clone();
                            let message = message.clone();

                            async move {
                                if let Err(e) =
                                    send_to_peer(&me, &url, PeerMessage::Chat { message }).await
                                {
                                    log::warn!("error sending chat to {url}: {e:?}");
                                }
                            }
                        });
                    }

                    chat_log.update(|log| {
                        log.add(ChatMessage {
                            source: ChatSource::Me,
                            message,
                        })
                    });
                }
            }
        }
    }
}

fn ice_server(spec: &str) -> RtcIceServer {
    let mut server = RtcIceServer::new();
    server.urls(&JsValue::from_str(spec));
    server
}

fn rtc_config() -> RtcConfiguration {
    let mut config = RtcConfiguration::new();
    config.ice_servers(
        [
            ice_server("stun:stun.services.mozilla.com"),
            ice_server("stun:stun.l.google.com:19302"),
        ]
        .into_iter()
        .collect::<Array>()
        .deref(),
    );
    config
}

fn make_remote_video_updater(
    connections: Rc<RefCell<HashMap<Rc<str>, Connection>>>,
    remote_videos: WriteSignal<Vec<(u64, ReadSignal<MediaStream>)>>,
) -> impl Fn() + Clone {
    move || {
        let mut vec = connections
            .borrow()
            .values()
            .filter_map(|connection| {
                connection
                    .stream
                    .as_ref()
                    .map(|stream| (connection.id, stream.read_only()))
            })
            .collect::<Vec<_>>();

        vec.sort_by_key(|(id, _)| *id);

        remote_videos.set(vec);
    }
}

fn make_track_listener(
    cx: Scope,
    connections: Rc<RefCell<HashMap<Rc<str>, Connection>>>,
    remote_videos: WriteSignal<Vec<(u64, ReadSignal<MediaStream>)>>,
    url: Rc<str>,
) -> impl Fn(RtcTrackEvent) {
    let update_remote_videos = make_remote_video_updater(connections.clone(), remote_videos);

    move |event| match event.streams().at(0).dyn_into::<MediaStream>() {
        Ok(new_stream) => {
            log::info!("got remote stream from {url}");

            let mut need_update = false;

            if let Some(connection) = connections.borrow_mut().get_mut(&url) {
                if let Some(stream) = connection.stream {
                    stream.set(new_stream);
                } else {
                    need_update = true;
                    connection.stream = Some(leptos::create_rw_signal(cx, new_stream));
                }
            }

            if need_update {
                update_remote_videos();
            }
        }

        Err(e) => log::warn!("error getting stream from track for {url}: {e:?}"),
    }
}

fn make_ice_listener(
    me: Rc<OnceCell<Box<str>>>,
    url: Rc<str>,
) -> impl Fn(RtcPeerConnectionIceEvent) {
    move |event| {
        if let Some(candidate) = event.candidate() {
            let me = me.clone();
            let url = url.clone();

            wasm_bindgen_futures::spawn_local(async move {
                send_to_peer(
                    &me,
                    &url,
                    PeerMessage::Candidate {
                        candidate: &candidate.candidate(),
                        sdp_mid: candidate.sdp_mid().as_deref(),
                        sdp_m_line_index: candidate.sdp_m_line_index(),
                    },
                )
                .map(|result| {
                    if let Err(e) = result {
                        log::warn!("error sending ICE candidate to {url}: {e:?}");
                    }
                })
                .await
            })
        }
    }
}

fn make_connection_adder(
    cx: Scope,
    connections: Rc<RefCell<HashMap<Rc<str>, Connection>>>,
    me: Rc<OnceCell<Box<str>>>,
    remote_videos: WriteSignal<Vec<(u64, ReadSignal<MediaStream>)>>,
    local_stream: MediaStream,
) -> impl FnMut(&str) -> Result<RtcPeerConnection, MyError> {
    let update_remote_videos = make_remote_video_updater(connections.clone(), remote_videos);
    let config = rtc_config();
    let mut next_id = 0;

    move |url| {
        log::info!("adding peer {url}");

        let url = Rc::<str>::from(url);

        let connection = RtcPeerConnection::new_with_configuration(&config)?;

        connections.borrow_mut().insert(
            url.clone(),
            Connection {
                id: next_id,
                connection: connection.clone(),
                stream: None,
            },
        );

        next_id += 1;

        update_remote_videos();

        let ontrack = Closure::wrap(Box::new(make_track_listener(
            cx,
            connections.clone(),
            remote_videos,
            url.clone(),
        )) as Box<dyn Fn(RtcTrackEvent)>);

        connection.set_ontrack(Some(ontrack.as_ref().unchecked_ref()));

        ontrack.forget();

        let onicecandidate = Closure::wrap(Box::new(make_ice_listener(me.clone(), url.clone()))
            as Box<dyn Fn(RtcPeerConnectionIceEvent)>);

        connection.set_onicecandidate(Some(onicecandidate.as_ref().unchecked_ref()));

        onicecandidate.forget();

        for track in local_stream.get_tracks().iter() {
            log::info!("adding track for {url}: {track:?}");

            connection.add_track(
                &track.dyn_into::<MediaStreamTrack>()?,
                &local_stream,
                &Array::new(),
            );
        }

        Ok::<_, MyError>(connection)
    }
}

fn get_sdp(object: &JsValue) -> Result<String, MyError> {
    Reflect::get(object, &JsValue::from_str("sdp"))?
        .as_string()
        .ok_or(MyError::NotAString)
}

async fn handle_peer_message(
    me: &OnceCell<Box<str>>,
    chat_log: WriteSignal<ChatLog>,
    url: &str,
    connection: RtcPeerConnection,
    message: PeerMessage<'_>,
) -> Result<(), MyError> {
    match message {
        PeerMessage::Offer { sdp } => {
            JsFuture::from(connection.set_remote_description(
                RtcSessionDescriptionInit::new(RtcSdpType::Offer).sdp(&sdp),
            ))
            .await?;

            let sdp = get_sdp(&JsFuture::from(connection.create_answer()).await?)?;

            JsFuture::from(connection.set_local_description(
                RtcSessionDescriptionInit::new(RtcSdpType::Answer).sdp(&sdp),
            ))
            .await?;

            send_to_peer(me, url, PeerMessage::Answer { sdp }).await?;
        }

        PeerMessage::Answer { sdp } => {
            JsFuture::from(connection.set_remote_description(
                RtcSessionDescriptionInit::new(RtcSdpType::Answer).sdp(&sdp),
            ))
            .await?;
        }

        PeerMessage::Candidate {
            candidate,
            sdp_mid,
            sdp_m_line_index,
        } => {
            JsFuture::from(
                connection.add_ice_candidate_with_opt_rtc_ice_candidate_init(Some(
                    RtcIceCandidateInit::new(candidate)
                        .sdp_mid(sdp_mid)
                        .sdp_m_line_index(sdp_m_line_index),
                )),
            )
            .await?;
        }

        PeerMessage::Chat { message } => {
            chat_log.update(|log| {
                log.add(ChatMessage {
                    source: ChatSource::SomeoneElse,
                    message,
                })
            });
        }
    }

    Ok(())
}

async fn handle_message(
    connections: &RefCell<HashMap<Rc<str>, Connection>>,
    me: &OnceCell<Box<str>>,
    chat_log: WriteSignal<ChatLog>,
    add_connection: &mut dyn (FnMut(&str) -> Result<RtcPeerConnection, MyError>),
    update_remote_videos: &dyn (Fn()),
    message: Message,
) -> Result<(), MyError> {
    log::debug!("got message {message:?}");

    match message {
        Message::Text(message) => match serde_json::from_str::<ClientMessage>(&message)? {
            ClientMessage::You { url } => {
                me.set(Box::from(url)).map_err(|_| MyError::RedundantYou)?
            }

            ClientMessage::Add { url } => {
                if !connections.borrow().contains_key(url) {
                    async {
                        let connection = add_connection(url)?;

                        let sdp = get_sdp(&JsFuture::from(connection.create_offer()).await?)?;

                        JsFuture::from(connection.set_local_description(
                            RtcSessionDescriptionInit::new(RtcSdpType::Offer).sdp(&sdp),
                        ))
                        .await?;

                        send_to_peer(me, url, PeerMessage::Offer { sdp }).await
                    }
                    .map(|result| {
                        if let Err(e) = result {
                            log::warn!("error adding connection {url}: {e:?}");
                        }
                    })
                    .await
                }
            }

            ClientMessage::Remove { url } => {
                connections.borrow_mut().remove(url);

                update_remote_videos();
            }

            ClientMessage::Peer { url, message } => {
                let connection = connections
                    .borrow()
                    .get(url)
                    .map(|c| Ok(c.connection.clone()));

                if let Err(e) = handle_peer_message(
                    me,
                    chat_log,
                    url,
                    connection.unwrap_or_else(|| add_connection(url))?,
                    message,
                )
                .await
                {
                    log::warn!("error accepting offer from {url}: {e:?}");
                }
            }
        },

        _ => return Err(MyError::UnexpectedMessage(message)),
    }

    Ok(())
}

fn websocket_url() -> Result<String, MyError> {
    let location = web_sys::window().unwrap().location();

    let base = format!(
        "https://{}{}",
        location.hostname()?,
        match location.port()?.deref() {
            "" => String::new(),
            port => format!(":{port}"),
        }
    );

    Ok(format!(
        "wss://{}/connect?f={base}/frame&d={base}/disconnect",
        env!("WEBSOCKET_BRIDGE_HOST")
    ))
}

async fn connect(
    cx: Scope,
    me: Rc<OnceCell<Box<str>>>,
    connections: Rc<RefCell<HashMap<Rc<str>, Connection>>>,
    local_video: WriteSignal<Option<MediaStream>>,
    remote_videos: WriteSignal<Vec<(u64, ReadSignal<MediaStream>)>>,
    chat_log: WriteSignal<ChatLog>,
) -> Result<(), MyError> {
    let window = web_sys::window().unwrap();

    let local_stream = JsFuture::from(
        window
            .navigator()
            .media_devices()?
            .get_user_media_with_constraints(
                MediaStreamConstraints::new()
                    .audio(&JsValue::TRUE)
                    .video(&JsValue::TRUE),
            )?,
    )
    .await?
    .dyn_into::<MediaStream>()?;

    local_video.set(Some(local_stream.clone()));

    let (mut tx, rx) = WebSocket::open(&websocket_url()?)?.split();

    tx.send(Message::Text(serde_json::to_string(
        &ServerMessage::Room {
            name: &window.location().href()?,
        },
    )?))
    .await?;

    let mut add_connection = make_connection_adder(
        cx,
        connections.clone(),
        me.clone(),
        remote_videos,
        local_stream,
    );

    let update_remote_videos = make_remote_video_updater(connections.clone(), remote_videos);

    let ping_intervals = stream::unfold((), |()| Delay::new(PING_INTERVAL).map(|r| Some((r, ()))));

    enum Item {
        Message(Message),
        Ping,
    }

    let mut rx = stream::select(
        ping_intervals.map(|r| r.map(|()| Item::Ping).map_err(MyError::from)),
        rx.map(|r| r.map(Item::Message).map_err(MyError::from)),
    );

    while let Some(item) = rx.try_next().await? {
        match item {
            Item::Message(message) => {
                handle_message(
                    &connections,
                    &me,
                    chat_log,
                    &mut add_connection,
                    &update_remote_videos,
                    message,
                )
                .await?
            }

            Item::Ping => {
                // Time to ping the server.  This is to avoid idle connection timeouts which might be imposed by
                // intermediate proxies.

                tx.send(Message::Text(serde_json::to_string(&ServerMessage::Ping)?))
                    .await?;
            }
        }
    }

    Ok(())
}
