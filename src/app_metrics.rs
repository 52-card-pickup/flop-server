use metrics::{gauge, histogram, increment_counter};

pub struct Metrics;

impl Metrics {
    pub fn c_http_requests_total_incr(labels: metrics_labels::HttpRequests) {
        let labels = [
            ("method", labels.method),
            ("path", labels.path),
            ("status", labels.status),
        ];
        increment_counter!("http_requests_total", &labels);
    }

    pub fn h_http_requests_duration_ms(labels: metrics_labels::HttpRequests, duration_ms: f64) {
        let labels = [
            ("method", labels.method),
            ("path", labels.path),
            ("status", labels.status),
        ];
        histogram!("http_requests_duration_ms", duration_ms, &labels);
    }

    pub fn g_rooms_total_set(rooms_total: usize) {
        gauge!("rooms_total", rooms_total as f64);
    }

    pub fn c_room_requests_total_incr(labels: metrics_labels::GameRoom) {
        if let Some(room_code) = labels.room_code {
            let labels = [("room_code", room_code)];
            increment_counter!("room_requests_total", &labels);
        }
    }

    pub fn c_players_total_incr() {
        increment_counter!("players_total");
    }
}

pub mod metrics_labels {
    #[derive(Clone)]
    pub struct HttpRequests {
        pub method: String,
        pub path: String,
        pub status: String,
    }

    pub fn http_requests(method: &str, path: &str, status: u16) -> HttpRequests {
        HttpRequests {
            method: method.to_string(),
            path: path.to_string(),
            status: status.to_string(),
        }
    }

    #[derive(Clone)]
    pub struct GameRoom {
        pub room_code: Option<String>,
    }

    pub fn room_requests(room_code: &str) -> GameRoom {
        GameRoom {
            room_code: Some(room_code.to_string()),
        }
    }

    pub fn room_requests_or_noop(room_code: Option<impl AsRef<str>>) -> GameRoom {
        GameRoom {
            room_code: room_code.map(|s| s.as_ref().to_string()),
        }
    }
}
