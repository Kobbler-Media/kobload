use usenet_proto::{IpPreference, UsenetServer};

pub fn engine_server(server: UsenetServer) -> kob_usenet::ServerConfig {
    kob_usenet::ServerConfig {
        id: server.id,
        name: server.name,
        host: server.host,
        port: server.port,
        tls: server.tls,
        verify_cert: server.verify_cert,
        ip_preference: match server.ip_preference {
            IpPreference::Any => kob_usenet::IpPreference::Any,
            IpPreference::V4 => kob_usenet::IpPreference::V4,
            IpPreference::V6 => kob_usenet::IpPreference::V6,
        },
        connections: usize::try_from(server.connections).unwrap_or(usize::MAX),
        priority: server.priority,
        backbone: server.backbone,
        retention_days: server.retention_days,
        expires_on: server.expires_on,
        quota_bytes: server.quota_bytes,
        username: server.username,
        password: server.password,
    }
}
