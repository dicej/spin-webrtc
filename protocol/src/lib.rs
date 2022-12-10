use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PeerMessage<'a> {
    Candidate {
        candidate: &'a str,
        sdp_mid: Option<&'a str>,
        sdp_m_line_index: Option<u16>,
    },
    Offer {
        sdp: String,
    },
    Answer {
        sdp: String,
    },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage<'a> {
    You {
        url: &'a str,
    },
    Add {
        url: &'a str,
    },
    Remove {
        url: &'a str,
    },
    Peer {
        url: &'a str,
        message: PeerMessage<'a>,
    },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage<'a> {
    Room { name: &'a str },
}
