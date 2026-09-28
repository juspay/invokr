use leptos::prelude::*;
use leptos_meta::*;
use leptos_router::{
    components::{Route, Router, Routes},
    path,
};

use crate::components::sidebar::Sidebar;
use crate::config::DashboardConfig;
use crate::pages::{
    org_detail::OrgDetailPage, organizations::OrganizationsPage,
    workspace_detail::WorkspaceDetailPage,
};

pub fn dashboard_prefix() -> String {
    use_context::<DashboardConfig>()
        .map(|c| c.dashboard_prefix.clone())
        .unwrap_or_default()
}

pub fn prefixed(path: &str) -> String {
    format!("{}{path}", dashboard_prefix())
}

fn pkg_base() -> String {
    use_context::<DashboardConfig>()
        .map(|c| {
            if c.dashboard_prefix.is_empty() {
                "/pkg".to_string()
            } else {
                format!("{}/pkg", c.dashboard_prefix)
            }
        })
        .unwrap_or_else(|| "/pkg".to_string())
}

/// The SSR shell — wraps the App in a full HTML document.
/// Only used during server-side rendering, NOT during hydration.
#[cfg(feature = "ssr")]
pub fn shell(app: impl IntoView) -> impl IntoView {
    let config_script = use_context::<DashboardConfig>()
        .map(|c| {
            format!(
                r#"window.__INVOKR_CONFIG__={{apiBaseUrl:"{}",apiPrefix:"{}",dashboardPrefix:"{}"}};"#,
                c.api_base_url, c.api_prefix, c.dashboard_prefix
            )
        })
        .unwrap_or_default();

    let pkg = pkg_base();
    let wasm_script = format!(
        r#"import init, {{ hydrate }} from '{pkg}/invokr_dashboard.js'; async function main() {{ await init('{pkg}/invokr_dashboard_bg.wasm'); hydrate(); }} main();"#
    );

    view! {
        <!DOCTYPE html>
        <html lang="en">
            <head>
                <meta charset="utf-8" />
                <meta name="viewport" content="width=device-width, initial-scale=1" />
            </head>
            <body>
                <script>{config_script}</script>
                {app}
                <script type="module">{wasm_script}</script>
            </body>
        </html>
    }
}

/// The App component — renders only body content.
/// Shared between SSR and hydration.
#[component]
pub fn App() -> impl IntoView {
    provide_meta_context();

    // During hydration, read config from the injected window.__INVOKR_CONFIG__
    #[cfg(feature = "hydrate")]
    {
        if use_context::<DashboardConfig>().is_none() {
            use wasm_bindgen::JsValue;
            let window = web_sys::window().expect("no global window");
            let config = js_sys::Reflect::get(&window, &JsValue::from_str("__INVOKR_CONFIG__"))
                .unwrap_or(JsValue::UNDEFINED);
            let get = |key: &str| -> String {
                if config.is_undefined() || config.is_null() {
                    return String::new();
                }
                js_sys::Reflect::get(&config, &JsValue::from_str(key))
                    .ok()
                    .and_then(|v| v.as_string())
                    .unwrap_or_default()
            };
            provide_context(DashboardConfig {
                api_base_url: get("apiBaseUrl"),
                api_prefix: get("apiPrefix"),
                dashboard_prefix: get("dashboardPrefix"),
            });
        }
    }

    let base = dashboard_prefix();
    let css_href = format!("{}/tailwind-output.css", pkg_base());

    view! {
        <Stylesheet href=css_href />
        <Title text="Invokr Dashboard" />
        <Router base=base>
            <div class="flex min-h-screen">
                <Sidebar />
                <main class="flex-1 p-8">
                    <Routes fallback=|| "Page not found.">
                        <Route path=path!("/") view=OrganizationsPage />
                        <Route path=path!("/orgs/:org_id") view=OrgDetailPage />
                        <Route path=path!("/orgs/:org_id/workspaces/:workspace_id") view=WorkspaceDetailPage />
                    </Routes>
                </main>
            </div>
        </Router>
    }
}
