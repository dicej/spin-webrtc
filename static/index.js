async function start(url) {
    const connections = {}
    const config = {
        iceServers: [
            { urls: "stun:stun.services.mozilla.com" },
            { urls: "stun:stun.l.google.com:19302" }
        ]
    }

    const stream = await navigator.mediaDevices.getUserMedia({
        audio: true,
        video: true
    })

    const localVideo = document.getElementById("localVideo")
    localVideo.srcObject = stream

    let me = undefined

    const sendToPeer = (url, body) => {
        // console.log(`send message to ${url}`, body)

        fetch(url, {
            method: "POST",
            body: JSON.stringify({
                type: "peer",
                url: me,
                body
            })
        }).catch(e => {
            console.log(`error sending to ${url}`, e)
        })
    }

    const add = url => {
        const connection = new RTCPeerConnection(config)
        connections[url] = { connection }

        connection.ontrack = event => {
            console.log(`got remote stream from ${url}`)

            if (!connections[url].element) {
                const video = document.createElement("video")
                video.setAttribute("playsinline", "")
                video.setAttribute("autoplay", "")
                connections[url].element = video
                document.getElementById("remoteVideos").appendChild(video)
            }

            connections[url].element.srcObject = event.streams[0]
        }

        connection.onicecandidate = event => {
            if (event.candidate) {
                sendToPeer(url, event.candidate)
            }
        }

        for (const track of stream.getTracks()) {
            console.log(`adding track for ${url}`, track)

            connection.addTrack(track, stream)
        }

        return connection
    }

    const ws = new WebSocket(url)

    ws.onopen = event => {
        ws.send(JSON.stringify({
            type: "room",
            name: location.pathname
        }))
    }

    ws.onmessage = event => {
        // console.log("receive message", event.data)

        const message = JSON.parse(event.data)

        if (message["type"] == "you") {
            me = message["url"]
        } else if (message["type"] == "add") {
            const url = message["url"]
            if (!connections[url]) {
                const connection = add(url)

                connection.createOffer().then(offer => {
                    connection.setLocalDescription(new RTCSessionDescription(offer))

                    sendToPeer(url, offer)
                })
            }
        } else if (message["type"] == "remove") {
            const url = message["url"]
            if (connections[url]) {
                connections[url].connection.close()
                if (connections[url].element) {
                    connections[url].element.remove()
                }
                delete connections[url]
            }
        } else if (message["type"] == "peer") {
            const url = message["url"]
            if (!connections[url]) {
                add(url)
            }

            const connection = connections[url].connection
            const body = message["body"]
            if (body["type"] == "offer") {
                connection.setRemoteDescription(new RTCSessionDescription(body))
                connection.createAnswer().then(answer => {
                    connection.setLocalDescription(new RTCSessionDescription(answer))
                    sendToPeer(url, answer)
                }).catch(e => {
                    console.log(`error creating answer for ${url}`, e)
                })
            } else if (body["type"] == "answer") {
                connection.setRemoteDescription(new RTCSessionDescription(body))
            } else if (body["candidate"]) {
                connection.addIceCandidate(body).catch(e => {
                    console.log(`error adding candidate for ${url}`, e)
                })
            }
        } else {
            console.log("unrecognized message", message)
        }
    }
}

const base = "https://" + location.hostname + (location.port ? ":" + location.port : "")

start("wss://[insert your websocket-bridge server here]/connect?f=" + base + "/frame&d=" + base + "/disconnect")
