//! Port forwards: what is reachable from this machine, and a form to add more.
//!
//! The list shows the forwards of the open cluster with live traffic. A new one
//! starts from a row's menu, which fills the form with the target and the ports
//! it exposes.

use std::{sync::Arc, time::Duration};

use gpui_kit::{
    assets::IconName,
    base::StyledExt,
    component::{
        ActiveTheme, h_flex,
        input::{Input, InputState},
        v_flex,
    },
    prelude::FluentBuilder,
    *,
};
use k8s_app::{AppCore, forward_ops};
use k8s_ops::forward::{ForwardSpec, ForwardStatus, PortOption};
use tokio::runtime::Handle;

use crate::{
    drawer::Target,
    ui::{
        Emphasis, Palette, button, button_icon, chip, chip_icon, message, page_title, pill,
        toast_success,
    },
};

/// Traffic counters change constantly; a couple of seconds keeps them lively
/// without redrawing for nothing.
const POLL: Duration = Duration::from_secs(2);

/// A port as typed. Blank is `None` when the field is optional.
fn parse_port(text: &str, required: bool) -> Result<Option<u16>, &'static str> {
    let text = text.trim();
    if text.is_empty() {
        return if required {
            Err("Enter the port to forward to.")
        } else {
            Ok(None)
        };
    }
    match text.parse::<u16>() {
        Ok(0) | Err(_) => Err("A port is a number from 1 to 65535."),
        Ok(port) => Ok(Some(port)),
    }
}

/// Bytes the way a person counts them.
fn bytes(count: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut value = count as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{count} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// `pods/web-1` as `pod web-1`, from the `group/version/plural` of a target.
fn describe(resource: &str, name: &str) -> String {
    let kind = match resource.rsplit('/').next().unwrap_or(resource) {
        "pods" => "pod",
        "services" => "service",
        "deployments" => "deployment",
        "statefulsets" => "statefulset",
        "daemonsets" => "daemonset",
        other => other,
    };
    format!("{kind} {name}")
}

/// The form for a forward being set up.
struct Draft {
    target: Target,
    ports: Vec<PortOption>,
    remote: Entity<InputState>,
    local: Entity<InputState>,
    expose: bool,
    error: Option<SharedString>,
    busy: bool,
}

pub struct PortsView {
    core: Arc<AppCore>,
    runtime: Handle,
    cluster: SharedString,
    forwards: Vec<ForwardStatus>,
    draft: Option<Draft>,
}

impl PortsView {
    pub fn new(
        core: &Arc<AppCore>,
        runtime: &Handle,
        cluster: SharedString,
        draft: Option<Target>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut view = Self {
            core: Arc::clone(core),
            runtime: runtime.clone(),
            forwards: forward_ops::list(core, &cluster),
            cluster,
            draft: None,
        };
        if let Some(target) = draft {
            view.begin(target, window, cx);
        }
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(POLL).await;
                let alive = this.update(cx, |this, cx| {
                    this.forwards = forward_ops::list(&this.core, &this.cluster);
                    cx.notify();
                });
                if alive.is_err() {
                    break;
                }
            }
        })
        .detach();
        view
    }

    /// Open the form for a target, asking the cluster which ports it exposes.
    pub fn begin(&mut self, target: Target, window: &mut Window, cx: &mut Context<Self>) {
        let remote = cx.new(|cx| InputState::new(window, cx).placeholder("e.g. 80"));
        let local = cx.new(|cx| InputState::new(window, cx).placeholder("Automatic"));
        let Some(namespace) = target.namespace.clone() else {
            return;
        };
        let (core, cluster, resource, name) = (
            Arc::clone(&self.core),
            self.cluster.clone(),
            target.resource.clone(),
            target.name.clone(),
        );
        let task = self.runtime.spawn(async move {
            forward_ops::ports(&core, &cluster, &resource, &namespace, &name).await
        });
        self.draft = Some(Draft {
            target,
            ports: Vec::new(),
            remote,
            local,
            expose: false,
            error: None,
            busy: false,
        });
        cx.spawn_in(window, async move |this, cx| {
            let ports = task.await.ok().and_then(Result::ok).unwrap_or_default();
            this.update_in(cx, |this, window, cx| {
                if let Some(draft) = &mut this.draft {
                    // Suggest the first port, so the common case is one click.
                    if let Some(first) = ports.first() {
                        draft.remote.update(cx, |input, cx| {
                            input.set_value(first.port.to_string(), window, cx)
                        });
                    }
                    draft.ports = ports;
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn start(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(draft) = &mut self.draft else {
            return;
        };
        let parsed = parse_port(&draft.remote.read(cx).value(), true)
            .and_then(|remote| Ok((remote, parse_port(&draft.local.read(cx).value(), false)?)));
        let (remote, local) = match parsed {
            Ok((Some(remote), local)) => (remote, local),
            Ok((None, _)) => return,
            Err(reason) => {
                draft.error = Some(reason.into());
                cx.notify();
                return;
            }
        };
        let Some(namespace) = draft.target.namespace.as_ref().map(ToString::to_string) else {
            return;
        };
        draft.error = None;
        draft.busy = true;
        let spec = ForwardSpec {
            namespace,
            resource: draft.target.resource.to_string(),
            name: draft.target.name.to_string(),
            remote_port: remote,
            local_port: local,
            expose_on_network: draft.expose,
        };
        let label = describe(&spec.resource, &spec.name);
        let (core, cluster) = (Arc::clone(&self.core), self.cluster.clone());
        let task = self
            .runtime
            .spawn(async move { forward_ops::start(&core, &cluster, spec).await });
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await.unwrap_or_else(|err| Err(err.to_string()));
            this.update_in(cx, |this, window, cx| {
                match result {
                    Ok(status) => {
                        this.draft = None;
                        this.forwards = forward_ops::list(&this.core, &this.cluster);
                        toast_success(
                            window,
                            cx,
                            format!(
                                "{}:{} reaches {label}",
                                status.local_address, status.local_port
                            ),
                        );
                    }
                    Err(reason) => {
                        if let Some(draft) = &mut this.draft {
                            draft.busy = false;
                            draft.error = Some(reason.into());
                        }
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn stop(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        forward_ops::stop(&self.core, id);
        self.forwards.retain(|forward| forward.id != id);
        toast_success(window, cx, "Forward stopped");
        cx.notify();
    }

    fn render_form(&self, draft: &Draft, cx: &mut Context<Self>) -> impl IntoElement {
        let (background, border, muted, danger, warning) = {
            let theme = cx.theme();
            (
                theme.background,
                theme.border,
                theme.muted_foreground,
                theme.danger,
                theme.warning,
            )
        };
        let label = |text: &'static str| div().text_xs().text_color(muted).child(text);
        let start = cx.listener(|this, _, window, cx| this.start(window, cx));
        let cancel = cx.listener(|this, _, _, cx| {
            this.draft = None;
            cx.notify();
        });
        let expose = cx.listener(|this, _, _, cx| {
            if let Some(draft) = &mut this.draft {
                draft.expose = !draft.expose;
            }
            cx.notify();
        });
        let remote = draft.remote.clone();
        let suggestions = draft.ports.iter().enumerate().map(|(index, option)| {
            let port = option.port;
            let remote = remote.clone();
            let text = match &option.name {
                Some(name) => format!("{port} · {name}"),
                None => port.to_string(),
            };
            chip(("ports-suggest", index), text, true, false, cx.theme()).on_click(cx.listener(
                move |_, _, window, cx| {
                    remote.update(cx, |input, cx| {
                        input.set_value(port.to_string(), window, cx)
                    });
                },
            ))
        });

        v_flex()
            .w(px(420.))
            .h_full()
            .flex_none()
            .bg(background)
            .border_l_1()
            .border_color(border)
            .child(
                v_flex()
                    .flex_none()
                    .px_4()
                    .py_3()
                    .gap_1()
                    .border_b_1()
                    .border_color(border)
                    .child(div().font_semibold().child("Forward a port"))
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .child(describe(&draft.target.resource, &draft.target.name)),
                    ),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .p_4()
                    .gap_3()
                    .child(label("Port on the target"))
                    .child(Input::new(&draft.remote))
                    .when(!draft.ports.is_empty(), |column| {
                        column.child(h_flex().gap_2().flex_wrap().children(suggestions))
                    })
                    .child(label("Port on this machine"))
                    .child(Input::new(&draft.local))
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .child("Leave it blank to pick a free port."),
                    )
                    .child(
                        chip(
                            "ports-expose",
                            "Let other computers connect",
                            true,
                            draft.expose,
                            cx.theme(),
                        )
                        .on_click(expose),
                    )
                    .when(draft.expose, |column| {
                        column.child(div().text_xs().text_color(warning).child(
                            "Anyone on your network can then reach this service, with no \
                             login of Kubernetes' own in front of it.",
                        ))
                    })
                    .when_some(draft.error.clone(), |column, error| {
                        column.child(div().text_sm().text_color(danger).child(error))
                    }),
            )
            .child(
                h_flex()
                    .flex_none()
                    .px_4()
                    .py_3()
                    .gap_2()
                    .items_center()
                    .border_t_1()
                    .border_color(border)
                    .child(
                        button_icon(
                            "ports-start",
                            IconName::ArrowLeftRight,
                            "Start forwarding",
                            !draft.busy,
                            Emphasis::Primary,
                            cx.theme(),
                        )
                        .when(!draft.busy, |b| b.on_click(start)),
                    )
                    .child(
                        button("ports-cancel", "Cancel", true, Emphasis::Plain, cx.theme())
                            .on_click(cancel),
                    )
                    .when(draft.busy, |row| {
                        row.child(div().text_xs().text_color(muted).child("Starting…"))
                    }),
            )
    }

    fn render_row(
        &self,
        index: usize,
        forward: &ForwardStatus,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let (border, muted, danger, success) = {
            let theme = cx.theme();
            (
                theme.border,
                theme.muted_foreground,
                theme.danger,
                theme.success,
            )
        };
        let address = format!("{}:{}", forward.local_address, forward.local_port);
        let url = format!("http://localhost:{}", forward.local_port);
        let id = forward.id;
        let copy_text = address.clone();
        let copy = cx.listener(move |_, _, window, cx| {
            cx.write_to_clipboard(ClipboardItem::new_string(copy_text.clone()));
            toast_success(window, cx, format!("Copied {copy_text}"));
        });
        let open = cx.listener(move |_, _, _, cx| cx.open_url(&url));
        let stop = cx.listener(move |this, _, window, cx| this.stop(id, window, cx));
        let exposed = forward.local_address != "127.0.0.1";

        v_flex()
            .px_4()
            .py_3()
            .gap_1()
            .when(index > 0, |row| row.border_t_1().border_color(border))
            .child(
                h_flex()
                    .gap_3()
                    .items_center()
                    .child(div().size(px(8.)).flex_none().rounded_full().bg(success))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .font_semibold()
                            .child(describe(&forward.resource, &forward.name)),
                    )
                    .when(exposed, |row| {
                        row.child(pill("On the network", cx.theme().warning))
                    })
                    .child(
                        chip_icon(
                            ("ports-open", index),
                            IconName::ExternalLink,
                            "Open",
                            true,
                            false,
                            cx.theme(),
                        )
                        .on_click(open),
                    )
                    .child(
                        chip_icon(
                            ("ports-copy", index),
                            IconName::Copy,
                            "Copy",
                            true,
                            false,
                            cx.theme(),
                        )
                        .on_click(copy),
                    )
                    .child(
                        chip_icon(
                            ("ports-stop", index),
                            IconName::Square,
                            "Stop",
                            true,
                            false,
                            cx.theme(),
                        )
                        .on_click(stop),
                    ),
            )
            .child(div().text_sm().text_color(muted).child(format!(
                "{address} → {}:{} · {} · {} sent · {} received",
                forward.namespace,
                forward.remote_port,
                match forward.active_connections {
                    1 => "1 connection".to_string(),
                    n => format!("{n} connections"),
                },
                bytes(forward.bytes_sent),
                bytes(forward.bytes_received),
            )))
            .when_some(forward.last_error.clone(), |column, error| {
                column.child(div().text_xs().text_color(danger).child(error))
            })
    }
}

impl Render for PortsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = Palette::of(cx);
        let form = self
            .draft
            .as_ref()
            .map(|draft| self.render_form(draft, cx).into_any_element());
        let rows = self
            .forwards
            .iter()
            .enumerate()
            .map(|(index, forward)| self.render_row(index, forward, cx).into_any_element())
            .collect::<Vec<_>>();

        div()
            .flex()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .child(
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .child(
                        h_flex()
                            .h(px(48.))
                            .flex_none()
                            .px_4()
                            .gap_3()
                            .items_center()
                            .child(page_title(
                                IconName::ArrowLeftRight,
                                "Port forwards",
                                cx.theme(),
                            ))
                            .when(!rows.is_empty(), |row| {
                                row.child(pill(format!("{} active", rows.len()), palette.muted))
                            }),
                    )
                    .child(if rows.is_empty() {
                        message(
                            "No port forwards. Right-click a pod, service or workload and choose \
                             Forward port…",
                            palette.muted,
                        )
                        .into_any_element()
                    } else {
                        div()
                            .id("ports-list")
                            .flex_1()
                            .min_h_0()
                            .overflow_y_scroll()
                            .children(rows)
                            .into_any_element()
                    }),
            )
            .children(form)
    }
}

#[cfg(test)]
mod tests {
    use super::{bytes, describe, parse_port};

    #[test]
    fn ports_are_numbers_from_one_to_65535_and_blank_means_automatic() {
        assert_eq!(parse_port(" 8080 ", true), Ok(Some(8080)));
        assert_eq!(parse_port("", false), Ok(None));
        assert!(parse_port("", true).is_err());
        for bad in ["0", "65536", "-1", "http", "80.5"] {
            assert!(parse_port(bad, false).is_err(), "{bad}");
        }
        assert_eq!(parse_port("65535", true), Ok(Some(65535)));
    }

    #[test]
    fn byte_counts_read_like_a_file_manager() {
        assert_eq!(bytes(0), "0 B");
        assert_eq!(bytes(1023), "1023 B");
        assert_eq!(bytes(1536), "1.5 KB");
        assert_eq!(bytes(5 * 1024 * 1024), "5.0 MB");
    }

    #[test]
    fn a_target_is_described_by_its_kind_and_name() {
        assert_eq!(describe("core/v1/pods", "web-1"), "pod web-1");
        assert_eq!(describe("apps/v1/deployments", "web"), "deployment web");
        assert_eq!(describe("x/v1/widgets", "w"), "widgets w");
    }
}
