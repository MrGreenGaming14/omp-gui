//! Remote hosts between the central config and the Tools panel's `loa_view::panel` view.

use loa_settings::app as domain;
use loa_view::panel as view;

/// The Remote hosts entry `alias` as the panel lists it.
pub fn host_to_view(alias: &str, entry: &domain::RemoteHost) -> view::RemoteHost {
    view::RemoteHost {
        alias: alias.to_owned(),
        hostname: entry.host.clone(),
        user: entry.user.clone(),
        port: entry.port,
        proxy_jump: entry.proxy_jump.clone(),
        limits: view::HostLimits {
            allowed_paths: entry.allowed_paths.clone(),
            read_only: entry.read_only,
            commands_allowed: entry.commands,
            auto_approve: entry.auto_approve,
        },
    }
}

/// A host from the panel as its alias and config entry.
pub fn host_from_view(host: view::RemoteHost) -> (String, domain::RemoteHost) {
    let entry = domain::RemoteHost {
        host: host.hostname,
        user: host.user,
        port: host.port,
        proxy_jump: host.proxy_jump,
        allowed_paths: host.limits.allowed_paths,
        read_only: host.limits.read_only,
        commands: host.limits.commands_allowed,
        auto_approve: host.limits.auto_approve,
    };
    (host.alias, entry)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_hosts_round_trip_through_the_view() {
        let entry = domain::RemoteHost {
            host: "build.example.com".into(),
            user: Some("ci".into()),
            port: Some(2222),
            proxy_jump: Some("bastion".into()),
            allowed_paths: vec!["/srv".into(), "/var/log".into()],
            read_only: false,
            commands: true,
            auto_approve: true,
        };
        assert_eq!(host_from_view(host_to_view("build", &entry)), ("build".to_owned(), entry));
    }
}
