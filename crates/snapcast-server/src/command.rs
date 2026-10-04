//! Command dispatch — handles each [`ServerCommand`] against shared state.
//!
//! Extracted from the `run()` loop so the state machine is one focused unit and
//! the crate root keeps to API types plus the server facade. `Stop`/`None`
//! terminate the loop and are handled by `run()` itself; every other command
//! lands here.

use std::sync::Arc;

use tokio::sync::{Mutex, mpsc};

use crate::session::SessionServer;
use crate::state::ServerState;
use crate::{ClientSettingsUpdate, ServerCommand, ServerEvent};

/// The handles and config a command handler needs (everything but loop control).
pub(crate) struct Dispatcher {
    /// Shared server state, guarded for concurrent access with the session task.
    pub(crate) shared_state: Arc<Mutex<ServerState>>,
    /// Session server — pushes settings and recomputes audio routing.
    pub(crate) session_srv: Arc<SessionServer>,
    /// Outbound event channel to the embedder (events + state snapshots).
    pub(crate) event_tx: mpsc::Sender<ServerEvent>,
    /// Playout buffer size in milliseconds (pushed to clients).
    pub(crate) buffer_ms: i32,
}

impl Dispatcher {
    async fn emit_event(&self, event: ServerEvent) {
        if let Err(e) = self.event_tx.send(event).await {
            tracing::warn!(error = %e, "failed to deliver server event");
        }
    }

    /// Handle one command.
    ///
    /// `Stop`/`None` are handled by the run loop and never reach here.
    pub(crate) async fn dispatch(&self, cmd: ServerCommand) {
        match cmd {
            ServerCommand::Stop => unreachable!("Stop is handled by the run loop"),
            ServerCommand::SetClientVolume {
                client_id,
                volume,
                muted,
            } => {
                let mut s = self.shared_state.lock().await;
                if let Some(c) = s.clients.get_mut(&client_id) {
                    c.config.volume.percent = volume;
                    c.config.volume.muted = muted;
                }
                let latency = s
                    .clients
                    .get(&client_id)
                    .map(|c| c.config.latency)
                    .unwrap_or(0);
                let snapshot = s.clone();
                drop(s);
                self.emit_event(ServerEvent::StateChanged(snapshot)).await;
                self.session_srv
                    .push_settings(ClientSettingsUpdate {
                        client_id: client_id.clone(),
                        buffer_ms: self.buffer_ms,
                        latency,
                        volume,
                        muted,
                    })
                    .await;
                self.emit_event(ServerEvent::ClientVolumeChanged {
                    client_id: client_id.clone(),
                    volume,
                    muted,
                })
                .await;
                self.session_srv.update_routing_for_client(&client_id).await;
            }
            ServerCommand::SetClientLatency { client_id, latency } => {
                let mut settings_update = None;
                let mut s = self.shared_state.lock().await;
                if let Some(c) = s.clients.get_mut(&client_id) {
                    c.config.latency = latency;
                    settings_update = Some(ClientSettingsUpdate {
                        client_id: client_id.clone(),
                        buffer_ms: self.buffer_ms,
                        latency,
                        volume: c.config.volume.percent,
                        muted: c.config.volume.muted,
                    });
                }
                let snapshot = s.clone();
                drop(s);
                self.emit_event(ServerEvent::StateChanged(snapshot)).await;
                if let Some(update) = settings_update {
                    self.session_srv.push_settings(update).await;
                }
                self.emit_event(ServerEvent::ClientLatencyChanged { client_id, latency })
                    .await;
            }
            ServerCommand::SetClientName { client_id, name } => {
                let mut s = self.shared_state.lock().await;
                if let Some(c) = s.clients.get_mut(&client_id) {
                    c.config.name = name.clone();
                }
                let snapshot = s.clone();
                drop(s);
                self.emit_event(ServerEvent::StateChanged(snapshot)).await;
                self.emit_event(ServerEvent::ClientNameChanged { client_id, name })
                    .await;
            }
            ServerCommand::SetGroupStream {
                group_id,
                stream_id,
            } => {
                let mut s = self.shared_state.lock().await;
                s.set_group_stream(&group_id, &stream_id);
                let snapshot = s.clone();
                drop(s);
                self.emit_event(ServerEvent::StateChanged(snapshot)).await;
                self.emit_event(ServerEvent::GroupStreamChanged {
                    group_id: group_id.clone(),
                    stream_id,
                })
                .await;
                self.session_srv.update_routing_for_group(&group_id).await;
            }
            ServerCommand::SetGroupMute { group_id, muted } => {
                let mut s = self.shared_state.lock().await;
                if let Some(g) = s.groups.iter_mut().find(|g| g.id == group_id) {
                    g.muted = muted;
                }
                let snapshot = s.clone();
                drop(s);
                self.emit_event(ServerEvent::StateChanged(snapshot)).await;
                self.emit_event(ServerEvent::GroupMuteChanged {
                    group_id: group_id.clone(),
                    muted,
                })
                .await;
                self.session_srv.update_routing_for_group(&group_id).await;
            }
            ServerCommand::SetGroupName { group_id, name } => {
                let mut s = self.shared_state.lock().await;
                if let Some(g) = s.groups.iter_mut().find(|g| g.id == group_id) {
                    g.name = name.clone();
                }
                let snapshot = s.clone();
                drop(s);
                self.emit_event(ServerEvent::StateChanged(snapshot)).await;
                self.emit_event(ServerEvent::GroupNameChanged { group_id, name })
                    .await;
            }
            ServerCommand::SetGroupClients { group_id, clients } => {
                let mut s = self.shared_state.lock().await;
                s.set_group_clients(&group_id, &clients);
                let snapshot = s.clone();
                drop(s);
                self.emit_event(ServerEvent::StateChanged(snapshot)).await;
                // Structural change — mirrors Server.OnUpdate in C++ snapserver
                self.emit_event(ServerEvent::ServerUpdated).await;
                self.session_srv.update_routing_all().await;
            }
            ServerCommand::DeleteClient { client_id } => {
                let mut s = self.shared_state.lock().await;
                s.remove_client_from_groups(&client_id);
                s.clients.remove(&client_id);
                let snapshot = s.clone();
                drop(s);
                self.emit_event(ServerEvent::StateChanged(snapshot)).await;
                self.emit_event(ServerEvent::ServerUpdated).await;
                self.session_srv.update_routing_all().await;
            }
            ServerCommand::SetStreamMeta {
                stream_id,
                metadata,
            } => {
                let mut s = self.shared_state.lock().await;
                if let Some(stream) = s.streams.iter_mut().find(|st| st.id == stream_id) {
                    stream.properties = metadata.clone();
                }
                drop(s);
                self.emit_event(ServerEvent::StreamMetaChanged {
                    stream_id,
                    metadata,
                })
                .await;
            }
            ServerCommand::AddStream { uri, response_tx } => {
                tracing::warn!(
                    uri,
                    "Dynamic stream addition requires application-owned stream orchestration"
                );
                let _ = response_tx.send(Err(
                    "dynamic Stream.AddStream is not supported by the embeddable server after run(); create streams before run()".into(),
                ));
            }
            ServerCommand::RemoveStream { stream_id } => {
                let mut s = self.shared_state.lock().await;
                s.streams.retain(|st| st.id != stream_id);
                // Clear stream_id on groups that referenced this stream
                for g in &mut s.groups {
                    if g.stream_id == stream_id {
                        g.stream_id.clear();
                    }
                }
                let snapshot = s.clone();
                drop(s);
                self.emit_event(ServerEvent::StateChanged(snapshot)).await;
                self.emit_event(ServerEvent::ServerUpdated).await;
                self.session_srv.update_routing_all().await;
            }
            ServerCommand::StreamControl {
                stream_id,
                command,
                params,
            } => {
                tracing::debug!(stream_id, command, ?params, "Stream control forwarded");
                // Forward to embedder via event — the library doesn't own stream readers
                self.emit_event(ServerEvent::StreamControl {
                    stream_id,
                    command,
                    params,
                })
                .await;
            }
            ServerCommand::GetStatus { response_tx } => {
                let s = self.shared_state.lock().await;
                let _ = response_tx.send(s.to_status());
            }
            #[cfg(feature = "custom-protocol")]
            ServerCommand::SendToClient { client_id, message } => {
                self.session_srv
                    .send_custom(&client_id, message.type_id, message.payload)
                    .await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::SessionServerConfig;
    use crate::state::{ServerState, StreamInfo};
    use std::collections::HashMap;

    /// Build a dispatcher over `state`, returning it and its event receiver. The
    /// session server has no registered client senders, so `push_settings` /
    /// `update_routing_*` are no-ops — exactly the isolation we want for testing
    /// the command state machine.
    fn dispatcher_with(state: ServerState) -> (Dispatcher, mpsc::Receiver<ServerEvent>) {
        let shared_state = Arc::new(Mutex::new(state));
        let session_srv = Arc::new(SessionServer::new(SessionServerConfig {
            buffer_ms: 1000,
            auth: None,
            client_filter: None,
            shared_state: Arc::clone(&shared_state),
            default_stream: "default".into(),
            send_audio_to_muted: false,
        }));
        let (event_tx, event_rx) = mpsc::channel(256);
        (
            Dispatcher {
                shared_state,
                session_srv,
                event_tx,
                buffer_ms: 1000,
            },
            event_rx,
        )
    }

    /// State with one client `c1` in a group on stream `default`; returns the group id.
    fn state_with_client() -> (ServerState, String) {
        let mut state = ServerState::default();
        state.get_or_create_client("c1", "host1", "mac1");
        let gid = state.group_for_client("c1", "default").id.clone();
        (state, gid)
    }

    fn drain(rx: &mut mpsc::Receiver<ServerEvent>) -> Vec<ServerEvent> {
        let mut out = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            out.push(ev);
        }
        out
    }

    #[tokio::test]
    async fn set_client_volume_mutates_state_and_emits_events() {
        let (state, _gid) = state_with_client();
        let (d, mut rx) = dispatcher_with(state);
        d.dispatch(ServerCommand::SetClientVolume {
            client_id: "c1".into(),
            volume: 42,
            muted: true,
        })
        .await;
        {
            let s = d.shared_state.lock().await;
            let c = s.clients.get("c1").unwrap();
            assert_eq!(c.config.volume.percent, 42);
            assert!(c.config.volume.muted);
        }
        let events = drain(&mut rx);
        assert!(
            events
                .iter()
                .any(|e| matches!(e, ServerEvent::StateChanged(_)))
        );
        assert!(events.iter().any(|e| matches!(
            e,
            ServerEvent::ClientVolumeChanged {
                volume: 42,
                muted: true,
                ..
            }
        )));
    }

    #[tokio::test]
    async fn set_client_volume_unknown_client_is_noop_but_still_notifies() {
        let (d, mut rx) = dispatcher_with(ServerState::default());
        d.dispatch(ServerCommand::SetClientVolume {
            client_id: "ghost".into(),
            volume: 10,
            muted: false,
        })
        .await;
        assert!(
            drain(&mut rx)
                .iter()
                .any(|e| matches!(e, ServerEvent::ClientVolumeChanged { .. }))
        );
    }

    #[tokio::test]
    async fn set_client_latency_unknown_client_still_emits_latency_event() {
        let (d, mut rx) = dispatcher_with(ServerState::default());
        d.dispatch(ServerCommand::SetClientLatency {
            client_id: "ghost".into(),
            latency: 77,
        })
        .await;
        let events = drain(&mut rx);
        assert!(
            events
                .iter()
                .any(|e| matches!(e, ServerEvent::ClientLatencyChanged { latency: 77, .. }))
        );
    }

    #[tokio::test]
    async fn set_client_latency_updates_config() {
        let (state, _gid) = state_with_client();
        let (d, mut rx) = dispatcher_with(state);
        d.dispatch(ServerCommand::SetClientLatency {
            client_id: "c1".into(),
            latency: 33,
        })
        .await;
        {
            let s = d.shared_state.lock().await;
            assert_eq!(s.clients.get("c1").unwrap().config.latency, 33);
        }
        assert!(
            drain(&mut rx)
                .iter()
                .any(|e| matches!(e, ServerEvent::ClientLatencyChanged { latency: 33, .. }))
        );
    }

    #[tokio::test]
    async fn set_client_name_updates_config() {
        let (state, _gid) = state_with_client();
        let (d, mut rx) = dispatcher_with(state);
        d.dispatch(ServerCommand::SetClientName {
            client_id: "c1".into(),
            name: "Kitchen".into(),
        })
        .await;
        {
            let s = d.shared_state.lock().await;
            assert_eq!(s.clients.get("c1").unwrap().config.name, "Kitchen");
        }
        assert!(
            drain(&mut rx)
                .iter()
                .any(|e| matches!(e, ServerEvent::ClientNameChanged { .. }))
        );
    }

    #[tokio::test]
    async fn set_group_stream_reassigns_and_emits() {
        let (state, gid) = state_with_client();
        let (d, mut rx) = dispatcher_with(state);
        d.dispatch(ServerCommand::SetGroupStream {
            group_id: gid.clone(),
            stream_id: "living-room".into(),
        })
        .await;
        {
            let s = d.shared_state.lock().await;
            let g = s.groups.iter().find(|g| g.id == gid).unwrap();
            assert_eq!(g.stream_id, "living-room");
        }
        assert!(
            drain(&mut rx)
                .iter()
                .any(|e| matches!(e, ServerEvent::GroupStreamChanged { .. }))
        );
    }

    #[tokio::test]
    async fn set_group_mute_and_name() {
        let (state, gid) = state_with_client();
        let (d, mut rx) = dispatcher_with(state);
        d.dispatch(ServerCommand::SetGroupMute {
            group_id: gid.clone(),
            muted: true,
        })
        .await;
        d.dispatch(ServerCommand::SetGroupName {
            group_id: gid.clone(),
            name: "Upstairs".into(),
        })
        .await;
        {
            let s = d.shared_state.lock().await;
            let g = s.groups.iter().find(|g| g.id == gid).unwrap();
            assert!(g.muted);
            assert_eq!(g.name, "Upstairs");
        }
        let events = drain(&mut rx);
        assert!(
            events
                .iter()
                .any(|e| matches!(e, ServerEvent::GroupMuteChanged { muted: true, .. }))
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, ServerEvent::GroupNameChanged { .. }))
        );
    }

    #[tokio::test]
    async fn set_group_name_unknown_group_still_emits_event() {
        let (d, mut rx) = dispatcher_with(ServerState::default());
        d.dispatch(ServerCommand::SetGroupName {
            group_id: "missing-group".into(),
            name: "Noop".into(),
        })
        .await;
        let events = drain(&mut rx);
        assert!(
            events
                .iter()
                .any(|e| matches!(e, ServerEvent::GroupNameChanged { group_id, .. } if group_id == "missing-group"))
        );
    }

    #[tokio::test]
    async fn delete_client_removes_from_state_and_groups() {
        let (state, _gid) = state_with_client();
        let (d, mut rx) = dispatcher_with(state);
        d.dispatch(ServerCommand::DeleteClient {
            client_id: "c1".into(),
        })
        .await;
        {
            let s = d.shared_state.lock().await;
            assert!(!s.clients.contains_key("c1"));
            assert!(
                s.groups
                    .iter()
                    .all(|g| !g.clients.contains(&"c1".to_string()))
            );
        }
        assert!(
            drain(&mut rx)
                .iter()
                .any(|e| matches!(e, ServerEvent::ServerUpdated))
        );
    }

    #[tokio::test]
    async fn delete_unknown_client_still_emits_structural_update() {
        let (d, mut rx) = dispatcher_with(ServerState::default());
        d.dispatch(ServerCommand::DeleteClient {
            client_id: "ghost".into(),
        })
        .await;
        let events = drain(&mut rx);
        assert!(
            events
                .iter()
                .any(|e| matches!(e, ServerEvent::ServerUpdated))
        );
    }

    #[tokio::test]
    async fn set_group_clients_reorganizes() {
        let mut state = ServerState::default();
        state.get_or_create_client("c1", "h1", "m1");
        state.get_or_create_client("c2", "h2", "m2");
        let gid = state.group_for_client("c1", "default").id.clone();
        state.group_for_client("c2", "default");
        let (d, mut rx) = dispatcher_with(state);
        d.dispatch(ServerCommand::SetGroupClients {
            group_id: gid.clone(),
            clients: vec!["c1".into(), "c2".into()],
        })
        .await;
        {
            let s = d.shared_state.lock().await;
            let g = s.groups.iter().find(|g| g.id == gid).unwrap();
            assert_eq!(g.clients.len(), 2);
        }
        assert!(
            drain(&mut rx)
                .iter()
                .any(|e| matches!(e, ServerEvent::ServerUpdated))
        );
    }

    #[tokio::test]
    async fn set_stream_meta_updates_properties() {
        let mut state = ServerState::default();
        state.streams.push(StreamInfo {
            id: "default".into(),
            status: "playing".into(),
            uri: "pipe:///tmp/snapfifo".into(),
            properties: Default::default(),
        });
        let (d, mut rx) = dispatcher_with(state);
        let mut meta = HashMap::new();
        meta.insert("title".to_string(), serde_json::json!("Song"));
        d.dispatch(ServerCommand::SetStreamMeta {
            stream_id: "default".into(),
            metadata: meta,
        })
        .await;
        {
            let s = d.shared_state.lock().await;
            let st = s.streams.iter().find(|st| st.id == "default").unwrap();
            assert_eq!(st.properties.get("title"), Some(&serde_json::json!("Song")));
        }
        assert!(
            drain(&mut rx)
                .iter()
                .any(|e| matches!(e, ServerEvent::StreamMetaChanged { .. }))
        );
    }

    #[tokio::test]
    async fn set_stream_meta_unknown_stream_still_emits_meta_event_without_state_snapshot() {
        let (d, mut rx) = dispatcher_with(ServerState::default());
        let mut meta = HashMap::new();
        meta.insert("artist".to_string(), serde_json::json!("Unknown"));
        d.dispatch(ServerCommand::SetStreamMeta {
            stream_id: "missing-stream".into(),
            metadata: meta,
        })
        .await;
        let events = drain(&mut rx);
        assert!(
            events
                .iter()
                .any(|e| matches!(e, ServerEvent::StreamMetaChanged { stream_id, .. } if stream_id == "missing-stream"))
        );
        assert!(!events.iter().any(|e| matches!(e, ServerEvent::StateChanged(_))));
    }

    #[tokio::test]
    async fn add_stream_is_rejected_for_embeddable_server() {
        let (d, _rx) = dispatcher_with(ServerState::default());
        let (tx, rx) = tokio::sync::oneshot::channel();
        d.dispatch(ServerCommand::AddStream {
            uri: "pipe:///tmp/x".into(),
            response_tx: tx,
        })
        .await;
        assert!(rx.await.unwrap().is_err());
    }

    #[tokio::test]
    async fn remove_stream_clears_group_reference() {
        let mut state = ServerState::default();
        state.get_or_create_client("c1", "h1", "m1");
        let gid = state.group_for_client("c1", "s1").id.clone();
        state.streams.push(StreamInfo {
            id: "s1".into(),
            status: "playing".into(),
            uri: "pipe:///tmp/x".into(),
            properties: Default::default(),
        });
        let (d, mut rx) = dispatcher_with(state);
        d.dispatch(ServerCommand::RemoveStream {
            stream_id: "s1".into(),
        })
        .await;
        {
            let s = d.shared_state.lock().await;
            assert!(s.streams.iter().all(|st| st.id != "s1"));
            let g = s.groups.iter().find(|g| g.id == gid).unwrap();
            assert!(g.stream_id.is_empty());
        }
        assert!(
            drain(&mut rx)
                .iter()
                .any(|e| matches!(e, ServerEvent::ServerUpdated))
        );
    }

    #[tokio::test]
    async fn remove_unknown_stream_still_emits_structural_update() {
        let (d, mut rx) = dispatcher_with(ServerState::default());
        d.dispatch(ServerCommand::RemoveStream {
            stream_id: "missing".into(),
        })
        .await;
        let events = drain(&mut rx);
        assert!(
            events
                .iter()
                .any(|e| matches!(e, ServerEvent::ServerUpdated))
        );
    }

    #[tokio::test]
    async fn stream_control_is_forwarded_as_event() {
        let (d, mut rx) = dispatcher_with(ServerState::default());
        d.dispatch(ServerCommand::StreamControl {
            stream_id: "s1".into(),
            command: "next".into(),
            params: serde_json::Value::Null,
        })
        .await;
        assert!(drain(&mut rx).iter().any(|e| matches!(
            e,
            ServerEvent::StreamControl { command, .. } if command == "next"
        )));
    }

    #[tokio::test]
    async fn get_status_responds_with_snapshot() {
        let (state, _gid) = state_with_client();
        let (d, _rx) = dispatcher_with(state);
        let (tx, rx) = tokio::sync::oneshot::channel();
        d.dispatch(ServerCommand::GetStatus { response_tx: tx })
            .await;
        let status = rx.await.unwrap();
        assert_eq!(status.server.groups.len(), 1);
    }

    #[tokio::test]
    async fn set_client_volume_event_order_is_state_then_volume_changed() {
        let (state, _gid) = state_with_client();
        let (d, mut rx) = dispatcher_with(state);
        d.dispatch(ServerCommand::SetClientVolume {
            client_id: "c1".into(),
            volume: 5,
            muted: false,
        })
        .await;
        let events = drain(&mut rx);
        let first_state = events
            .iter()
            .position(|e| matches!(e, ServerEvent::StateChanged(_)));
        let first_volume = events
            .iter()
            .position(|e| matches!(e, ServerEvent::ClientVolumeChanged { .. }));
        assert!(first_state.is_some());
        assert!(first_volume.is_some());
        assert!(first_state.unwrap() < first_volume.unwrap());
    }

    #[tokio::test]
    async fn dispatch_survives_closed_event_channel() {
        let (state, _gid) = state_with_client();
        let (d, rx) = dispatcher_with(state);
        drop(rx);
        // Must not panic when event delivery fails.
        d.dispatch(ServerCommand::SetClientName {
            client_id: "c1".into(),
            name: "NoReceiver".into(),
        })
        .await;
    }

    #[cfg(feature = "custom-protocol")]
    #[tokio::test]
    async fn send_to_client_unknown_is_noop() {
        let (d, _rx) = dispatcher_with(ServerState::default());
        // No registered client — must be a graceful no-op, not a panic.
        d.dispatch(ServerCommand::SendToClient {
            client_id: "ghost".into(),
            message: snapcast_proto::CustomMessage::new(9, b"hi"),
        })
        .await;
    }
}
