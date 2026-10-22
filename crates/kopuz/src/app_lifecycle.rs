use dioxus::prelude::*;

/// The JS runtime the daemon borrows from this process. Desktop runs its own;
/// on Android the only capable runtime is the WebView already drawing the UI,
/// so the daemon's solve requests are drained through `document::eval`. Which
/// source needed a script, and why, stays behind the seam.
#[cfg(target_os = "android")]
pub fn use_webview_script_engine() {
    use_hook(|| {
        let (engine, mut rx) = daemon::script_engine::webview_channel();
        if daemon::script_engine::set_engine(engine).is_err() {
            tracing::warn!("a script engine is already registered; the webview one is not active");
        }
        spawn(async move {
            while let Some(req) = rx.recv().await {
                let wrapped = format!(
                    "globalThis.print=function(s){{dioxus.send(s);}};\
                     try{{{}}}catch(e){{dioxus.send('\\u0000ERR'+(e&&e.stack?e.stack:e));}}",
                    req.program
                );
                let mut eval = dioxus::document::eval(&wrapped);
                let result = match tokio::time::timeout(
                    std::time::Duration::from_secs(20),
                    eval.recv::<String>(),
                )
                .await
                {
                    Ok(Ok(s)) => match s.strip_prefix('\u{0}') {
                        Some(err) => Err(format!("webview JS: {}", err.trim_start_matches("ERR"))),
                        None => Ok(s),
                    },
                    Ok(Err(error)) => Err(format!("webview eval recv: {error}")),
                    Err(_) => Err("webview decipher timed out".to_string()),
                };
                let _ = req.reply.send(result);
            }
        });
    });
}

/// What the network banner says, as the daemon's network status and offline mode have it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NetworkBanner {
    /// Offline mode is on with the internet down.
    Offline,
    /// The internet is there and offline mode, or a move off `source`, waits on someone to switch back.
    SwitchBack { source: String, name: String },
    /// Offline mode just ended.
    BackOnline,
}

/// The banner `status` calls for over these sources; `None` when nothing needs saying.
fn banner_for(status: &api::NetworkStatus, sources: &[api::SourceInfo]) -> Option<NetworkBanner> {
    let offline = status.offline_mode.is_on();
    if status.internet_down {
        return offline.then_some(NetworkBanner::Offline);
    }
    let back_to = match (&status.left_source, offline) {
        (Some(left), _) => sources.iter().find(|source| &source.id == left),
        (None, true) => sources.iter().find(|source| source.active),
        (None, false) => None,
    }?;
    Some(NetworkBanner::SwitchBack {
        source: back_to.id.clone(),
        name: back_to.name.clone(),
    })
}

/// The app's offline flag, which the pages' downloads-only views read, and its banner, both fed by the daemon rather than a probe of the app's own.
pub fn use_offline_mode(mut network_banner: Signal<Option<NetworkBanner>>) -> Signal<bool> {
    let status = hooks::sources::use_network_status();
    let sources = hooks::sources::use_sources();
    let mut is_offline = use_signal(|| false);
    use_context_provider(|| is_offline);
    use_effect(move || {
        let status = status.read().clone();
        let offline = status.offline_mode.is_on();
        if offline != *is_offline.peek() {
            tracing::info!(offline, "offline mode changed");
            is_offline.set(offline);
        }
        let next = banner_for(&status, sources.read().as_deref().unwrap_or_default());
        let shown = network_banner.peek().clone();
        match next {
            Some(banner) if shown.as_ref() != Some(&banner) => network_banner.set(Some(banner)),
            Some(_) => {}
            None if matches!(
                shown,
                Some(NetworkBanner::Offline | NetworkBanner::SwitchBack { .. })
            ) =>
            {
                network_banner.set(Some(NetworkBanner::BackOnline));
                spawn(async move {
                    utils::sleep(std::time::Duration::from_secs(4)).await;
                    if network_banner.read().as_ref() == Some(&NetworkBanner::BackOnline) {
                        network_banner.set(None);
                    }
                });
            }
            None => {}
        }
    });
    is_offline
}

/// Leave offline mode, then go back to `source` when the daemon had moved off it.
pub fn switch_back(source: Option<String>) {
    spawn(async move {
        let api = hooks::api::consume_api();
        let left = match api.set_offline(false).await {
            Ok(status) => status.left_source,
            Err(error) => {
                hooks::toast::toast_api_error(&error);
                return;
            }
        };
        if let Some(source) = source.filter(|source| left.as_ref() == Some(source)) {
            hooks::source_switch::apply_source_switch(source).await;
        }
    });
}

pub fn banner_text(banner: &NetworkBanner) -> String {
    match banner {
        NetworkBanner::Offline => i18n::t("network_offline_banner"),
        NetworkBanner::SwitchBack { name, .. } => {
            i18n::t_with("back_online_switch_to", &[("source", name.clone().into())])
        }
        NetworkBanner::BackOnline => i18n::t("network_online_banner"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(id: &str, active: bool) -> api::SourceInfo {
        api::SourceInfo {
            id: id.into(),
            name: format!("{id} name"),
            active,
            ..Default::default()
        }
    }

    fn status(
        internet_down: bool,
        left: Option<&str>,
        mode: api::OfflineMode,
    ) -> api::NetworkStatus {
        api::NetworkStatus {
            internet_down,
            left_source: left.map(str::to_string),
            offline_mode: mode,
        }
    }

    #[test]
    fn the_banner_follows_the_daemon() {
        use api::OfflineMode::*;
        let sources = [source("folder", false), source("yt", true)];
        let switch_back = |id: &str| {
            Some(NetworkBanner::SwitchBack {
                source: id.into(),
                name: format!("{id} name"),
            })
        };

        assert_eq!(banner_for(&status(false, None, Off), &sources), None);
        assert_eq!(
            banner_for(&status(true, None, Automatic), &sources),
            Some(NetworkBanner::Offline)
        );
        assert_eq!(
            banner_for(&status(true, Some("yt"), Off), &sources),
            None,
            "a move to a folder library while it is down needs no banner"
        );
        assert_eq!(
            banner_for(&status(false, None, Automatic), &sources),
            switch_back("yt"),
            "back online, offline mode waits on a yes"
        );
        assert_eq!(
            banner_for(&status(false, None, Requested), &sources),
            switch_back("yt")
        );
        let moved = [source("folder", true), source("yt", false)];
        assert_eq!(
            banner_for(&status(false, Some("yt"), Off), &moved),
            switch_back("yt")
        );
    }
}
