//! Re-exported from [`snapcast_proto::status`].

pub use snapcast_proto::status::*;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reexports_are_usable_from_server_status_module() {
        let status = StreamStatus::from("playing");
        assert_eq!(status, StreamStatus::Playing);

        let stream = Stream {
            id: "s1".to_string(),
            status,
            uri: StreamUri {
                raw: "pipe:///tmp/snapfifo".to_string(),
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(stream.id, "s1");
        assert_eq!(stream.status, StreamStatus::Playing);
    }
}
