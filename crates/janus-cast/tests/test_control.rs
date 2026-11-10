use janus_cast::control::*;
use janus_cast::wire::{
    self, CONNECTION_NAMESPACE, HEARTBEAT_NAMESPACE, Message, Payload, RECEIVER,
    RECEIVER_NAMESPACE, SENDER,
};
fn message(namespace: &str, text: &str) -> Message {
    Message::new(RECEIVER, SENDER, namespace, Payload::Text(text.into())).unwrap()
}

#[test]
fn heartbeat_and_read_only_request_are_valid_cast_messages() {
    let request = get_status(7).unwrap();
    let Payload::Text(text) = request.payload else {
        panic!("not text")
    };
    let value: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(value["type"], "GET_STATUS");
    assert_eq!(value["requestId"], 7);
    assert!(get_status(0).is_err());
    assert!(matches!(
        parse(&message(HEARTBEAT_NAMESPACE, "{\"type\":\"PING\"}"), None),
        Ok(Event::Ping)
    ));
    assert_eq!(pong().unwrap().destination, RECEIVER);
}

#[test]
fn busy_status_is_an_observation_not_a_stop_or_launch() {
    let msg = message(
        RECEIVER_NAMESPACE,
        r#"{"type":"RECEIVER_STATUS","requestId":7,"status":{"applications":[{"appId":"app","sessionId":"private-session","transportId":"transport","namespaces":[{"name":"urn:x-cast:com.google.cast.media"}]}],"volume":{"level":0.5,"muted":false}}}"#,
    );
    let Event::Status { status, .. } = parse(&msg, Some(7)).unwrap() else {
        panic!("not status")
    };
    assert!(status.has_active_application());
    assert!(!format!("{status:?}").contains("private-session"));
    assert!(matches!(parse(&msg, Some(8)), Err(Error::Correlation)));
}

#[test]
fn idle_unsolicited_status_close_and_unknown_namespace() {
    let msg = message(
        RECEIVER_NAMESPACE,
        r#"{"type":"RECEIVER_STATUS","status":{}}"#,
    );
    let Event::Status { request_id, status } = parse(&msg, None).unwrap() else {
        panic!("not status")
    };
    assert_eq!(request_id, None);
    assert!(!status.has_active_application());
    assert!(matches!(
        parse(&message(CONNECTION_NAMESPACE, r#"{"type":"CLOSE"}"#), None),
        Ok(Event::Closed)
    ));
    assert!(matches!(
        parse(&message("urn:x-cast:future", "{}"), None),
        Ok(Event::Other)
    ));
}

#[test]
fn routing_invalid_json_volume_and_application_flood_are_rejected() {
    let mut msg = message(
        RECEIVER_NAMESPACE,
        r#"{"type":"RECEIVER_STATUS","status":{"volume":{"level":2.0}}}"#,
    );
    assert!(parse(&msg, None).is_err());
    msg.source = "other-app".into();
    assert!(matches!(parse(&msg, None), Err(Error::Route)));
    assert!(parse(&message(RECEIVER_NAMESPACE, "{"), None).is_err());
    let application = serde_json::json!({"appId":"a", "sessionId":"s", "transportId":"t"});
    let text = serde_json::json!({"type":"RECEIVER_STATUS","status":{"applications":vec![application; MAX_APPLICATIONS + 1]}}).to_string();
    assert!(parse(&message(RECEIVER_NAMESPACE, &text), None).is_err());
}

#[tokio::test]
async fn status_and_heartbeat_roundtrip_over_the_framed_channel() {
    let (mut sender, mut receiver) = tokio::io::duplex(8192);
    let peer = tokio::spawn(async move {
        let request = wire::read(&mut receiver).await.unwrap();
        assert_eq!(request.namespace, RECEIVER_NAMESPACE);
        let Payload::Text(json) = request.payload else {
            panic!("wrong payload")
        };
        let request: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(request["type"], "GET_STATUS");
        assert_eq!(request["requestId"], 23);
        wire::write(
            &mut receiver,
            &message(
                RECEIVER_NAMESPACE,
                r#"{"type":"RECEIVER_STATUS","requestId":23,"status":{"applications":[]}}"#,
            ),
        )
        .await
        .unwrap();
        wire::write(
            &mut receiver,
            &message(HEARTBEAT_NAMESPACE, r#"{"type":"PING"}"#),
        )
        .await
        .unwrap();
        let response = wire::read(&mut receiver).await.unwrap();
        assert_eq!(response.source, SENDER);
        assert_eq!(response.destination, RECEIVER);
        let Payload::Text(json) = response.payload else {
            panic!("wrong payload")
        };
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&json).unwrap()["type"],
            "PONG"
        );
    });
    wire::write(&mut sender, &get_status(23).unwrap())
        .await
        .unwrap();
    let response = wire::read(&mut sender).await.unwrap();
    assert!(matches!(
        parse(&response, Some(23)),
        Ok(Event::Status { .. })
    ));
    let ping = wire::read(&mut sender).await.unwrap();
    assert!(matches!(parse(&ping, None), Ok(Event::Ping)));
    wire::write(&mut sender, &pong().unwrap()).await.unwrap();
    peer.await.unwrap();
}

fn json(message: &Message) -> serde_json::Value {
    let Payload::Text(text) = &message.payload else {
        panic!("text payload expected")
    };
    serde_json::from_str(text).unwrap()
}

#[test]
fn test_connect_close_and_ping_follow_the_platform_connection_format() {
    let connect = connect().unwrap();
    assert_eq!(connect.namespace, CONNECTION_NAMESPACE);
    assert_eq!(connect.destination, RECEIVER);
    let body = json(&connect);
    assert_eq!(body["type"], "CONNECT");
    assert_eq!(body["connType"], 0);
    assert!(body["origin"].as_object().unwrap().is_empty());
    assert_eq!(body["senderInfo"]["connectionType"], 1);
    assert_eq!(json(&close().unwrap())["type"], "CLOSE");
    let ping = ping().unwrap();
    assert_eq!(ping.namespace, HEARTBEAT_NAMESPACE);
    assert_eq!(json(&ping)["type"], "PING");
}

#[test]
fn test_app_availability_request_is_bounded_and_launches_nothing() {
    let request = get_app_availability(9, &[SCREEN_MIRRORING_APP, AUDIO_MIRRORING_APP]).unwrap();
    assert_eq!(request.namespace, RECEIVER_NAMESPACE);
    let body = json(&request);
    assert_eq!(body["type"], "GET_APP_AVAILABILITY");
    assert_eq!(body["requestId"], 9);
    assert_eq!(body["appId"], serde_json::json!(["0F5096E8", "85CDB22F"]));
    assert!(get_app_availability(0, &[SCREEN_MIRRORING_APP]).is_err());
    assert!(get_app_availability(1, &[]).is_err());
    assert!(get_app_availability(1, &["A"; 9]).is_err());
}

#[test]
fn test_app_availability_response_is_correlated_and_validated() {
    let reply = |body: serde_json::Value| message(RECEIVER_NAMESPACE, &body.to_string());
    let good = reply(
        serde_json::json!({"responseType":"GET_APP_AVAILABILITY","requestId":4,
        "availability":{"0F5096E8":"APP_AVAILABLE","85CDB22F":"APP_UNAVAILABLE","ABC":"???"}}),
    );
    let Event::AppAvailability { request_id, apps } = parse(&good, Some(4)).unwrap() else {
        panic!("availability event expected")
    };
    assert_eq!(request_id, 4);
    assert!(apps.contains(&("0F5096E8".into(), AppAvailability::Available)));
    assert!(apps.contains(&("85CDB22F".into(), AppAvailability::Unavailable)));
    assert!(apps.contains(&("ABC".into(), AppAvailability::Unknown)));
    assert!(matches!(parse(&good, Some(5)), Err(Error::Correlation)));
    for bad in [
        serde_json::json!({"responseType":"GET_APP_AVAILABILITY","availability":{}}),
        serde_json::json!({"responseType":"GET_APP_AVAILABILITY","requestId":1}),
        serde_json::json!({"responseType":"GET_APP_AVAILABILITY","requestId":1,
            "availability":{"bad id!":"APP_AVAILABLE"}}),
        serde_json::json!({"responseType":"GET_APP_AVAILABILITY","requestId":1,
            "availability":(0..9).map(|n| (format!("A{n}"), "APP_AVAILABLE")).collect::<std::collections::BTreeMap<_, _>>()}),
    ] {
        assert!(parse(&reply(bad), None).is_err());
    }
    // A reply with neither `type` nor a known responseType is ignored.
    assert!(matches!(
        parse(&reply(serde_json::json!({"x":1})), None),
        Ok(Event::Other)
    ));
}
