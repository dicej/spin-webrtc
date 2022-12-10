#![deny(warnings)]

use {
    futures::{FutureExt, SinkExt, StreamExt, TryFutureExt},
    js_sys::{Array, Reflect},
    leptos::{
        self, create_component, leptos_dom,
        web_sys::{
            self, Element, HtmlVideoElement, MediaStream, MediaStreamConstraints, MediaStreamTrack,
            RtcConfiguration, RtcIceCandidateInit, RtcIceServer, RtcPeerConnection,
            RtcPeerConnectionIceEvent, RtcSdpType, RtcSessionDescriptionInit, RtcTrackEvent,
        },
        For, ForProps, IntoChild, Prop, ReadSignal, RwSignal, Scope, WriteSignal,
    },
    once_cell::unsync::OnceCell,
    reqwasm::{
        http::Request,
        websocket::{futures::WebSocket, Message, WebSocketError},
    },
    spin_webrtc_protocol::{ClientMessage, PeerMessage, ServerMessage},
    std::{cell::RefCell, collections::HashMap, fmt::Debug, ops::Deref, rc::Rc},
    thiserror::Error,
    wasm_bindgen::{closure::Closure, JsCast, JsValue},
    wasm_bindgen_futures::JsFuture,
};

#[derive(Error, Debug)]
pub enum MyError {
    #[error("JS error")]
    Js(JsValue),

    #[error("JS error")]
    GlooJs(#[from] gloo_utils::errors::JsError),

    #[error("JSON error")]
    Json(#[from] serde_json::Error),

    #[error("WebSocket error")]
    WebSocket(WebSocketError),

    #[error("HTTP error")]
    Http(#[from] reqwasm::Error),

    #[error("redundant ClientMessage::You")]
    RedundantYou,

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

fn main() {
    console_error_panic_hook::set_once();

    _ = console_log::init_with_level(log::Level::Info);

    leptos::mount_to_body(videos);
}

pub fn videos(cx: Scope) -> Element {
    let (local_video, set_local_video) = leptos::create_signal(cx, None);
    let (remote_videos, set_remote_videos) = leptos::create_signal(cx, Vec::new());

    wasm_bindgen_futures::spawn_local(async move {
        if let Err(e) = connect(cx, set_local_video, set_remote_videos).await {
            log::warn!("{e:?}");
        }
    });

    let remote_video_elements = move |_, (_, video): &(_, ReadSignal<MediaStream>)| {
        let element = leptos::view! { _, <video playsinline autoplay/> }
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
    };

    let local_video_element =
        leptos::view! { cx, <video id="localVideo" playsinline autoplay muted/> }
            .dyn_into::<HtmlVideoElement>()
            .unwrap();

    leptos::create_effect(cx, {
        let local_video_element = local_video_element.clone();

        move |_| {
            local_video_element
                .clone()
                .set_src_object(local_video.get().as_ref());
        }
    });

    leptos::view! { cx,
        <div>
            {Element::from(local_video_element)}
            <div id="remoteVideos">
                <For each=move || remote_videos.get() key=|(id, _)| *id>
                    {remote_video_elements}
                </For>
            </div>
        </div>
    }
}

struct Connection {
    id: u64,
    connection: RtcPeerConnection,
    stream: Option<RwSignal<MediaStream>>,
}

async fn connect(
    cx: Scope,
    local_video: WriteSignal<Option<MediaStream>>,
    remote_videos: WriteSignal<Vec<(u64, ReadSignal<MediaStream>)>>,
) -> Result<(), MyError> {
    let window = web_sys::window().unwrap();
    let location = window.location();

    let base = format!(
        "https://{}{}",
        location.hostname()?,
        match location.port()?.deref() {
            "" => String::new(),
            port => format!(":{port}"),
        }
    );

    let url = format!(
        "wss://{}/connect?f={base}/frame&d={base}/disconnect",
        env!("WEBSOCKET_BRIDGE_HOST")
    );

    let connections = Rc::new(RefCell::new(HashMap::<Rc<str>, Connection>::new()));

    let ice_server = |spec| {
        let mut server = RtcIceServer::new();
        server.urls(&JsValue::from_str(spec));
        server
    };

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

    let me = Rc::new(OnceCell::<Box<str>>::new());

    let send_to_peer = {
        let me = me.clone();

        move |url: &str, message: PeerMessage<'_>| {
            Request::post(url)
                .body(
                    serde_json::to_string(&ClientMessage::Peer {
                        url: me.get().unwrap().as_ref(),
                        message,
                    })
                    .unwrap(),
                )
                .send()
                .map_err(MyError::from)
        }
    };

    let update_remote_videos = {
        let connections = connections.clone();

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
    };

    let mut add = {
        let connections = connections.clone();
        let update_remote_videos = update_remote_videos.clone();
        let send_to_peer = send_to_peer.clone();
        let mut next_id = 0;

        move |url: Rc<str>| {
            log::info!("adding peer {url}");

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

            let ontrack = Closure::wrap(Box::new({
                let url = url.clone();
                let update_remote_videos = update_remote_videos.clone();
                let connections = connections.clone();

                move |event: RtcTrackEvent| match event.streams().at(0).dyn_into::<MediaStream>() {
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
            }) as Box<dyn Fn(RtcTrackEvent)>);

            connection.set_ontrack(Some(ontrack.as_ref().unchecked_ref()));

            ontrack.forget();

            let onicecandidate = Closure::wrap(Box::new({
                let url = url.clone();
                let send_to_peer = send_to_peer.clone();

                move |event: RtcPeerConnectionIceEvent| {
                    if let Some(candidate) = event.candidate() {
                        let url = url.clone();
                        let send_to_peer = send_to_peer.clone();

                        wasm_bindgen_futures::spawn_local(async move {
                            send_to_peer(
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
                            .await;

                            drop(url);
                        })
                    }
                }
            })
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
    };

    let (mut tx, mut rx) = WebSocket::open(&url)?.split();

    tx.send(Message::Text(
        serde_json::to_string(&ServerMessage::Room {
            name: &window.location().pathname()?,
        })
        .unwrap(),
    ))
    .await?;

    let get_sdp = |object: &JsValue| {
        Reflect::get(object, &JsValue::from_str("sdp"))?
            .as_string()
            .ok_or(MyError::NotAString)
    };

    while let Some(message) = rx.next().await {
        let message = message?;

        log::debug!("got message {message:?}");

        match message {
            Message::Text(message) => match serde_json::from_str::<ClientMessage>(&message)? {
                ClientMessage::You { url } => {
                    me.set(Box::from(url)).map_err(|_| MyError::RedundantYou)?
                }

                ClientMessage::Add { url } => {
                    if !connections.borrow().contains_key(url) {
                        async {
                            let connection = add(Rc::from(url))?;

                            let sdp = get_sdp(&JsFuture::from(connection.create_offer()).await?)?;

                            JsFuture::from(connection.set_local_description(
                                RtcSessionDescriptionInit::new(RtcSdpType::Offer).sdp(&sdp),
                            ))
                            .await?;

                            send_to_peer(url, PeerMessage::Offer { sdp }).await
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

                    let connection = connection.unwrap_or_else(|| add(Rc::from(url)))?;

                    match message {
                        PeerMessage::Offer { sdp } => {
                            async {
                                JsFuture::from(connection.set_remote_description(
                                    RtcSessionDescriptionInit::new(RtcSdpType::Offer).sdp(&sdp),
                                ))
                                .await?;

                                let sdp =
                                    get_sdp(&JsFuture::from(connection.create_answer()).await?)?;

                                JsFuture::from(connection.set_local_description(
                                    RtcSessionDescriptionInit::new(RtcSdpType::Answer).sdp(&sdp),
                                ))
                                .await?;

                                send_to_peer(url, PeerMessage::Answer { sdp }).await
                            }
                            .map(|result| {
                                if let Err(e) = result {
                                    log::warn!("error accepting offer from {url}: {e:?}");
                                }
                            })
                            .await
                        }

                        PeerMessage::Answer { sdp } => {
                            JsFuture::from(connection.set_remote_description(
                                RtcSessionDescriptionInit::new(RtcSdpType::Answer).sdp(&sdp),
                            ))
                            .map(|result| {
                                if let Err(e) = result {
                                    log::warn!("error accepting answer from {url}: {e:?}");
                                }
                            })
                            .await
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
                            .map(|result| {
                                if let Err(e) = result {
                                    log::warn!("error accepting answer from {url}: {e:?}");
                                }
                            })
                            .await
                        }
                    }
                }
            },

            _ => return Err(MyError::UnexpectedMessage(message)),
        }
    }

    Ok(())
}
